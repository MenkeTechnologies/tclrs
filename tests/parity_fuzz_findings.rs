//! Minimised findings from the differential fuzzer (`scripts/fuzz_parity.sh`).
//!
//! Every case here came out of the fuzzer's shrinker and has been reduced to one
//! statement. Nothing is asserted from memory: tclsh is run on the same source
//! in the same process tree and *its* answer is the expectation, so a test that
//! disagrees with the reference interpreter cannot be written by accident.
//!
//! The file has two halves, and they mean different things:
//!
//! * **`deviations`** — the divergences the harness allowlists. Each test pins
//!   the deviation as it is, so an allowlist entry cannot outlive the behavior it
//!   excuses: fix the behavior and the test fails, which is the prompt to delete
//!   the entry from `scripts/fuzz/classify.pl` and the row from the report.
//! * **`bugs`** — divergences that are not documented anywhere and are, on the
//!   reading of `expr(n)` and `Tcl(n)` given in each test, tclrs bugs. These pin
//!   what tclrs does today. Fixing one turns its test into a plain equality
//!   assertion, and the BUGS.md entry moves from open to fixed.
//!
//! Where a finding is about the *driver* — an exit status, a location line —
//! the test runs the binary. Everything else goes through the library, which is
//! cheaper and reports the same message.

use std::path::PathBuf;
use std::process::Command;

const TCLRS: &str = env!("CARGO_BIN_EXE_tclrs");

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

/// What one engine did with a script: its stdout, and the first line of its
/// error, which is the part both engines word the same way when they agree.
#[derive(Debug, PartialEq, Eq)]
struct Observed {
    stdout: String,
    error: String,
}

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tclrs-findings-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

/// A file name of this program's own. The tests run in parallel threads of one
/// process, so a shared name would let one case read another's script.
fn case_path(program: &str, label: &str) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    program.hash(&mut h);
    scratch().join(format!("{label}-{:016x}.tcl", h.finish()))
}

/// Run `program` under tclsh. The expectation of every test in this file.
fn reference(tclsh: &PathBuf, program: &str) -> Observed {
    let path = case_path(program, "case");
    std::fs::write(&path, program).expect("write case");
    let out = Command::new(tclsh).arg(&path).output().expect("run tclsh");
    Observed {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        error: String::from_utf8_lossy(&out.stderr)
            .lines()
            .next()
            .unwrap_or("")
            .to_string(),
    }
}

/// Run `program` under tclrs, through the library.
///
/// A message located while compiling carries ` (line N)` when it is formatted
/// (`src/runtime.rs`, `TclError::fmt`); the binary prints the location on its
/// own line instead. The suffix is dropped so both paths report the same string
/// and a test does not depend on which one it went through.
fn subject(program: &str) -> Observed {
    let (result, stdout) = tclrs::eval_captured(program);
    let error = match result {
        Ok(_) => String::new(),
        Err(e) => {
            let mut msg = e;
            if let Some(i) = msg.rfind(" (line ") {
                if msg.ends_with(')') {
                    msg.truncate(i);
                }
            }
            msg
        }
    };
    Observed { stdout, error }
}

/// Assert that tclsh and tclrs disagree, and in exactly the recorded way.
///
/// Both halves are checked: `tclsh_says` against a live tclsh, so the reference
/// behavior in this file is never a guess, and `tclrs_says` against tclrs, so a
/// fix is caught rather than silently accepted. The two must also actually
/// differ — a "finding" whose two sides are equal is not a finding.
fn diverges(tclsh: &PathBuf, program: &str, tclsh_says: Observed, tclrs_says: Observed) {
    assert_ne!(
        tclsh_says, tclrs_says,
        "the recorded halves of {program:?} are equal, so this is not a divergence"
    );
    assert_eq!(
        reference(tclsh, program),
        tclsh_says,
        "tclsh no longer behaves as recorded for {program:?} — the reference \
         interpreter changed, so this finding needs re-measuring"
    );
    assert_eq!(
        subject(program),
        tclrs_says,
        "tclrs no longer behaves as recorded for {program:?} — if that is a fix, \
         turn this into an equality assertion and update BUGS.md"
    );
}

