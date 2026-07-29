//! Differential execution for arrays inside a procedure.
//!
//! Same contract as the other differential suites: every program is run by both
//! tclsh and tclrs and the outputs are compared byte for byte, so nothing here
//! is an expectation written by hand.
//!
//! An array is a property of a *variable*, and a procedure's variables are frame
//! slots rather than entries in the VM's global table — so a local array had to
//! be refused until the ops could reach either home. What that refusal was
//! hiding, and what these programs pin, is that the two homes differ in ways a
//! script can see:
//!
//! * A local array belongs to its *activation*. Two calls do not accumulate,
//!   and two frames of a recursive procedure hold different elements — where a
//!   global-table array would have shared one map between them.
//! * A local array and a global one of the same name are different variables,
//!   and `global` is what chooses between them.
//! * `unset` of a local removes it, which the frame slot could not express
//!   before.
//! * The refusal wording for `array set` over an existing scalar follows the
//!   *enclosing body*, not the variable: inside a procedure tclsh names the
//!   variable, at the top level it names the first element it was about to
//!   write. A global reached through `global` from inside a body takes the
//!   body's wording, which is what says the scope decides.
//!
//! Each program is complete on its own, for the reason
//! `list_commands_differential.rs` gives: a shape this frontend refuses would
//! otherwise take the rest of a combined file down with it.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Programs whose output must agree, byte for byte.
const PROGRAMS: &[&str] = &[
    // ── a local array is local ───────────────────────────────────────────
    "proc p {} {array set a {x 1}; set a(y) 2; return [lsort [array get a]]}\nputs [p]\nputs [p]",
    "proc p {} {set a(n) 1; return [array size a]}\nputs [p][p][p]",
    // A global of the same name is a different variable.
    "set a(g) 1\nproc p {} {array set a {x 1}; return [array size a]}\nputs [p]\nputs [lsort [array get a]]",
    // `global` chooses the global one.
    "set a(g) 1\nproc p {} {global a; set a(fromproc) 1; return [lsort [array names a]]}\nputs [p]\nputs [lsort [array names a]]",
    // ── the ensemble, on a local ─────────────────────────────────────────
    "proc p {} {array set c {k v}; return \"[array exists c] [array size c] [array names c] [array get c]\"}\nputs [p]",
    "proc p {} {return [array exists nope]}\nputs [p]",
    "proc p {} {return [array size nope]}\nputs [p]",
    "proc p {} {array set c {a 1 b 2 aa 3}; return [lsort [array names c a*]]}\nputs [p]",
    "proc p {} {array set c {a 1 b 2}; return [lsort [array names c -exact b]]}\nputs [p]",
    "proc p {} {array set c {a 1 b 2}; array unset c a; return [lsort [array get c]]}\nputs [p]",
    "proc p {} {array set c {a 1 b 2}; array unset c; return [array exists c]}\nputs [p]",
    // ── unset ────────────────────────────────────────────────────────────
    "proc p {} {array set b {x 1 y 2}; unset b(x); return [lsort [array get b]]}\nputs [p]",
    "proc p {} {array set b {x 1}; unset b; return [array exists b]}\nputs [p]",
    "proc p {} {array set b {x 1}; unset -nocomplain b(nope); return [array size b]}\nputs [p]",
    // ── elements ─────────────────────────────────────────────────────────
    "proc p {} {set d(k) 1; incr d(k) 4; return $d(k)}\nputs [p]",
    "proc p {} {incr d(fresh); return $d(fresh)}\nputs [p]",
    "proc p {} {set d(a) 1; set d(b) 2; return [lsort [array names d]]}\nputs [p]",
    // An index that is itself substituted.
    "proc p {} {set k name; set d($k) v; return [array get d]}\nputs [p]",
    // ── recursion: activations do not share ──────────────────────────────
    "proc rec {n} {set arr($n) here\nif {$n > 0} {rec [expr {$n - 1}]}\nreturn [array names arr]}\nputs [rec 2]",
    "proc rec {n} {set arr(x) $n\nif {$n > 0} {rec [expr {$n - 1}]}\nreturn $arr(x)}\nputs [rec 3]",
    // ── a coroutine's frame is its own ───────────────────────────────────
    "proc co {} {array set z {a 1}; yield [array size z]; set z(b) 2; return [array size z]}\nputs [coroutine c1 co]\nputs [c1]",
    "proc co {} {array set z {}; set z(only) 1; yield [array names z]; return done}\nputs [coroutine c2 co]\nputs [coroutine c3 co]",
    // ── a local array is still not a value ───────────────────────────────
    "proc p {} {array set e {x 1}; return [catch {set e} m]}\nputs [p]",
    "proc p {} {array set e {x 1}; catch {set e} m; return $m}\nputs [p]",
    "proc p {} {array set e {x 1}; catch {set e scalar} m; return $m}\nputs [p]",
    // ── array set over an existing scalar: the body decides the wording ──
    "set a 5\ncatch {array set a {}} m\nputs $m",
    "set a 5\ncatch {array set a {k v}} m\nputs $m",
    "proc p {} {set b 5; catch {array set b {}} m; return $m}\nputs [p]",
    "proc p {} {set b 5; catch {array set b {k v}} m; return $m}\nputs [p]",
    "set a 5\nproc p {} {global a; catch {array set a {k v}} m; return $m}\nputs [p]",
    // ── reading an element of an array that is not there ─────────────────
    "proc p {} {return [catch {set d(nope)} m]}\nputs [p]",
    "proc p {} {catch {set d(nope)} m; return $m}\nputs [p]",
    "proc p {} {array set d {x 1}; catch {set d(nope)} m; return $m}\nputs [p]",
    // A local scalar is not an array.
    "proc p {} {set s plain; catch {set s(k)} m; return $m}\nputs [p]",
    // ── a local array survives a nested call and a loop ──────────────────
    "proc inner {} {array set q {deep 1}; return [array size q]}\nproc outer {} {array set q {a 1 b 2}; set n [inner]; return \"$n [array size q]\"}\nputs [outer]",
    "proc p {} {for {set i 0} {$i < 4} {incr i} {set t($i) $i}\nreturn [array size t]}\nputs [p]",
];

