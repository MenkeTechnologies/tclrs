//! Differential driver tests: the `tclrs` binary against `tclsh`.
//!
//! The other suites compare what a script computes. This one compares what the
//! process does with it — stdout, stderr and exit status, for the same script
//! given to both binaries the same way. That is where the driver's own rules
//! live, and none of them are guessable: a script read from a file stops at its
//! first failure and exits 1, the same script piped to stdin reports the
//! failure and carries on and exits 0, and a file that cannot be read has a
//! message with a fixed spelling.
//!
//! Expectations are never written by hand here either; `tclsh` produces them.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// The tclrs binary under test, built by cargo for this integration test.
const TCLRS: &str = env!("CARGO_BIN_EXE_tclrs");

/// Scripts that both implementations run to completion. Compared in full:
/// stdout, stderr and exit status.
///
/// Every one is run as a file with the arguments `alpha` and `beta gamma`, so
/// `argv0` / `argc` / `argv` are under test throughout.
const SCRIPTS: &[&str] = &[
    // The driver's own variables.
    "puts $argv0",
    "puts $argc",
    "puts $argv",
    "puts [llength $argv]\nputs [lindex $argv 1]",
    // Plain scripts, to pin stdout and a zero exit for the ordinary case.
    "puts hello",
    "set x 5\nputs [expr {$x * 2}]",
    "puts -nonewline a\nputs b",
    "set i 0\nwhile {$i < 3} {puts $i; incr i}",
    // `eval` of a literal script.
    "puts [eval {expr 1+2}]",
    "puts [eval {}]",
    "eval {puts a}\nputs b",
    "puts <[eval {  set z 4  }]>\nputs $z",
    // `eval` of a script that is not a literal — the case the compiler cannot
    // see and the runtime-eval path exists for.
    "set c {set y 9}\neval $c\nputs $y",
    "set body {puts $k}\nforeach k {1 2 3} {eval $body}",
    "set cmd list\nputs [eval $cmd 1 2 3]",
    // `eval` concatenates several arguments the way `concat` does.
    "puts [eval list a b c]",
    "puts [eval {list} {a b} {c}]",
    "set s {}\nforeach w {a b} {eval [list lappend s $w]}\nputs $s",
    // State written inside an `eval` is visible outside it, and the other way
    // round — one set of variables, not two.
    "set p 1\neval {set p [expr {$p + 1}]}\nputs $p",
    "eval {eval {set deep 3}}\nputs $deep",
    "puts [eval {eval {expr 6*7}}]",
    "eval {set a(k) v}\nputs $a(k)",
    "set d [dict create a 1]\neval {dict set d b 2}\nputs [dict get $d b]",
    // The same `eval` text on every pass of a loop: the cache makes it compile
    // once, and the answer must not change because of it.
    "set i 0\nwhile {$i < 5} {eval {incr i}}\nputs $i",
    "set n 0\nforeach v {a b c d} {eval {incr n}}\nputs $n",
    // A loop whose `eval` text differs on every pass, which the cache must not
    // conflate.
    "foreach n {1 2 3} {eval \"puts [expr {$n * 10}]\"}",
    // The rest of the command set, run through the driver rather than the
    // library, so the binary is known to reach it.
    "set l [list a b c]\nputs [llength $l]\nputs [lindex $l end]\nputs [lsort {c a b}]",
    "array set a {x 1 y 2}\nputs [lsort [array names a]]\nputs $a(x)",
    "set d [dict create a 1 b 2]\nputs [dict get $d b]\nputs [dict size $d]",
];

/// Scripts whose *first* command fails, so both implementations produce the
/// same (empty) stdout before stopping.
///
/// stdout and exit status are compared in full. stderr is compared on its first
/// line — the message — because the reference interpreter follows it with the
/// stack of commands that raised it, and tclrs resolves command dispatch while
/// compiling and has no such stack. Where tclrs does report a source location,
/// the line it prints must appear verbatim in `tclsh`'s report; that is checked
/// too.
const FAILING: &[&str] = &[
    "nosuchcmd",
    "nosuchcmd a b c",
    "eval",
    "eval {nosuchcmd}",
    "eval {set x {",
    "break",
    "puts [expr {1/0}]",
    "puts [expr {1 % 0}]",
    "llength {a {b}",
];

