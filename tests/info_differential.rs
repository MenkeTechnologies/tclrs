//! Differential execution for the `info` ensemble.
//!
//! Same rule as the other differential suites: no expected value is written by
//! hand. Every program is run by tclsh and by tclrs and the two outputs compared
//! byte for byte, so a misreading of what `info exists` counts as existing, of
//! where `info complete` draws the line, or of how a subcommand refuses, fails
//! here instead of becoming a baked-in bug.
//!
//! What this suite must not ask, because the answer is the machine's rather than
//! the language's:
//!
//! * `info hostname`, `info nameofexecutable`, `info script` — the host, the
//!   binary being run, and this checkout's layout. Programs below assert only
//!   their *shape* (non-empty, absolute) where they mention them at all.
//! * `info library`, and therefore an unfiltered `info globals` — tclsh has a
//!   script library and the `auto_path` that `init.tcl` sets from it; tclrs has
//!   neither. Every `info globals` / `info vars` program filters by a pattern
//!   that only the program's own variables can match, and the library is asked
//!   about only after `tcl_library` is gone, which is the state tclrs is
//!   permanently in. BUGS.md records the difference.
//! * An unfiltered `info commands`, whose contents are the two interpreters'
//!   command sets and are not equal. Membership of a single named command is
//!   asked instead, which is what a script actually does with it.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const PROGRAMS: &[&str] = &[
    // ── info exists ──
    // The question must not be able to create its own answer: asking about an
    // unset variable leaves it unset, so a second ask agrees with the first.
    "puts [info exists nope]",
    "puts [info exists nope]\nputs [info exists nope]",
    "set a 1\nputs [info exists a]",
    "set a {}\nputs [info exists a]",
    "set a 1\nunset a\nputs [info exists a]",
    "set a 1\nunset a\nputs [info exists a]\nset a 2\nputs [info exists a]\nputs $a",
    // Elements and arrays.
    "set a(k) v\nputs [info exists a(k)]\nputs [info exists a(z)]\nputs [info exists a]",
    "set a(k) v\nunset a(k)\nputs [info exists a(k)]\nputs [info exists a]",
    "array set a {}\nputs [info exists a]\nputs [info exists a(k)]",
    // A scalar asked about by element, and an array asked about as a scalar.
    "set s plain\nputs [info exists s(k)]",
    "set i k\nset a(k) v\nputs [info exists a($i)]",
    // Inside a procedure: parameters exist, the caller's variables do not, and
    // `global` brings one into view.
    "proc p {x} {return [info exists x]}\nputs [p 1]",
    "proc p {} {return [info exists nothinghere]}\nputs [p]",
    "set outer 1\nproc p {} {return [info exists outer]}\nputs [p]",
    "set outer 1\nproc p {} {global outer\nreturn [info exists outer]}\nputs [p]",
    "proc p {} {set local 1\nreturn [info exists local]}\nputs [p]",
    "proc p {a {b 2}} {return [list [info exists a] [info exists b]]}\nputs [p 1]",
    "proc p {} {set arr(x) 1\nreturn [list [info exists arr] [info exists arr(x)] [info exists arr(y)]]}\nputs [p]",
    // Asking does not create it in a frame either.
    "proc p {} {set r [info exists v]\nset v 9\nreturn [list $r [info exists v]]}\nputs [p]",
    // ── info complete ──
    "puts [info complete {}]",
    "puts [info complete {puts hi}]",
    "puts [info complete \"set x 1\\nset y 2\"]",
    "puts [info complete \"{\"]",
    "puts [info complete \"\\}\"]",
    "puts [info complete {[}]",
    "puts [info complete {puts [expr 1}]",
    "puts [info complete {puts [expr {1}]}]",
    "puts [info complete {set x \"abc}]",
    "puts [info complete {set x \"abc\"}]",
    "puts [info complete {if {1} {}]",
    "puts [info complete {if {1} {} }]",
    "puts [info complete {proc p {} {}}]",
    // Closed but malformed is complete: nothing more could be typed to fix it.
    "puts [info complete {set x \"a\"b}]",
    "puts [info complete \"puts \\}extra\"]",
    "puts [info complete {{a}x}]",
    // A trailing backslash continues a line but does not leave the command open.
    "puts [info complete \"puts a\\\\\"]",
    "puts [info complete {#}]",
    "puts [info complete \"# comment\\n\"]",
    // Building input a character at a time is what the command exists for.
    "set src {}\nforeach c [split {puts [list a b]} {}] {append src $c\nputs -nonewline [info complete $src]}\nputs {}",
    // ── info args / info default ──
    "proc p {a b} {}\nputs [info args p]",
    "proc p {} {}\nputs [info args p]x",
    "proc p {a {b 2} args} {}\nputs [info args p]",
    "proc p {args} {}\nputs [info args p]",
    "proc p {{a 1}} {}\nputs [info args p]",
    // A name computed at run time, which is why this is answered from a table
    // rather than from the call site.
    "proc p {x y} {}\nset n p\nputs [info args $n]",
    "proc p {a {b hello}} {}\nputs [info default p a v]\nputs [info default p b w]\nputs $w",
    "proc p {a {b {}}} {}\nputs [info default p b v]\nputs \"<$v>\"",
    "proc p {{a { x y }}} {}\nputs [info default p a v]\nputs \"<$v>\"",
    "proc p {{a 1}} {}\nset v preset\nputs [info default p a v]\nputs $v",
    "proc p {a} {}\nset v preset\nputs [info default p a v]\nputs \"<$v>\"",
    "proc p {{a 1}} {}\nputs [info default p a arr(k)]\nputs $arr(k)",
    // ── info procs ──
    "proc p {} {}\nputs [info procs p]",
    "puts [info procs nosuchproc]x",
    "proc zzx {} {}\nproc zzy {} {}\nproc b {} {}\nputs [lsort [info procs zz*]]",
    "proc p {} {}\nputs [expr {[lsearch -exact [info procs] p] >= 0}]",
    "puts [info procs puts]x",
    // ── info commands ──
    "proc mine {} {}\nputs [info commands mine]",
    "puts [expr {[lsearch -exact [info commands] puts] >= 0}]",
    "puts [expr {[lsearch -exact [info commands] set] >= 0}]",
    "puts [expr {[lsearch -exact [info commands] nosuchcommandanywhere] >= 0}]",
    "puts [info commands nosuchcommandanywhere]x",
    // ── info globals / info vars ──
    // Filtered by a prefix only this program can produce, so neither
    // interpreter's own globals can enter the answer.
    "set zzalpha 1\nset zzbeta 2\nputs [lsort [info globals zz*]]",
    "puts [info globals zz*]x",
    "set zzx 1\nunset zzx\nputs [info globals zz*]x",
    "set zza(k) 1\nputs [info globals zz*]",
    "set zzalpha 1\nputs [lsort [info vars zz*]]",
    "proc p {} {global zzg\nreturn [info globals zz*]}\nset zzg 1\nputs [p]",
    // A pattern that names a namespace is matched and answered in fully
    // qualified form, whichever spelling it was written in; one with no
    // separator answers the bare names.
    "namespace eval zzn {variable x 1; variable y 2}\nputs [lsort [info vars zzn::*]]",
    "namespace eval zzn {variable x 1}\nputs [lsort [info vars ::zzn::*]]",
    "namespace eval zzn {variable x 1}\nputs [info vars ::zzn::x]",
    "namespace eval zzn {proc f {} {}; proc g {} {}}\nputs [lsort [info procs zzn::*]]",
    "namespace eval zzn {proc f {} {}}\nputs [lsort [info procs ::zzn::*]]",
    "namespace eval zzn {proc f {} {}}\nputs [lsort [info commands ::zzn::f]]",
    "namespace eval zzn {proc f {} {}}\nputs [info procs ::zzn::nosuch]x",
    "puts [info vars ::zznosuch::*]x",
    "set zzalpha 1\nputs [info vars ::zzalpha]",
    // ── versions ──
    "puts [info tclversion]",
    // `info patchlevel` is deliberately NOT compared: it is the release each
    // interpreter IS, not behaviour they can agree on. tclrs answers with the
    // Tcl it is written against (9.0.4), and a reference from any other 9.0.x
    // answers with its own — a difference that says nothing about the port.
    // `tests/version_pin.rs` pins what tclrs reports.
    // ── abbreviation, which tclsh resolves for any unique prefix ──
    "set a 1\nputs [info ex a]",
    "puts [info comp {puts hi}]",
    "proc p {a} {}\nputs [info ar p]",
    "puts [info tclv]",
    // ── the platform, which both engines are asked about on the same one ──
    "puts [info sharedlibextension]",
    // The one-argument form sets what later asks report, so a program can make
    // this answer its own rather than the file's.
    "puts [info script /tmp/tclrs-info-fixed.tcl]\nputs [info script]",
    "set old [info script /tmp/tclrs-info-fixed.tcl]\nputs [string equal $old [info script]]",
    // ── shape of the machine-dependent answers, never their content ──
    "puts [expr {[string length [info hostname]] > 0}]",
    "puts [string match /* [info nameofexecutable]]",
];