fn out(stdout: &str) -> Observed {
    Observed {
        stdout: stdout.to_string(),
        error: String::new(),
    }
}

fn err(error: &str) -> Observed {
    Observed {
        stdout: String::new(),
        error: error.to_string(),
    }
}

// ── deviations the harness allowlists ───────────────────────────────────────

/// A1: reading a variable that was never set. tclrs collapses no-such-variable,
/// unset element and empty into one `Undef` (`src/assoc.rs`), so the read
/// succeeds with the empty string where tclsh raises an error.
#[test]
fn deviation_unset_variable_reads_as_empty() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    diverges(
        &tclsh,
        "puts <$nosuchvar>",
        err("can't read \"nosuchvar\": no such variable"),
        out("<>\n"),
    );
    // Through `catch`, the same deviation is visible without either engine
    // failing: tclsh's catch has a message to report and tclrs's has none.
    diverges(
        &tclsh,
        "catch {set x $nosuchvar} m\nputs [string length $m]",
        out("40\n"),
        out("0\n"),
    );
}

/// A4: arity is resolved while compiling, so nothing runs at all — where tclsh
/// reaches the call, having already run everything before it.
#[test]
fn deviation_arity_is_reported_before_anything_runs() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    let program = "proc f {} {puts body}\nf\nf 1\n";
    diverges(
        &tclsh,
        program,
        Observed {
            stdout: "body\n".to_string(),
            error: "wrong # args: should be \"f\"".to_string(),
        },
        err("wrong # args: should be \"f\""),
    );
}

/// A2: an unterminated brace is reported where the input ran out rather than
/// where the brace opened. The message agrees; the line does not.
#[test]
fn deviation_unterminated_brace_reports_the_last_line() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    let program = "set x {\nputs a\nputs b\n";
    let path = case_path(program, "brace");
    std::fs::write(&path, program).expect("write case");

    let reference = Command::new(&tclsh).arg(&path).output().expect("run tclsh");
    let actual = Command::new(TCLRS).arg(&path).output().expect("run tclrs");
    let rerr = String::from_utf8_lossy(&reference.stderr).into_owned();
    let serr = String::from_utf8_lossy(&actual.stderr).into_owned();

    assert_eq!(rerr.lines().next(), Some("missing close-brace"));
    assert_eq!(serr.lines().next(), Some("missing close-brace"));
    // tclsh locates the brace that opened, on line 1. tclrs locates where the
    // input ran out — one past the three lines of the script.
    assert!(
        rerr.contains("line 1"),
        "tclsh no longer reports the opening line: {rerr:?}"
    );
    assert!(
        serr.contains("line 4"),
        "tclrs no longer reports the end of the input: {serr:?}"
    );
}

// ── bugs ────────────────────────────────────────────────────────────────────

/// `expr` coerces a non-numeric string to zero in every context that wants a
/// number, where `expr(n)` requires an error: "cannot use non-numeric string".
/// This is the widest finding of the run — the boolean cases change which branch
/// runs, so a script that tclsh refuses to run silently takes a path here.
#[test]
fn bug_non_numeric_strings_are_coerced_instead_of_refused() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    // Boolean contexts: `if`, `while` and `?:` all take the string as true.
    diverges(
        &tclsh,
        "if {\"b\"} {puts taken}",
        err("expected boolean value but got \"b\""),
        out("taken\n"),
    );
    diverges(
        &tclsh,
        "while {\"b\"} {puts once; break}",
        err("expected boolean value but got \"b\""),
        out("once\n"),
    );
    diverges(
        &tclsh,
        "puts [expr {\"b\" ? 1 : 2}]",
        err("expected boolean value but got \"b\""),
        out("1\n"),
    );
    // Bitwise and shift contexts: the string becomes 0.
    diverges(
        &tclsh,
        "puts [expr {\"b\" >> 1}]",
        err("cannot use non-numeric string \"b\" as left operand of \">>\""),
        out("0\n"),
    );
    diverges(
        &tclsh,
        "puts [expr {\"b\" & 1}]",
        err("cannot use non-numeric string \"b\" as left operand of \"&\""),
        out("0\n"),
    );
    diverges(
        &tclsh,
        "puts [expr {\"b\" | 1}]",
        err("cannot use non-numeric string \"b\" as left operand of \"|\""),
        out("1\n"),
    );
    diverges(
        &tclsh,
        "puts [expr {~\"b\"}]",
        err("cannot use non-numeric string \"b\" as operand of \"~\""),
        out("-1\n"),
    );
    diverges(
        &tclsh,
        "puts [expr {!\"b\"}]",
        err("cannot use non-numeric string \"b\" as operand of \"!\""),
        out("0\n"),
    );
}

