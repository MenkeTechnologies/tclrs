//! Differential execution of coroutines: `coroutine`, `yield`, `yieldto`,
//! `info coroutine` and the lifecycle of a coroutine's context command.
//!
//! Same contract as the other harnesses — no expected output is written by
//! hand. Every program is run by tclsh and by tclrs and the two stdouts are
//! compared byte for byte, so which value a resumption delivers, where a
//! `yieldto` sends its result, and when a context command stops existing are
//! all checked against the reference implementation.
//!
//! The suspension points are what make these worth running: a coroutine parks
//! in the middle of an expression (`incr x [yield $x]`), inside a loop body,
//! inside an open `catch` region and several procedure calls deep, and each
//! time the resumed VM has to continue with a stack the compiler's static
//! depth accounting still describes.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const PROGRAMS: &[&str] = &[
    // ── generators: the value of each resumption ──
    "proc gen {} {yield\nset i 0\nwhile {$i < 3} {yield $i\nincr i}\nreturn done}\ncoroutine c gen\nputs [c]\nputs [c]\nputs [c]\nputs [c]",
    "proc allNumbers {} {yield\nset i 0\nwhile 1 {yield $i\nincr i 2}}\ncoroutine nextNumber allNumbers\nfor {set i 0} {$i < 10} {incr i} {puts \"received [nextNumber]\"}",
    // The `coroutine` command's own value is the first yield's argument.
    "proc g {} {yield hello\nyield world\nreturn fin}\nputs \"create: [coroutine c g]\"\nputs [c]\nputs [c]",
    // A body that never yields runs to completion inside `coroutine`.
    "proc g {} {return 42}\nputs [coroutine c g]",
    "proc g {} {puts side\nreturn {}}\nputs \"<[coroutine c g]>\"",
    // An empty yield yields the empty string.
    "proc g {} {yield\nyield\nreturn}\ncoroutine c g\nputs \"<[c]>\"\nputs \"<[c]>\"",
    // ── the resumption value arrives as the value of `yield` ──
    "proc g {} {set a [yield A]\nputs \"got $a\"\nset b [yield B]\nputs \"got $b\"\nreturn end}\ncoroutine c g\nputs [c 1]\nputs [c 2]",
    // A coroutine parked in the middle of an expression: the resumption value
    // lands where the compiler left room for the `yield`'s result.
    "proc acc {} {set x 0\nwhile 1 {incr x [yield $x]}}\ncoroutine accumulator acc\nfor {set i 0} {$i < 10} {incr i} {puts \"$i -> [accumulator $i]\"}",
    "proc g {} {puts \"sum [expr {1 + [yield a] + [yield b]}]\"\nreturn done}\ncoroutine c g\nputs [c 10]\nputs [c 20]",
    // ── arguments to the body ──
    "proc g {a b} {yield \"$a-$b\"\nyield [expr {$a+$b}]}\ncoroutine c g 3 4\nputs [c]",
    "proc g {a {b B} args} {yield \"$a|$b|$args\"\nyield [llength $args]}\ncoroutine c g 1\nputs [c]\nputs [coroutine d g 1 2 3 4]\nputs [d]",
    // ── suspending at depth ──
    "proc inner {} {yield deep}\nproc outer {} {inner\nreturn done}\ncoroutine c outer\nputs [c]",
    "proc fib {n} {if {$n < 2} {yield $n\nreturn $n}\nset a [fib [expr {$n-1}]]\nset b [fib [expr {$n-2}]]\nset s [expr {$a+$b}]\nyield $s\nreturn $s}\ncoroutine c fib 5\nfor {set i 0} {$i < 8} {incr i} {puts \"[catch {c} m] $m\"}",
    // Suspending inside every loop this frontend compiles, and inside a `catch`
    // region that is still open across the suspension.
    "proc g {} {\n    puts [catch {yield A\nerror boom} m]\n    puts \"caught: $m\"\n    foreach x {1 2 3} {yield $x}\n    set i 0\n    while {$i < 2} {yield w$i\nincr i}\n    for {set j 0} {$j < 2} {incr j} {yield f$j}\n    return fin\n}\ncoroutine c g\nfor {set k 0} {$k < 8} {incr k} {puts [c]}",
    "proc g {} {foreach x {a b c} {if {$x eq \"b\"} {continue}\nyield $x}\nreturn last}\ncoroutine c g\nputs [c]\nputs [c]",
    // ── resuming from inside a loop, a procedure and another coroutine ──
    "proc gen {} {yield\nset i 0\nwhile 1 {yield [incr i]}}\ncoroutine c gen\nproc take {n} {set out {}\nfor {set k 0} {$k < $n} {incr k} {lappend out [c]}\nreturn $out}\nputs [take 3]\nwhile 1 {set v [c]\nif {$v > 6} break\nputs \"loop $v\"}\nputs [take 2]",
    "proc src {} {yield\nset i 0\nwhile 1 {yield [incr i]}}\nproc filt {} {yield\nwhile 1 {set v [a]\nyield [expr {$v*10}]}}\ncoroutine a src\ncoroutine b filt\nfor {set k 0} {$k < 4} {incr k} {puts [b]}",
    // A producer and a consumer that alternate, driven from the script.
    "proc produce {} {yield\nforeach word {alpha beta gamma} {yield $word}\nreturn {}}\nproc consume {} {set seen {}\nwhile 1 {set w [p]\nif {$w eq \"\"} break\nlappend seen [string toupper $w]}\nyield $seen\nreturn done}\ncoroutine p produce\ncoroutine c consume\nputs [c]",
    // ── yieldto: the resumer is handed to the target ──
    "proc bbody {} {yield\nputs \"b resumed\"\nreturn B}\nproc abody {} {yield\nputs \"a resumed\"\nset r [yieldto b 1]\nputs \"a again <$r>\"\nreturn A}\ncoroutine b bbody\ncoroutine a abody\nputs \"call a -> <[a]>\"\nputs [catch {a} m]\nputs \"a: $m\"\nputs [catch {b} m]\nputs \"b: $m\"",
    "proc g {} {set r [yieldto c2 X]\nputs \"back <$r>\"\nyield done}\nproc h {} {yield\nputs \"in c2\"\nreturn H}\ncoroutine c2 h\ncoroutine c1 g\nputs \"create c1 gave <[c1]>\"",
    // A resumption after `yieldto` delivers the whole argument list, quoted as
    // a list rather than as a single value.
    "proc g {} {set r [yieldto d]\nputs \"<$r>\"\nputs [llength $r]\nyield done}\nproc h {} {yield\nreturn {}}\ncoroutine d h\ncoroutine c g\nc {a b} {} \"c d\" e",
    // The three-way juggler from coroutine(n), which cedes control to a name it
    // reads out of `info coroutine`.
    "proc j {name target {value {}}} {\n    if {$value eq \"\"} {set value [yield [info coroutine]]}\n    while {$value ne \"\"} {\n        puts \"$name : $value\"\n        set value [string range $value 0 end-1]\n        set got [yieldto $target $value]\n        set value [lindex $got 0]\n    }\n}\ncoroutine j1 j Larry [coroutine j2 j Curly [coroutine j3 j Moe j1]] Nyuck\nputs done",
    // ── info coroutine ──
    "proc where {} {return [info coroutine]}\nproc g {} {yield [where]\nyield [where]}\nputs \"first: [coroutine c g]\"\nputs \"second: [c]\"\nputs \"outside: <[where]>\"",
    "puts \"<[info coroutine]>\"",
    // ── globals, arrays and locals ──
    "set gv 0\nproc g {} {global gv\nset loc 0\nwhile 1 {incr loc\nincr gv\nyield \"$loc $gv\"}}\ncoroutine c g\nputs [c]\nputs [c]\nincr gv 10\nputs [c]\nputs $gv",
    "set a(x) 1\nproc g {} {global a\nset a(y) 2\nyield [array size a]\nset a(z) 3\nyield [lsort [array names a]]}\ncoroutine c g\nputs [c]\nputs $a(y)\nputs [c]\nputs [array size a]",
    "proc g {} {set d [dict create k 1]\nyield [dict get $d k]\ndict set d k 2\nyield [dict get $d k]}\ncoroutine c g\nputs [c]\nputs [c]",
    // ── lifecycle ──
    "proc g {} {yield a\nreturn b}\ncoroutine c g\nputs [c]\nputs [catch {c} m]\nputs $m",
    "proc g {} {yield a\nyield b}\ncoroutine c g\nputs [c]\nputs [c]\nputs [catch {c} m]\nputs $m",
    // Re-creating a name that is still live replaces the coroutine.
    "proc g {t} {yield $t-1\nyield $t-2}\nputs [coroutine c g a]\nputs [c]\nputs [coroutine c g b]\nputs [c]",
    // ── errors ──
    "proc g {} {yield a\nerror boom}\ncoroutine c g\nputs [catch {c} m]\nputs $m\nputs [catch {c} m2]\nputs $m2",
    "proc g {} {yield\nputs [catch {error inside} m]\nputs $m\nyield x\nerror out}\ncoroutine c g\nc\nputs ---\nputs [catch {c} e]\nputs $e",
    // An error out of a coroutine resumed by a coroutine: caught in the middle
    // one the first time, uncaught the second, which ends them both.
    "proc inner {} {yield\nerror deep}\nproc outer {} {yield\nputs [catch {i} m]\nputs \"outer caught: $m\"\nyield ok\ni}\ncoroutine i inner\ncoroutine o outer\nputs [o]\nputs [catch {o} m]\nputs \"top: $m\"\nputs [catch {o} m2]\nputs \"o now: $m2\"",
    // Resuming with more than a `yield` can take, which leaves the coroutine
    // suspended exactly where it was.
    "proc g {} {set a [yield A]\nputs \"got <$a>\"\nyield}\ncoroutine c g\nputs [catch {c 1 2} m]\nputs $m\nputs [catch {c} m2]\nputs \"<$m2>\"",
    // `yield` outside a coroutine, including through a procedure that is only
    // sometimes called from one.
    "puts [catch {yield} m]\nputs $m\nputs [catch {yield x} m]\nputs $m\nproc p {} {yield 1}\nputs [catch {p} m]\nputs $m\nputs alive",
    "proc p {} {return [yield 1]}\nproc g {} {yield [p]\nreturn done}\ncoroutine c g\nputs [c]\nputs [catch {p} m]\nputs $m",
    // Resuming a coroutine that is running.
    "proc g {} {yield\nputs inner\nc\nputs back}\ncoroutine c g\nputs [catch {c} m]\nputs $m",
    // ── the pieces together ──
    "proc chars {s} {\n    yield\n    foreach ch [split $s {}] {yield $ch}\n    return {}\n}\nproc runs {} {\n    yield\n    set prev {}\n    set n 0\n    while 1 {\n        set ch [src]\n        if {$ch eq $prev} {incr n\ncontinue}\n        if {$prev ne \"\"} {yield \"$prev$n\"}\n        set prev $ch\n        set n 1\n        if {$ch eq \"\"} break\n    }\n    return {}\n}\ncoroutine src chars aaabbcaaa\ncoroutine rle runs\nwhile 1 {set r [rle]\nif {$r eq \"\"} break\nputs $r}",
];