/// Failures: the message tclsh produces is the specification.
const ERRORS: &[&str] = &[
    // No subcommand, and one that neither implements.
    "info",
    "info nosuchsubcommand",
    "info e",
    "info c",
    // Wrong argument counts.
    "info exists",
    "info exists a b",
    "info complete",
    "info complete a b",
    "info tclversion x",
    "info patchlevel x",
    "info args",
    "info args a b",
    "info default p",
    "info default p a",
    "info default p a b c",
    "info procs a b",
    "info globals a b",
    "info hostname x",
    "info script a b",
    "info sharedlibextension x",
    // Arguments that name nothing.
    "info args nosuchproc",
    "proc p {} {}\ninfo args p extra",
    "info default nosuchproc a v",
    "proc p {a} {}\ninfo default p nosucharg v",
    // A procedure's name is not a command's name.
    "info args puts",
    // Failure inside catch still has to say the same thing.
    "puts [catch {info args nosuchproc} m]\nputs $m",
    "proc p {a} {}\nputs [catch {info default p zz v} m]\nputs $m",
    "puts [catch {info nosuchsubcommand} m]\nputs $m",
    "puts [catch {info exists} m]\nputs $m",
    // The library, asked about in the state tclrs is permanently in: tclsh
    // raises this too once `tcl_library` is gone. tclrs has no such variable to
    // remove, so removing it is allowed to fail.
    "catch {unset ::tcl_library}\nputs [catch {info library} m]\nputs $m",
];