/// A literal word whose value is an integral decimal loses its spelling: it is
/// interned as a number and printed from that, so `puts 3.0` prints `3`. Tcl's
/// first rule is that a value's string representation is what the script wrote —
/// `expr {3.0}` does print `3.0` here, so it is the literal path, and `--disasm`
/// shows the word reaching `LoadConst`.
///
/// The smallest divergence the fuzzer found, and the only one that silently
/// changes a value rather than a message.
#[test]
fn bug_integral_decimal_literals_lose_their_spelling() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    diverges(&tclsh, "puts 3.0", out("3.0\n"), out("3\n"));
    diverges(&tclsh, "puts 1.0", out("1.0\n"), out("1\n"));
    diverges(&tclsh, "puts 0.0", out("0.0\n"), out("0\n"));
    diverges(&tclsh, "set x 3.0\nputs s=$x", out("s=3.0\n"), out("s=3\n"));
    diverges(
        &tclsh,
        "set x -0.0\nputs v=$x",
        out("v=-0.0\n"),
        out("v=-0\n"),
    );
    // A fractional part that is not zero keeps its spelling, which is why the
    // existing suites — `set x 1.10` — did not catch this.
    let same = "set x 1.10\nputs a=$x";
    assert_eq!(reference(&tclsh, same), subject(same));
    let same = "puts 2.50";
    assert_eq!(reference(&tclsh, same), subject(same));
    // Neither does an exponent form, or a leading zero.
    for same in ["puts 3.0e0", "puts 1e3", "puts 007.0"] {
        assert_eq!(reference(&tclsh, same), subject(same), "{same}");
    }
}

/// The integer literal `-0` reaches `format`'s floating-point conversions with its
/// sign. tclsh converts the integer first, which loses the sign, so it prints a
/// positive zero. The double `-0.0` keeps its sign in both.
#[test]
fn bug_format_keeps_the_sign_of_integer_negative_zero() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    diverges(
        &tclsh,
        "puts [format %.2f -0]",
        out("0.00\n"),
        out("-0.00\n"),
    );
    diverges(
        &tclsh,
        "puts [format %e -0]",
        out("0.000000e+00\n"),
        out("-0.000000e+00\n"),
    );
    diverges(&tclsh, "puts [format %g -0]", out("0\n"), out("-0\n"));
    // The double keeps its sign under both, and `%d` drops it under both.
    for same in ["puts [format %.2f -0.0]", "puts [format %d -0]"] {
        assert_eq!(reference(&tclsh, same), subject(same), "{same}");
    }
}

/// `incr` on a value that is not an integer reports an `expr` operand error
/// rather than `incr`'s own diagnostic.
#[test]
fn bug_incr_reports_an_expr_error_instead_of_its_own() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    diverges(
        &tclsh,
        "set x 5\nincr x abc",
        err("expected integer but got \"abc\""),
        err("can't use non-numeric string as operand of \"+\": \"abc\""),
    );
    diverges(
        &tclsh,
        "set x abc\nincr x",
        err("expected integer but got \"abc\""),
        err("can't use non-numeric string as operand of \"+\": \"abc\""),
    );
}

/// `format`'s integer conversions name a list as a list. tclrs quotes the value
/// instead, which is the wording for a non-list.
#[test]
fn bug_format_reports_a_list_as_a_quoted_string() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    diverges(
        &tclsh,
        "puts [format %+d {{a b} c}]",
        err("expected integer but got a list"),
        err("expected integer but got \"{a b} c\""),
    );
}