/// Whole stdin sessions. The driver reads stdin a command at a time, so a
/// failure does not end the session — which makes stdout, stderr and exit
/// status all comparable in full, including after an error.
const STDIN_SESSIONS: &[&str] = &[
    "puts a\nnosuch\nputs b\n",
    "nosuch\n",
    "eval\n",
    "set x 5\nputs [expr {$x*2}]\n",
    // A non-terminal stdin echoes nothing, so a bare value prints nothing.
    "expr {1+2}\nlist a b c\n",
    // A command spanning lines, held open by a brace, a quote and a bracket.
    "set y {\nabc}\nputs $y\n",
    "puts \"a\nb\"\n",
    "set q [expr {1 +\n2}]\nputs $q\n",
    "puts [list a \\\nb]\n",
    "if {1} {\nputs inner\n}\n",
    // Text left unfinished at end of input is discarded, not run.
    "puts a\nset x {\n",
    "puts a\nputs \"open\n",
    // Blank lines and comments produce nothing.
    "\n\n   \nputs a\n",
    "# comment\nputs a\n",
    // State persists from one command to the next, including through `eval`.
    "set x 1\nset x [expr {$x + 1}]\nputs $x\n",
    "puts [eval {expr 2+2}]\nset q 7\neval {incr q}\nputs $q\n",
    // A failed command does not take the session down with it.
    "set a 1\nnosuch\nputs $a\n",
    "puts a; puts b\n",
];

/// The arguments every file-mode script is run with.
const SCRIPT_ARGS: &[&str] = &["alpha", "beta gamma"];

#[derive(Debug, PartialEq, Eq)]
struct Run {
    stdout: String,
    stderr: String,
    status: Option<i32>,
}

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

/// A directory of this test's own, so concurrent runs never share a script
/// file. Removed by the OS's temp policy, not by the test, so a failure leaves
/// the script that caused it behind.
fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tclrs-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

fn run(binary: &PathBuf, args: &[&str], stdin: &str) -> Run {
    use std::io::Write;
    let mut child = Command::new(binary)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait");
    Run {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        status: out.status.code(),
    }
}

/// Write `program` to a file in the scratch directory and run it under both
/// binaries with the same path and the same arguments.
fn run_file(tclsh: &PathBuf, program: &str, index: usize) -> (Run, Run) {
    let path = scratch().join(format!("script-{index}.tcl"));
    std::fs::write(&path, program).expect("write script");
    let path = path.to_string_lossy().into_owned();
    let mut args = vec![path.as_str()];
    args.extend_from_slice(SCRIPT_ARGS);
    let tclrs_bin = PathBuf::from(TCLRS);
    (run(tclsh, &args, ""), run(&tclrs_bin, &args, ""))
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}

#[test]
fn scripts_run_from_a_file_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };

    let mut failures = Vec::new();
    for (i, program) in SCRIPTS.iter().enumerate() {
        let (reference, actual) = run_file(&tclsh, program, i);
        assert!(
            reference.stderr.is_empty() && reference.status == Some(0),
            "SCRIPTS entry is not a clean run under tclsh:\n{program}\n{reference:?}"
        );
        if actual != reference {
            failures.push(format!(
                "program:\n{program}\n  tclsh: {reference:?}\n  tclrs: {actual:?}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} scripts diverge:\n\n{}",
        failures.len(),
        SCRIPTS.len(),
        failures.join("\n\n")
    );
}

#[test]
fn failing_scripts_report_and_exit_like_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };

    let mut failures = Vec::new();
    for (i, program) in FAILING.iter().enumerate() {
        let (reference, actual) = run_file(&tclsh, program, 1000 + i);
        assert!(
            reference.status != Some(0),
            "FAILING entry succeeds under tclsh:\n{program}\n{reference:?}"
        );
        let mut diverged = Vec::new();
        if actual.stdout != reference.stdout {
            diverged.push(format!(
                "stdout: tclsh {:?} vs tclrs {:?}",
                reference.stdout, actual.stdout
            ));
        }
        if actual.status != reference.status {
            diverged.push(format!(
                "status: tclsh {:?} vs tclrs {:?}",
                reference.status, actual.status
            ));
        }
        if first_line(&actual.stderr) != first_line(&reference.stderr) {
            diverged.push(format!(
                "message: tclsh {:?} vs tclrs {:?}",
                first_line(&reference.stderr),
                first_line(&actual.stderr)
            ));
        }
        // tclrs prints at most one further line, the source location, and only
        // in the spelling `tclsh` uses for the same failure.
        for line in actual.stderr.lines().skip(1) {
            if !reference.stderr.lines().any(|l| l == line) {
                diverged.push(format!("tclrs invented the line {line:?}"));
            }
        }
        if !diverged.is_empty() {
            failures.push(format!("program:\n{program}\n  {}", diverged.join("\n  ")));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} failing scripts diverge:\n\n{}",
        failures.len(),
        FAILING.len(),
        failures.join("\n\n")
    );
}