fn tclsh() -> Option<PathBuf> {
    for name in ["tclsh9.0", "tclsh", "tclsh8.6"] {
        let Ok(out) = Command::new("sh")
            .arg("-c")
            .arg(format!("command -v {name}"))
            .output()
        else {
            continue;
        };
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if path.is_empty() {
            continue;
        }
        // Only the exact release this port is written against is an oracle.
        // tclrs targets 9.0.4 (`src/cmd_info.rs`'s `TCL_PATCHLEVEL`), and a
        // reference from any other release reports ITS version's differences
        // as tclrs failures: 8.6 words errors differently ("couldn't compile
        // regular expression" for "cannot compile") and has a different
        // ensemble membership, while 9.0.3 predates the lseq fixes (a zero
        // step yields the empty list where the manual says it yields `count`
        // elements, and a bareword argument is still an expr). The ubuntu CI
        // image ships 8.6, so CI skips these and they run against a matching
        // tclsh locally.
        let Ok(v) = Command::new("sh")
            .arg("-c")
            .arg(format!("printf 'puts [info patchlevel]\\n' | {path}"))
            .output()
        else {
            continue;
        };
        if String::from_utf8_lossy(&v.stdout).trim() == "9.0.4" {
            return Some(PathBuf::from(path));
        }
    }
    None
}

/// What tclsh did: its stdout when it succeeded, its message when it failed.
/// Both are compared, because a command that refuses has to refuse the same way.
fn reference(tclsh: &PathBuf, program: &str) -> Result<String, String> {
    // The test functions run in parallel, so the scratch file is unique per call
    // rather than per process.
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "tclrs-info-{}-{}.tcl",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path, program).expect("write program");
    let out = Command::new(tclsh).arg(&path).output().expect("run tclsh");
    let _ = std::fs::remove_file(&path);
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

fn compare(tclsh: &PathBuf, programs: &[&str]) {
    let mut failures = Vec::new();
    for program in programs {
        match (reference(tclsh, program), tclrs::eval(program)) {
            (Ok(expected), Ok(got)) if got.output == expected => {}
            (Ok(expected), Ok(got)) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs: {:?}",
                got.output
            )),
            (Ok(expected), Err(e)) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs failed: {e}"
            )),
            (Err(expected), Err(got))
                if got
                    .to_string()
                    .starts_with(expected.lines().next().unwrap_or_default()) => {}
            (Err(expected), Err(got)) => failures.push(format!(
                "program:\n{program}\n  tclsh error: {expected:?}\n  tclrs error: {got:?}"
            )),
            (Err(expected), Ok(got)) => failures.push(format!(
                "program:\n{program}\n  tclsh error: {expected:?}\n  tclrs accepted it: {:?}",
                got.output
            )),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} programs diverge:\n\n{}",
        failures.len(),
        programs.len(),
        failures.join("\n\n")
    );
}