fn tclsh() -> Option<PathBuf> {
    for name in ["tclsh", "tclsh9.0", "tclsh8.6"] {
        if let Ok(out) = Command::new("sh")
            .arg("-c")
            .arg(format!("command -v {name}"))
            .output()
        {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }
    }
    None
}

/// Run a program through tclsh, returning its stdout and the first line of any
/// error it reported. tclsh follows an error with a stack trace and tclrs does
/// not, so only the first line is comparable.
fn reference(tclsh: &PathBuf, program: &str) -> (String, Option<String>) {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "tclrs-localarr-{}-{}.tcl",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path, program).expect("write program");
    let out = Command::new(tclsh).arg(&path).output().expect("run tclsh");
    let _ = std::fs::remove_file(&path);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let error = (!out.status.success()).then(|| {
        String::from_utf8_lossy(&out.stderr)
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_string()
    });
    (stdout, error)
}

#[test]
fn local_arrays_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };

    let mut failures = Vec::new();
    for program in PROGRAMS {
        let (expected, error) = reference(&tclsh, program);
        assert!(
            error.is_none(),
            "tclsh rejected a program that should run:\n{program}\n{}",
            error.unwrap_or_default()
        );
        match tclrs::eval(program) {
            Ok(outcome) if outcome.output == expected => {}
            Ok(outcome) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs: {:?}",
                outcome.output
            )),
            Err(e) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs failed: {e}"
            )),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} programs diverge:\n\n{}",
        failures.len(),
        PROGRAMS.len(),
        failures.join("\n\n")
    );
}

/// The whole point of a *local* array: an activation's elements are its own.
///
/// A global-table array keyed by name would pass every single-call program
/// above and fail these, so they are the assertions that would catch a
/// regression back to the old storage.
#[test]
fn a_local_array_belongs_to_its_activation() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };

    for program in [
        // Two calls: the second must not see the first's elements.
        "proc p {} {set a(x) 1; return [array size a]}\nputs \"[p] [p]\"",
        // Recursion: the deepest frame's array is empty of the shallower ones.
        "proc rec {n} {set arr($n) 1\nif {$n > 0} {return [rec [expr {$n - 1}]]}\nreturn [array size arr]}\nputs [rec 3]",
        // The caller's array is intact after the callee builds its own.
        "proc inner {} {set a(only) 1; return [array size a]}\nproc outer {} {set a(x) 1; set a(y) 2; inner; return [array size a]}\nputs [outer]",
    ] {
        let (expected, error) = reference(&tclsh, program);
        assert!(error.is_none(), "tclsh rejected:\n{program}");
        let got = tclrs::eval(program)
            .unwrap_or_else(|e| panic!("tclrs failed on:\n{program}\n{e}"))
            .output;
        assert_eq!(got, expected, "an activation's array leaked:\n{program}");
    }
}
