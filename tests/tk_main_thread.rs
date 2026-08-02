//! `--tk` moves the interpreter to the main thread. It may not cost anything.
//!
//! macOS Tk has to be initialised on the main thread
//! (`tk9.0.4/macosx/tkMacOSXNotify.c:258-272`), and the main thread's stack is
//! 8 MiB, while 1000 levels of nested `eval` need 99 MiB in this profile. The
//! binary bridges that by running the interpreter on a stack it maps itself,
//! on the same thread — see `src/main_thread.rs`.
//!
//! That bridge is either invisible or broken, and this file is what tells the
//! two apart: every script below is run twice, once the ordinary way and once
//! with `--tk`, and the two runs have to agree on stdout, stderr and exit
//! status. The deep-nesting cases are the ones that matter — they are what the
//! 256 MiB stack exists for, and they are what a stack switch that lost 240 MiB
//! of it would fail.

#![cfg(feature = "tk")]

use std::process::Command;

const TCLRS: &str = env!("CARGO_BIN_EXE_tclrs");

/// What one run produced.
#[derive(PartialEq, Eq, Debug)]
struct Ran {
    stdout: String,
    stderr: String,
    status: Option<i32>,
}

fn run(args: &[&str]) -> Ran {
    let out = Command::new(TCLRS).args(args).output().expect("run tclrs");
    Ran {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        status: out.status.code(),
    }
}

/// A script that nests `eval` `depth` levels deep and then prints.
fn nested(depth: usize) -> String {
    let mut script = "set n ok".to_string();
    for _ in 0..depth {
        script = format!("eval {{{script}}}");
    }
    format!("{script}\nputs $n\n")
}

/// Run `script` both ways and return the pair.
fn both_ways(script: &str) -> (Ran, Ran) {
    (run(&["-c", script]), run(&["--tk", "-c", script]))
}

#[test]
fn a_tk_session_runs_an_ordinary_script_identically() {
    for script in [
        "puts hello",
        "puts [expr {6 * 7}]",
        "set x 1; incr x; puts $x",
        "nosuchcommand",
        "puts [string toupper abc]",
    ] {
        let (worker, main) = both_ways(script);
        assert_eq!(worker, main, "--tk changed the result of {script:?}");
    }
}

#[test]
fn the_recursion_limit_is_the_same_on_the_main_thread() {
    // 1000 is the deepest the interpreter allows and 1001 is the refusal. Both
    // have to come out the same on the borrowed stack: the first proves the
    // stack is really 256 MiB and not the main thread's 8 MiB, and the second
    // proves the limit still ends the recursion before the stack does.
    for depth in [1000, 1001] {
        let (worker, main) = both_ways(&nested(depth));
        assert_eq!(
            worker, main,
            "--tk changed what {depth} levels of nested eval do"
        );
    }

    // And spelled out, so a failure says which half broke rather than only
    // that the two disagreed.
    let deep = run(&["--tk", "-c", &nested(1000)]);
    assert_eq!(deep.stdout, "ok\n");
    assert_eq!(deep.status, Some(0));

    let refused = run(&["--tk", "-c", &nested(1001)]);
    assert_eq!(
        refused.stderr, "too many nested evaluations (infinite loop?)\n",
        "the refusal is what keeps the stack switch honest"
    );
    assert_eq!(refused.status, Some(1));
}

#[test]
fn a_panic_on_the_borrowed_stack_does_not_unwind_past_the_switch() {
    // A stack switch that let a panic unwind through it would be walking a
    // stack the runtime knows nothing about. `main_thread::run` catches
    // instead, so a failing script is an exit status and not an abort.
    let ran = run(&["--tk", "-c", "expr {1/0}"]);
    assert_eq!(ran.status, Some(1), "expected an exit status, got {ran:?}");
    assert!(!ran.stderr.contains("panicked"), "{ran:?}");
}

#[test]
fn tk_is_advertised_only_where_it_exists() {
    let help = run(&["--help"]);
    assert_eq!(help.status, Some(0));
    assert!(
        help.stdout.contains("--tk"),
        "a build that can host Tk should say so in --help"
    );
}

#[test]
fn the_option_belongs_to_the_driver_and_not_to_the_script() {
    // Everything after `-c` is the script's argv, so a `--tk` there is an
    // argument and not an option — the same rule every other option follows.
    let ran = run(&["-c", "puts $argv", "--tk"]);
    assert_eq!(ran.stdout, "--tk\n");
    assert_eq!(ran.status, Some(0));
}