#[test]
fn info_matches_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    compare(&tclsh, PROGRAMS);
}

#[test]
fn info_errors_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    compare(&tclsh, ERRORS);
}

/// Every subcommand tclsh has is either answered or refused **by name**, and a
/// refusal names the subcommand it refused. Silently accepting one and returning
/// something wrong is the failure this prevents; so is refusing with a message
/// that does not say what was refused.
///
/// The list is tclsh's, read from tclsh — asking it to resolve a prefix no
/// subcommand can match makes it print all of them.
#[test]
fn every_subcommand_is_answered_or_refused_by_name() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    let listing = match reference(&tclsh, "info \u{7f}nosuch") {
        Err(msg) => msg,
        Ok(out) => panic!("tclsh accepted a nonexistent info subcommand: {out:?}"),
    };
    let first = listing.lines().next().unwrap_or_default();
    let listed = first
        .split_once("must be ")
        .map(|(_, rest)| rest)
        .unwrap_or_default();
    let names: Vec<String> = listed
        .split(", ")
        .map(|w| w.trim().trim_start_matches("or ").to_string())
        .filter(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_lowercase()))
        .collect();
    assert!(
        names.len() > 20,
        "could not read tclsh's subcommand list from {listing:?}, got {names:?}"
    );

    let mut problems = Vec::new();
    for name in &names {
        // Called with no arguments: enough to reach the subcommand's own
        // dispatch, which is all this test is about.
        let program = format!("info {name}");
        match tclrs::eval(&program) {
            // Answered, or refused for a reason of its own (wrong arity, a
            // missing procedure). Either way tclrs did not silently ignore it.
            Ok(_) => {}
            Err(e) => {
                let msg = e.to_string();
                let refused_generically = msg.contains("unknown or ambiguous subcommand");
                if refused_generically {
                    problems.push(format!(
                        "info {name}: tclsh has it, tclrs does not know the name at all: {msg}"
                    ));
                } else if !msg.contains(name.as_str()) {
                    problems.push(format!(
                        "info {name}: refused without naming the subcommand: {msg}"
                    ));
                }
            }
        }
    }
    assert!(
        problems.is_empty(),
        "{} of {} of tclsh's info subcommands are mishandled:\n{}",
        problems.len(),
        names.len(),
        problems.join("\n")
    );
}

/// A refusal for something tclsh *supports* is catchable, and silent where the
/// script never reaches it.
///
/// This is the shape of every "not supported yet" refusal, and it has to hang on
/// a subcommand tclsh answers, so that tclsh never raises anything. `info locals`
/// was that subcommand until it was implemented — the compiler bakes the frame's
/// candidate names in and the run keeps the ones that are set — so `info frame`
/// carries it now: tclsh answers a description of a call frame and nothing here
/// records the stack of *commands* that would be needed to.
/// While the refusal was a compile-time verdict, `catch {info locals}` killed
/// the whole script and `if {0} {info locals}` refused a branch tclsh runs — a
/// script was punished for *mentioning* a construct. Reporting earlier than
/// tclsh is no service when tclsh's answer is to work.
///
/// The dead-branch half is a real differential assertion: both engines print
/// `survived` and nothing else. The `catch` half cannot be — tclsh succeeds
/// there and tclrs still lacks the subcommand — so it pins that the refusal is
/// *reachable by a script* rather than fatal to it, which is the part that
/// changed. Whoever implements `info frame` should swap in whatever is refused
/// then rather than delete the test.
#[test]
fn a_refusal_for_something_tclsh_has_is_catchable_and_skippable() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };

    // Never executed: both engines run the script to completion.
    compare(&tclsh, &["if {0} {info frame}\nputs survived"]);
    compare(
        &tclsh,
        &["proc p {} {if {0} {info frame}; return ok}\nputs [p]"],
    );

    // Executed: catchable here, where it used to end the program.
    let out =
        tclrs::eval("puts [catch {info frame} e]\nputs [string match {*not supported yet*} $e]")
            .expect("the script runs to completion");
    assert_eq!(
        out.output, "1\n1\n",
        "the refusal should reach the script's own catch: {:?}",
        out.output
    );
}