/// Programs whose error escapes to the top level, where the message tclsh
/// prints is the specification for the one tclrs reports.
const FAILING: &[&str] = &[
    // Calling a coroutine whose body has returned.
    "proc g {} {yield a}\ncoroutine c g\nc\nc",
    "proc g {} {return 1}\ncoroutine c g\nc",
    // An error the body raises and nobody catches.
    "proc g {} {yield a\nerror boom}\ncoroutine c g\nc",
    "proc g {} {error early}\ncoroutine c g",
    // An error out of a coroutine resumed by a coroutine unwinds both.
    "proc inner {} {yield\nerror deep}\nproc outer {} {yield\ni}\ncoroutine i inner\ncoroutine o outer\no",
    // Too many arguments for a coroutine suspended at a `yield`.
    "proc g {} {yield}\ncoroutine c g\nc 1 2",
    // `yield` and `yieldto` outside a coroutine.
    "yield",
    "proc p {} {yield}\np",
    // Resuming the running coroutine.
    "proc g {} {yield\nc}\ncoroutine c g\nc",
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

/// tclsh's stdout, or its error message when the script fails.
fn reference(tclsh: &PathBuf, program: &str) -> Result<String, String> {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "tclrs-coro-{}-{}.tcl",
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

#[test]
fn coroutines_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };

    let mut failures = Vec::new();
    for program in PROGRAMS {
        let expected = match reference(&tclsh, program) {
            Ok(out) => out,
            Err(e) => {
                failures.push(format!("tclsh rejected program:\n{program}\n{e}"));
                continue;
            }
        };
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

/// Failures have to match too: an error that escapes a coroutine carries the
/// message tclsh produces.
#[test]
fn coroutine_errors_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };

    let mut failures = Vec::new();
    for program in FAILING {
        let Err(expected) = reference(&tclsh, program) else {
            failures.push(format!(
                "tclsh accepted a program meant to fail:\n{program}"
            ));
            continue;
        };
        // tclsh writes the message followed by a stack trace; only the first
        // line is the message itself.
        let expected = expected.lines().next().unwrap_or_default().to_string();
        match tclrs::eval(program) {
            Err(actual) if actual == expected => {}
            Err(actual) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs: {actual:?}"
            )),
            Ok(outcome) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs succeeded: {outcome:?}"
            )),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} error programs diverge:\n\n{}",
        failures.len(),
        FAILING.len(),
        failures.join("\n\n")
    );
}