/// Where tclrs does refuse a non-numeric or floating-point operand, it words the
/// refusal as Tcl 8 did. tclsh 9.0.4 names the offending value and which side of
/// the operator it was on.
#[test]
fn bug_expr_operand_errors_use_the_older_wording() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    diverges(
        &tclsh,
        "puts [expr {1 + \"abc\"}]",
        err("cannot use non-numeric string \"abc\" as right operand of \"+\""),
        err("can't use non-numeric string as operand of \"+\": \"abc\""),
    );
    diverges(
        &tclsh,
        "puts [expr {\"10\" - \"b\"}]",
        err("cannot use non-numeric string \"b\" as right operand of \"-\""),
        err("can't use non-numeric string as operand of \"-\": \"b\""),
    );
    diverges(
        &tclsh,
        "puts [expr {1.0 % 2}]",
        err("cannot use floating-point value \"1.0\" as left operand of \"%\""),
        err("can't use floating-point value as operand of \"%\""),
    );
}

/// `**` with two integer operands and a negative exponent. `expr(n)` keeps the
/// result integral for integral operands, which truncates to zero; tclrs returns
/// the floating-point power. README [0x04] claims the integral rule.
#[test]
fn bug_integer_exponentiation_with_a_negative_exponent_returns_a_double() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    diverges(&tclsh, "puts [expr {2 ** -1}]", out("0\n"), out("0.5\n"));
    diverges(&tclsh, "puts [expr {2 ** -3}]", out("0\n"), out("0.125\n"));
    diverges(
        &tclsh,
        "puts [expr {(-2) ** -65536}]",
        out("0\n"),
        out("0.0\n"),
    );
}

/// An integer literal too large for an `i64` becomes a double instead of the
/// documented `integer value too large to represent` (BUGS.md, README [0x05]),
/// so the answer is silently wrong rather than refused — and an operator with no
/// floating-point meaning then reports the wrong error.
#[test]
fn bug_out_of_range_integer_literals_become_doubles() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    diverges(
        &tclsh,
        "puts [expr {99999999999999999999 + 1}]",
        out("100000000000000000000\n"),
        out("1e+20\n"),
    );
    diverges(
        &tclsh,
        "puts [expr {99999999999999999999 % 3}]",
        out("0\n"),
        err("can't use floating-point value as operand of \"%\""),
    );
}

/// A character `expr` cannot use is reported as the first *byte* of its UTF-8
/// encoding: `Ã` is 0xC3, the lead byte of `Ü`. tclsh names the character.
#[test]
fn bug_non_ascii_in_an_expression_is_reported_by_byte() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    diverges(
        &tclsh,
        "puts [expr {Ü}]",
        err("invalid character \"Ü\""),
        err("unexpected character 'Ã' in expression"),
    );
    diverges(
        &tclsh,
        "puts [expr {1 + αβγ}]",
        err("invalid character \"α\""),
        err("unexpected character 'Î' in expression"),
    );
}

/// tclrs lowers a whole script before running any of it, so a compile-time error
/// inside code that never executes stops a script tclsh runs to completion. The
/// mechanism is documented (README [0x05]: "at compile time where the script's
/// shape decides it"); this consequence — a working script refused — is not.
#[test]
fn bug_unreachable_code_is_still_compiled() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    // An arity error in a branch that is never taken.
    diverges(
        &tclsh,
        "if {0} {incr}\nputs reached",
        out("reached\n"),
        err("wrong # args: should be \"incr varName ?increment?\""),
    );
    // An expression that cannot be parsed, in a branch that is never taken.
    diverges(
        &tclsh,
        "if {0} {puts [expr {1 +}]}\nputs reached",
        out("reached\n"),
        err("premature end of expression"),
    );
    // A command that does not exist, in a branch that is never taken. tclsh
    // resolves command names when it reaches them.
    diverges(
        &tclsh,
        "if {0} {nosuchcommand}\nputs reached",
        out("reached\n"),
        err("invalid command name \"nosuchcommand\""),
    );
    // A `switch` arm that is never selected. Its body is a braced word, so
    // nothing in it is even parsed until tclsh picks the arm — this is the
    // largest single signature in a fuzz run.
    diverges(
        &tclsh,
        "switch -- x {*b {puts \"a}}\nputs reached",
        out("reached\n"),
        err("missing \""),
    );
}