#[test]
fn stdin_sessions_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    let tclrs_bin = PathBuf::from(TCLRS);

    let mut failures = Vec::new();
    for session in STDIN_SESSIONS {
        let reference = run(&tclsh, &[], session);
        let actual = run(&tclrs_bin, &[], session);
        if actual != reference {
            failures.push(format!(
                "session:\n{session}\n  tclsh: {reference:?}\n  tclrs: {actual:?}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} stdin sessions diverge:\n\n{}",
        failures.len(),
        STDIN_SESSIONS.len(),
        failures.join("\n\n")
    );
}

/// A script that cannot be read is reported the same way, down to the wording
/// of the reason.
#[test]
fn unreadable_scripts_are_reported_like_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    let tclrs_bin = PathBuf::from(TCLRS);
    let dir = scratch();

    let missing = dir.join("definitely-absent.tcl");
    let _ = std::fs::remove_file(&missing);
    let cases: Vec<String> = vec![
        missing.to_string_lossy().into_owned(),
        dir.to_string_lossy().into_owned(),
    ];

    for path in &cases {
        let reference = run(&tclsh, &[path], "");
        let actual = run(&tclrs_bin, &[path], "");
        assert_eq!(
            actual, reference,
            "{path}: tclsh {reference:?} vs tclrs {actual:?}"
        );
    }
}

/// A script's exit status is 1 when it fails and 0 when it does not, whichever
/// binary runs it — the property the two suites above rest on, asserted on its
/// own so a regression names itself.
#[test]
fn exit_status_follows_the_script() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    for (program, expected) in [("puts ok", Some(0)), ("nosuchcmd", Some(1))] {
        let (reference, actual) = run_file(&tclsh, program, 2000);
        assert_eq!(reference.status, expected, "tclsh {program:?}");
        assert_eq!(actual.status, expected, "tclrs {program:?}");
    }
}

/// A nested script runs on a VM of its own, so nesting costs native stack and
/// a runaway `eval` would overflow it — a signal, not an error. It is refused
/// instead, at the depth the reference interpreter refuses it and with the same
/// message, which is what this pins: the boundary, from both sides.
#[test]
fn deep_eval_nesting_is_refused_like_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };

    let nested = |depth: usize| {
        let mut script = "set n ok".to_string();
        for _ in 0..depth {
            script = format!("eval {{{script}}}");
        }
        format!("{script}\nputs $n\n")
    };

    for (depth, index) in [(1000, 3000), (1001, 3001), (5000, 3002)] {
        let (reference, actual) = run_file(&tclsh, &nested(depth), index);
        assert_eq!(
            actual.stdout, reference.stdout,
            "depth {depth}: tclsh {reference:?} vs tclrs {actual:?}"
        );
        assert_eq!(
            actual.status, reference.status,
            "depth {depth}: tclsh {reference:?} vs tclrs {actual:?}"
        );
        assert_eq!(
            first_line(&actual.stderr),
            first_line(&reference.stderr),
            "depth {depth}"
        );
    }
}