/// A coroutine's context command is a value like any other, so a script's
/// value can come out of one.
#[test]
fn script_value_can_come_from_a_coroutine() {
    let script = "proc g {} {yield first\nreturn second}\ncoroutine c g";
    assert_eq!(tclrs::eval(script).unwrap().result, "first");
    assert_eq!(
        tclrs::eval(&format!("{script}\nc")).unwrap().result,
        "second"
    );
    assert_eq!(
        tclrs::eval("proc g {} {yield {}}\ncoroutine c g")
            .unwrap()
            .result,
        ""
    );
    assert_eq!(
        tclrs::eval("proc g {} {yield [info coroutine]}\ncoroutine c g")
            .unwrap()
            .result,
        "::c"
    );
}

/// Constructs whose Tcl semantics this frontend does not model are refused at
/// compile time. Approximating them would be worse than failing: a coroutine
/// that half works corrupts the control flow of everything that resumes it.
#[test]
fn unsupported_coroutine_constructs_are_refused() {
    for (src, expected) in [
        // The name has to be known to every call site, so `coroutine` may only
        // appear where the prescan reaches it.
        (
            "proc g {} {yield}\nif {1} {coroutine c g}",
            "\"coroutine\" is only supported at the top level",
        ),
        (
            "proc g {} {yield}\nwhile {1} {coroutine c g}",
            "\"coroutine\" is only supported at the top level",
        ),
        (
            "proc g {} {yield}\nproc make {} {coroutine c g}",
            "\"coroutine\" is only supported at the top level",
        ),
        (
            "proc g {} {yield}\ncatch {coroutine c g}",
            "\"coroutine\" is only supported at the top level",
        ),
        // The body is entered through the chunk's sub table, so it has to be a
        // procedure this script defines.
        (
            "coroutine c nosuchproc",
            "invalid command name \"nosuchproc\"",
        ),
        (
            "coroutine c puts hi",
            "a coroutine of the built-in command \"puts\" is not supported",
        ),
        (
            "set n c\nproc g {} {yield}\ncoroutine $n g",
            "coroutine name must be a literal",
        ),
        (
            "proc g {} {yield}\nset b g\ncoroutine c $b",
            "coroutine command must be a literal",
        ),
        // One name, one meaning.
        // Whichever command the compiler reaches first reports the clash;
        // both names are known to both prescans before either is compiled.
        (
            "proc c {} {}\nproc g {} {yield}\ncoroutine c g",
            "procedure \"c\" collides with a coroutine",
        ),
        (
            "proc g {} {yield}\ncoroutine c g\nproc c {} {}",
            "coroutine \"c\" collides with a procedure",
        ),
        (
            "proc g {} {yield}\ncoroutine set g",
            "redefining the built-in command \"set\"",
        ),
        (
            "proc yield {} {}",
            "redefining the built-in command \"yield\"",
        ),
        (
            "proc coroutine {a b} {}",
            "redefining the built-in command \"coroutine\"",
        ),
        // Argument counts, at the creation and at the commands themselves.
        (
            "coroutine",
            "wrong # args: should be \"coroutine name cmd ?arg ...?\"",
        ),
        (
            "proc g {} {yield}\ncoroutine c",
            "wrong # args: should be \"coroutine name cmd ?arg ...?\"",
        ),
        (
            "proc g {a b} {yield}\ncoroutine c g 1",
            "wrong # args: should be \"g a b\"",
        ),
        // The coroutine is created so that the body actually runs: an argument
        // count is checked when the command is reached, as tclsh checks it, so
        // defining a procedure whose body could not work is not itself an
        // error in either engine.
        (
            "proc g {} {yield a b}\ncoroutine c g",
            "wrong # args: should be \"yield ?value?\"",
        ),
        (
            "yieldto",
            "wrong # args: should be \"yieldto command ?arg ...?\"",
        ),
        // `yieldto` at a command that is not a coroutine would have to run that
        // command in the resumer's context, which this frontend cannot do.
        (
            "proc g {} {yieldto string cat a}",
            "ceding control to a command that is not a coroutine",
        ),
        (
            "proc h {} {}\nproc g {} {yieldto h}",
            "ceding control to a command that is not a coroutine",
        ),
        // `info` has exactly one subcommand here.
        (
            "puts [info commands]",
            "only \"info coroutine\" is supported",
        ),
        ("puts [info level]", "only \"info coroutine\" is supported"),
        ("info", "wrong # args"),
        (
            "puts [info coroutine x]",
            "wrong # args: should be \"info coroutine\"",
        ),
    ] {
        let err = tclrs::eval(src).expect_err(&format!("{src:?} should fail"));
        assert!(
            err.contains(expected),
            "{src:?}: expected an error mentioning {expected:?}, got {err:?}"
        );
    }
}

/// A `yieldto` whose target is a word can only be checked when it runs, and
/// the check is the same one: the target must be a coroutine of this script.
#[test]
fn a_computed_yieldto_target_is_checked_at_run_time() {
    let err = tclrs::eval(
        "proc g {} {set t string\nyieldto $t cat a}\ncoroutine c g\nputs [catch {c} m]\nputs $m",
    )
    .expect_err("should fail");
    assert!(
        err.contains("ceding control to a command that is not a coroutine"),
        "got {err:?}"
    );
}