/// A compile-time error inside a *nested body* is located at line 1, whatever
/// line the body is on. At a script's own top level the line is right, which is
/// what makes this a body-scoped bug rather than a missing line counter.
#[test]
fn bug_compile_time_errors_inside_a_body_are_located_at_line_one() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    let location = |program: &str| -> String {
        let path = case_path(program, "loc");
        std::fs::write(&path, program).expect("write case");
        let out = Command::new(TCLRS).arg(&path).output().expect("run tclrs");
        String::from_utf8_lossy(&out.stderr)
            .lines()
            .find_map(|l| {
                l.rfind("line ")
                    .map(|i| l[i..].trim_end_matches(')').to_string())
            })
            .unwrap_or_default()
    };
    let reference_location = |program: &str| -> String {
        let path = case_path(program, "loc-ref");
        std::fs::write(&path, program).expect("write case");
        let out = Command::new(&tclsh).arg(&path).output().expect("run tclsh");
        String::from_utf8_lossy(&out.stderr)
            .lines()
            .filter_map(|l| {
                l.rfind("line ")
                    .map(|i| l[i..].trim_end_matches(')').to_string())
            })
            .next_back()
            .unwrap_or_default()
    };

    // The call is on line 3, inside a body. tclsh reports line 3.
    let nested = "proc f {a} {return $a}\nputs one\nif {1} {f}\n";
    assert_eq!(reference_location(nested), "line 3");
    assert_eq!(
        location(nested),
        "line 1",
        "tclrs no longer reports line 1 for a failure inside a body — if it now \
         reports line 3 this is fixed"
    );

    // The same call at the script's top level is located correctly, so the line
    // counter itself works.
    let flat = "proc f {a} {return $a}\nputs one\nf\n";
    assert_eq!(reference_location(flat), "line 3");
    assert_eq!(location(flat), "line 3");
}

/// Nesting depth is bounded only by the stack, in both implementations. Run as
/// subprocesses because a stack overflow aborts the process rather than
/// panicking, so it cannot be caught in-process.
///
/// Found by the `parse` cargo-fuzz target rather than by the differential
/// fuzzer: the generator only writes programs, and no program has fifty thousand
/// open brackets.
///
/// The two limits are far apart, and tclsh's is the lower one — which is why this
/// input is not a case the differential harness can charge against tclrs at
/// 50_000 (tclsh has no answer to compare with) and is a `CRITICAL` at 100_000
/// (tclrs aborts, and that is never suppressible).
#[test]
fn bug_deep_nesting_exhausts_the_stack_in_both_engines() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    // `code()` is `None` for a process killed by a signal, which is how both a
    // segfault and a Rust stack-overflow abort arrive here.
    let run = |binary: &PathBuf, depth: usize| -> (Option<i32>, String) {
        let program = "[".repeat(depth);
        let path = case_path(&format!("depth-{depth}"), "nest");
        std::fs::write(&path, &program).expect("write case");
        let out = Command::new(binary).arg(&path).output().expect("run");
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .next()
                .unwrap_or("")
                .to_string(),
        )
    };
    let subject = PathBuf::from(TCLRS);

    // 10_000 levels: both report the parse error.
    assert_eq!(run(&tclsh, 10_000).1, "missing close-bracket");
    assert_eq!(run(&subject, 10_000).1, "missing close-bracket");

    // 50_000: tclsh dies on a signal, tclrs still reports. The harness calls
    // this EXCLUDED — there is no reference behavior to compare against.
    let (status, _) = run(&tclsh, 50_000);
    assert!(
        status.is_none(),
        "tclsh no longer dies on a signal at 50_000 levels (exit {status:?}) — if \
         it now reports an error, this case becomes a plain parity comparison"
    );
    assert_eq!(
        run(&subject, 50_000),
        (Some(1), "missing close-bracket".to_string())
    );

    // 100_000: tclrs aborts too. On the driver's own thread, which is
    // RECOMMENDED_STACK, so this is not a small-stack artifact.
    let (status, _) = run(&subject, 100_000);
    assert!(
        status.is_none(),
        "tclrs no longer aborts at 100_000 levels (exit {status:?}) — if the \
         parser now bounds its recursion, this is fixed"
    );
}