// ── the driver's own surface ─────────────────────────────────────────────
//
// `-c` and `--version` have no `tclsh` equivalent — `tclsh` reads stdin for any
// argument beginning with `-` — so there is nothing to compare them against and
// they are pinned directly.

#[test]
fn dash_c_runs_the_script_it_is_given() {
    let tclrs_bin = PathBuf::from(TCLRS);
    let ran = run(&tclrs_bin, &["-c", "set x 6\nputs [expr {$x * 7}]"], "");
    assert_eq!(ran.stdout, "42\n");
    assert_eq!(ran.stderr, "");
    assert_eq!(ran.status, Some(0));

    let failed = run(&tclrs_bin, &["-c", "nosuchcmd"], "");
    assert_eq!(failed.stdout, "");
    assert_eq!(failed.stderr, "invalid command name \"nosuchcmd\"\n");
    assert_eq!(failed.status, Some(1));

    let missing = run(&tclrs_bin, &["-c"], "");
    assert_eq!(missing.stderr, "tclrs: -c requires a script\n");
    assert_eq!(missing.status, Some(1));
}

#[test]
fn version_prints_the_version_and_nothing_else() {
    let tclrs_bin = PathBuf::from(TCLRS);
    let run = run(&tclrs_bin, &["--version"], "");
    assert_eq!(run.stdout, format!("tclrs {}\n", env!("CARGO_PKG_VERSION")));
    assert_eq!(run.stderr, "");
    assert_eq!(run.status, Some(0));
}

/// An option the driver does not know is refused. `tclsh` would read stdin
/// instead; guessing at an unknown option is the one place this binary chooses
/// to differ, and it does so loudly.
#[test]
fn an_unknown_option_is_refused() {
    let tclrs_bin = PathBuf::from(TCLRS);
    let run = run(&tclrs_bin, &["--wat"], "puts unreached\n");
    assert_eq!(run.stdout, "");
    assert_eq!(run.stderr, "tclrs: unknown option \"--wat\"\n");
    assert_eq!(run.status, Some(1));
}

/// Nothing is printed that the script did not print: no banner, no prompt when
/// stdin is not a terminal, no trailing blank line at end of input.
#[test]
fn the_driver_prints_nothing_of_its_own() {
    let tclrs_bin = PathBuf::from(TCLRS);
    for (args, stdin) in [
        (vec!["-c", "puts x"], ""),
        (vec![], "puts x\n"),
        (vec![], ""),
    ] {
        let run = run(&tclrs_bin, &args, stdin);
        assert!(
            run.stdout.is_empty() || run.stdout == "x\n",
            "{args:?} printed {:?}",
            run.stdout
        );
        assert_eq!(run.stderr, "", "{args:?} wrote to stderr");
    }
}

/// The dumps answer about the script rather than running it: nothing the script
/// would have printed appears, and the script's own text does.
#[test]
fn a_dump_describes_the_script_instead_of_running_it() {
    let tclrs_bin = PathBuf::from(TCLRS);
    for flag in ["--dump-tokens", "--dump-ast"] {
        let run = run(&tclrs_bin, &[flag, "-c", "puts \"x is $x\""], "");
        assert_eq!(run.status, Some(0), "{flag} exited {:?}", run.status);
        assert_eq!(run.stderr, "", "{flag} wrote to stderr");
        assert!(!run.stdout.contains("x is 5"), "{flag} ran the script");
        // The `$x` was read as a substitution, and the dump says so.
        assert!(run.stdout.contains("var"), "{flag} printed {:?}", run.stdout);
    }
}

/// A script that does not parse is refused by a dump the way it is refused by a
/// run: the parser's message on stderr, nothing on stdout, exit 1.
#[test]
fn a_dump_refuses_a_script_that_does_not_parse() {
    let tclrs_bin = PathBuf::from(TCLRS);
    let run = run(&tclrs_bin, &["--dump-ast", "-c", "puts {unclosed"], "");
    assert_eq!(run.stdout, "");
    assert!(run.stderr.contains("missing close-brace"), "{:?}", run.stderr);
    assert_eq!(run.status, Some(1));
}
