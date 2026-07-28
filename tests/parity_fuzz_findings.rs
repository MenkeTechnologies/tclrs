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

/// Assert that tclsh and tclrs now agree on `program` — the shape a finding
/// takes once it is fixed. tclsh is still run, so the expectation is still the
/// reference interpreter's own answer and not a hand-written one; `expected` is
/// checked against it as well, so a test cannot pass by both engines being
/// wrong in the same new way.
fn agrees(tclsh: &PathBuf, program: &str, expected: Observed) {
    let reference = reference(tclsh, program);
    assert_eq!(
        reference, expected,
        "tclsh no longer behaves as recorded for {program:?} — the reference \
         interpreter changed, so this expectation needs re-measuring"
    );
    assert_eq!(subject(program), reference, "{program:?}");
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

/// **Fixed.** A condition has to be a Tcl boolean, and `if {"b"}` is
/// `expected boolean value but got "b"` rather than a taken branch.
///
/// The rule is `ParseBoolean` plus `TclParseNumber` (`tclObj.c`), ported in
/// `runtime::tcl_bool` and reached through `ext::BOOL`: a number in any radix, or
/// one of `true` / `false` / `yes` / `no` / `on` / `off` abbreviated to any
/// unambiguous prefix, in any case. `o` is ambiguous between `on` and `off`, so
/// it is not one.
///
/// The conversion is only emitted where the value could be a string — a
/// relational or arithmetic condition already produces a number — because it is
/// an extension op, and one of those inside a loop body would cost the loop
/// fusevm's tracing tier. `tiers::tests` pins that it does not.
#[test]
fn conditions_are_tcl_booleans_not_the_vms_truthiness() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    // The reproducers, in each of the contexts that branch on a value.
    agrees(
        &tclsh,
        "if {\"b\"} {puts taken}",
        err("expected boolean value but got \"b\""),
    );
    agrees(
        &tclsh,
        "while {\"b\"} {puts once; break}",
        err("expected boolean value but got \"b\""),
    );
    agrees(
        &tclsh,
        "for {} {\"b\"} {} {puts once}",
        err("expected boolean value but got \"b\""),
    );
    agrees(
        &tclsh,
        "puts [expr {\"b\" ? 1 : 2}]",
        err("expected boolean value but got \"b\""),
    );
    agrees(
        &tclsh,
        "puts [expr {\"b\" && 1}]",
        err("expected boolean value but got \"b\""),
    );
    agrees(
        &tclsh,
        "puts [expr {1 && \"b\"}]",
        err("expected boolean value but got \"b\""),
    );
    agrees(
        &tclsh,
        "puts [expr {0 || \"b\"}]",
        err("expected boolean value but got \"b\""),
    );
    // The word table, its abbreviations and its one ambiguity.
    for program in [
        "if {\"true\"} {puts t}",
        "if {\"tRuE\"} {puts t}",
        "if {\"t\"} {puts t}",
        "if {\"fals\"} {puts t} else {puts f}",
        "if {\"y\"} {puts t}",
        "if {\"n\"} {puts t} else {puts f}",
        "if {\"on\"} {puts t}",
        "if {\"of\"} {puts t} else {puts f}",
        "if {\"o\"} {puts t}",
        "puts [expr {\"yes\" && \"no\"}]",
        "puts [expr {!\"true\"}]",
        "puts [expr {!\"off\"}]",
    ] {
        assert_eq!(reference(&tclsh, program), subject(program), "{program}");
    }
    // Numbers in a boolean position, in every spelling the number parser takes,
    // and the strings that are neither a word nor a number.
    for value in [
        "0",
        "1",
        "2",
        "-1",
        "007",
        "010",
        "0x10",
        "0o17",
        "0b101",
        "0d9",
        "1_0",
        "1_000_000",
        "0.0",
        "-0.0",
        "1.5",
        "1e3",
        " 1 ",
        "inf",
        "",
        " ",
        "b",
        "abc",
        "a b",
        "1x",
        "_1",
        "1_",
        "0x_10",
        "099",
        "0.",
    ] {
        // Braced, so an empty value is still an assignment rather than a read.
        let program = format!("set x {{{value}}}\nif {{$x}} {{puts t}} else {{puts f}}");
        assert_eq!(reference(&tclsh, &program), subject(&program), "{program}");
    }
    // Only the operand that is evaluated is held to the rule: the left one
    // decided these, so the right one is never read.
    for program in [
        "puts [expr {0 && \"b\"}]",
        "puts [expr {1 || \"b\"}]",
        "puts [expr {0 && [error nope]}]",
    ] {
        assert_eq!(reference(&tclsh, program), subject(program), "{program}");
    }
}

/// The other half of the same coercion, still open: outside a boolean position
/// `expr` takes a non-numeric string as zero. What tclsh reports there is the
/// operand wording of `bug_expr_operand_errors_use_the_older_wording`, and `!`
/// now joins that class — it refuses the operand rather than answering 0, in
/// tclrs's own wording for a refused operand.
#[test]
fn bug_non_numeric_strings_are_coerced_outside_a_boolean_position() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
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
    // `!` no longer answers 0; only the wording of the refusal differs now.
    diverges(
        &tclsh,
        "puts [expr {!\"b\"}]",
        err("cannot use non-numeric string \"b\" as operand of \"!\""),
        err("can't use non-numeric string as operand of \"!\": \"b\""),
    );
}

/// **Fixed.** A literal keeps the spelling the script wrote: `puts 3.0` prints
/// `3.0`.
///
/// The cause was `compiler::literal_value` interning a literal as a
/// `Value::Float` when the canonical spelling round-tripped. A `Value::Float`
/// reaching `puts` is stringified by fusevm's `as_str_cow`, not by Tcl's
/// formatter — only an `expr` result passes through the `NORM` extension op that
/// applies that — so the double `3.0` printed as `3`. No literal is interned as a
/// `Float` now; an integer still is, because `i64::to_string` *is* the spelling
/// Tcl prints.
#[test]
fn literals_keep_the_spelling_the_script_wrote() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    agrees(&tclsh, "puts 3.0", out("3.0\n"));
    agrees(&tclsh, "puts 1.0", out("1.0\n"));
    agrees(&tclsh, "puts 0.0", out("0.0\n"));
    agrees(&tclsh, "set x 3.0\nputs s=$x", out("s=3.0\n"));
    agrees(&tclsh, "set x -0.0\nputs v=$x", out("v=-0.0\n"));
    // The spellings that always survived, kept as a guard against a fix that
    // trades one direction for the other.
    for same in [
        "set x 1.10\nputs a=$x",
        "puts 2.50",
        "puts 3.0e0",
        "puts 1e3",
        "puts 007.0",
        "puts 5",
        "puts 007",
        "puts -0",
    ] {
        assert_eq!(reference(&tclsh, same), subject(same), "{same}");
    }
    // A double that reaches a string by any other route is formatted by Tcl's
    // rules, which is what the literal path was bypassing.
    for same in [
        "puts [expr {3.0}]",
        "puts [expr {1.5 + 1.5}]",
        "set x 2.5\nputs [expr {$x * 2}]",
        "set x 3.0\nputs [string length $x]",
        "set x 3.0\nputs [list $x]",
        "set x 3.0\nappend x !\nputs $x",
        "puts [lindex {3.0 4.0} 0]",
        "foreach v {3.0 1.0} {puts $v}",
        "set a(k) 3.0\nputs $a(k)",
        "puts [format %s 3.0]",
        "switch -- 3.0 {3.0 {puts hit} default {puts miss}}",
    ] {
        assert_eq!(reference(&tclsh, same), subject(same), "{same}");
    }
}

/// **Fixed.** `format`'s floating-point conversions convert an integer spelling
/// as an integer, and an integer has no negative zero: `format %.2f -0` is
/// `0.00`. The double `-0.0` keeps its sign, in both engines.
#[test]
fn format_drops_the_sign_of_integer_negative_zero() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    agrees(&tclsh, "puts [format %.2f -0]", out("0.00\n"));
    agrees(&tclsh, "puts [format %e -0]", out("0.000000e+00\n"));
    agrees(&tclsh, "puts [format %g -0]", out("0\n"));
    // Every other spelling of an integer zero, and the doubles that do keep the
    // sign — the boundary the fix has to land on.
    for same in [
        "puts [format %.2f -00]",
        "puts [format %.2f -0x0]",
        "puts [format %.2f -0_0]",
        "puts [format %.2f { -0 }]",
        "puts [format %.2f -0.0]",
        "puts [format %.2f -0e0]",
        "puts [format %.2f -1e-400]",
        "puts [format %d -0]",
        "puts [format %.2f -5]",
    ] {
        assert_eq!(reference(&tclsh, same), subject(same), "{same}");
    }
}

/// **Fixed for an increment the script wrote.** `incr x abc` is
/// `expected integer but got "abc"`, checked while compiling, where the
/// increment is a literal word.
#[test]
fn incr_reports_its_own_diagnostic_for_a_literal_increment() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    agrees(
        &tclsh,
        "set x 5\nincr x abc",
        err("expected integer but got \"abc\""),
    );
    agrees(
        &tclsh,
        "set x 5\nincr x 1.0",
        err("expected integer but got \"1.0\""),
    );
    agrees(
        &tclsh,
        "set x 5\nincr x {}",
        err("expected integer but got \"\""),
    );
    // A value whose text could be several list elements is named as a list, and
    // one that merely contains a space still is — the looser of the reference
    // implementation's two list screens (`list::looks_like_a_list`).
    agrees(
        &tclsh,
        "set x 5\nincr x {a b}",
        err("expected integer but got a list"),
    );
    agrees(
        &tclsh,
        "set x 5\nincr x a\\ b",
        err("expected integer but got a list"),
    );
    // The increments that are integers keep working, in every spelling.
    for same in [
        "set x 5\nincr x\nputs $x",
        "set x 5\nincr x -3\nputs $x",
        "set x 5\nincr x +3\nputs $x",
        "set x 5\nincr x 0x10\nputs $x",
        "set x 5\nincr x { 3 }\nputs $x",
        "set x 5\nincr x 1_0\nputs $x",
        "set a(k) 5\nincr a(k) abc",
    ] {
        assert_eq!(reference(&tclsh, same), subject(same), "{same}");
    }
}

/// The rest of the same finding, still open: when it is the *variable* that does
/// not hold an integer, the refusal comes from the numeric hook behind
/// `Op::Add`, in `expr`'s wording.
///
/// The fix would be an extension op in `incr`'s lowering, and fusevm's tracing
/// tier rejects `Op::Extended` inside a loop body — so it would take the
/// compiled trace away from every loop that counts with `incr`, which
/// `tiers::tests::a_proc_local_counter_loop_reaches_a_compiled_trace` and
/// `bench/counted_loop_proc.tcl` both depend on. Not taken; recorded instead.
#[test]
fn bug_incr_reports_an_expr_error_for_a_non_integer_variable() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    diverges(
        &tclsh,
        "set x abc\nincr x",
        err("expected integer but got \"abc\""),
        err("can't use non-numeric string as operand of \"+\": \"abc\""),
    );
    diverges(
        &tclsh,
        "set x 5\nset y abc\nincr x $y",
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

/// **Fixed.** `**` keeps an integral result for integral operands even when the
/// exponent is negative, so the true value truncates toward zero: `2 ** -1` is 0.
/// Only ±1 survives, and a zero base has no value at all there.
#[test]
fn integer_exponentiation_stays_integral_for_a_negative_exponent() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    agrees(&tclsh, "puts [expr {2 ** -1}]", out("0\n"));
    agrees(&tclsh, "puts [expr {2 ** -3}]", out("0\n"));
    agrees(&tclsh, "puts [expr {(-2) ** -65536}]", out("0\n"));
    agrees(&tclsh, "puts [expr {1 ** -100}]", out("1\n"));
    agrees(&tclsh, "puts [expr {(-1) ** -101}]", out("-1\n"));
    agrees(&tclsh, "puts [expr {(-1) ** -100}]", out("1\n"));
    agrees(
        &tclsh,
        "puts [expr {0 ** -1}]",
        err("exponentiation of zero by negative power"),
    );
    // A double operand anywhere makes it a floating-point power again, and the
    // exponent's own sign does not change that.
    for same in [
        "puts [expr {2.0 ** -1}]",
        "puts [expr {2 ** -1.0}]",
        "puts [expr {4 ** 0.5}]",
        "puts [expr {0.5 ** -1}]",
        "puts [expr {0.0 ** -1}]",
        "puts [expr {0 ** -1.5}]",
        "puts [expr {0 ** 0}]",
        "puts [expr {2 ** 62}]",
        // `i64::MIN` has no negated form, so the parity of the exponent has to be
        // read off its low bit. Written as a subtraction because the literal
        // `-9223372036854775808` is `-(9223372036854775808)`, and the positive
        // half of that is past `i64` — the bignum case, not this one.
        "puts [expr {2 ** (-9223372036854775807 - 1)}]",
        "puts [expr {(-1) ** (-9223372036854775807 - 1)}]",
    ] {
        assert_eq!(reference(&tclsh, same), subject(same), "{same}");
    }
}

/// **Fixed**, in the sense this frontend documents: an integer beyond `i64` is
/// now the refusal the crate promises rather than a silently wrong answer.
///
/// tclsh still differs, and will until there is a bignum — it promotes and
/// answers exactly. What changed is that tclrs reports
/// `integer value too large to represent` instead of switching to floating point
/// and answering `1e+20`, so the divergence is a refusal the harness counts as a
/// documented skip rather than a wrong value. Both halves of the cause are
/// closed: `expr::parse_number` refuses the literal, and `runtime::parse_number`
/// no longer hands an out-of-range integer spelling to the double parser.
#[test]
fn out_of_range_integers_are_refused_rather_than_becoming_doubles() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    let refused = || err("integer value too large to represent");
    diverges(
        &tclsh,
        "puts [expr {99999999999999999999 + 1}]",
        out("100000000000000000000\n"),
        refused(),
    );
    // The operator with no floating-point meaning now reports the overflow
    // rather than complaining about a double it was never given.
    diverges(
        &tclsh,
        "puts [expr {99999999999999999999 % 3}]",
        out("0\n"),
        refused(),
    );
    // Through a variable, which is the runtime half of the same cause.
    diverges(
        &tclsh,
        "set x 99999999999999999999\nputs [expr {$x + 1}]",
        out("100000000000000000000\n"),
        refused(),
    );
    // And in the radix spellings.
    diverges(
        &tclsh,
        "puts [expr {0x10000000000000000}]",
        out("18446744073709551616\n"),
        refused(),
    );
    // The `i64` ends themselves are not overflow, and still agree.
    for same in [
        "puts [expr {9223372036854775807 - 1}]",
        "puts [expr {-9223372036854775807 + 1}]",
        "puts [expr {0x7fffffffffffffff}]",
    ] {
        assert_eq!(reference(&tclsh, same), subject(same), "{same}");
    }
}

/// **Fixed.** A character `expr` cannot use is named as a character, in tclsh's
/// own wording: `invalid character "Ü"`, not the `Ã` that is the lead byte of its
/// UTF-8 encoding.
#[test]
fn an_unusable_character_in_an_expression_is_named_as_a_character() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    agrees(&tclsh, "puts [expr {Ü}]", err("invalid character \"Ü\""));
    agrees(
        &tclsh,
        "puts [expr {1 + αβγ}]",
        err("invalid character \"α\""),
    );
    // The same wording covers the ASCII characters, which were named correctly
    // before but with the other message.
    agrees(&tclsh, "puts [expr {@}]", err("invalid character \"@\""));
    for same in [
        "puts [expr {1 + @}]",
        "puts [expr {é}]",
        "puts [expr {日}]",
        "puts [expr {\u{1F600}}]",
    ] {
        assert_eq!(reference(&tclsh, same), subject(same), "{same}");
    }
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

/// **Fixed.** A failure inside a body is located at the script's own command,
/// which is the line tclsh's `(file "…" line N)` names.
///
/// A braced body is parsed as a script of its own, so its commands are numbered
/// from 1 relative to the body's text; the compiler used that number, so an error
/// inside `if {1} {f}` on line 3 was reported at line 1. tclsh reports the
/// *top-level command's* line there and gives the position inside the body as a
/// separate relative line (`("while" body line 2)`), which tclrs has no
/// equivalent of — so the fix is to stop a body's own numbering from moving the
/// reported line, not to make it absolute.
#[test]
fn compile_time_errors_are_located_at_the_scripts_own_command() {
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

    // Every shape is checked against tclsh's own location rather than against a
    // number written here: the command on one line with its body, a body that
    // spans lines so the failing line and the command's line differ, a body
    // reached through a line continuation, and the same call at the top level.
    for program in [
        "proc f {a} {return $a}\nputs one\nif {1} {f}\n",
        "proc f {a} {return $a}\nputs one\nif {1} {\n    f\n}\n",
        "proc f {a} {return $a}\nputs one\nif {1} \\\n   {f}\n",
        "proc f {a} {return $a}\nputs one\nf\n",
        "while {1} {\n   incr\n}\n",
        "\nif {1} {\n   nosuchcmd\n}\n",
        "puts a\nputs b\nputs c\nif {1} {puts [expr {1 +}]}\n",
        "puts a\nfor {set i 0} {$i < 1} {incr i} {\n  incr\n}\n",
    ] {
        assert_eq!(
            location(program),
            reference_location(program),
            "{program:?}"
        );
    }
}

/// **Fixed for tclrs.** Input nesting is bounded by
/// [`tclrs::parser::MAX_NESTING_DEPTH`] rather than by the stack, so the deepest
/// input reports a Tcl error instead of aborting the process. tclsh has no such
/// bound and still segfaults, well before the depth tclrs refuses at.
///
/// Found by the `parse` cargo-fuzz target rather than by the differential
/// fuzzer: the generator only writes programs, and no program has fifty thousand
/// open brackets.
///
/// Run as subprocesses because a stack overflow aborts the process rather than
/// panicking, so it cannot be caught in-process. The limit is above every depth
/// tclsh survives on purpose — nothing tclsh can parse becomes a refusal here.
#[test]
fn deep_nesting_is_refused_rather_than_exhausting_the_stack() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    // `code()` is `None` for a process killed by a signal, which is how both a
    // segfault and a Rust stack-overflow abort arrive here.
    let run = |binary: &PathBuf, program: &str, label: &str| -> (Option<i32>, String) {
        let path = case_path(label, "nest");
        std::fs::write(&path, program).expect("write case");
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
    let brackets = |depth: usize| "[".repeat(depth);
    let subject = PathBuf::from(TCLRS);
    let limit = tclrs::parser::MAX_NESTING_DEPTH;

    // 10_000 levels: both report the parse error, and that parity is what keeps
    // the limit from being set anywhere tclsh still works.
    assert_eq!(
        run(&tclsh, &brackets(10_000), "ref-10k").1,
        "missing close-bracket"
    );
    assert_eq!(
        run(&subject, &brackets(10_000), "sub-10k").1,
        "missing close-bracket"
    );

    // 50_000: tclsh dies on a signal, tclrs still parses. The harness calls this
    // EXCLUDED — there is no reference behavior to compare against.
    let (status, _) = run(&tclsh, &brackets(50_000), "ref-50k");
    assert!(
        status.is_none(),
        "tclsh no longer dies on a signal at 50_000 levels (exit {status:?}) — if \
         it now reports an error, this case becomes a plain parity comparison"
    );
    assert_eq!(
        run(&subject, &brackets(50_000), "sub-50k"),
        (Some(1), "missing close-bracket".to_string())
    );

    // Exactly at the limit still parses; one past it is the refusal, and so is
    // the 100_000 that used to abort.
    assert_eq!(
        run(&subject, &brackets(limit), "sub-limit"),
        (Some(1), "missing close-bracket".to_string())
    );
    let refusal = (
        Some(1),
        "too many nested substitutions (infinite loop?)".to_string(),
    );
    assert_eq!(run(&subject, &brackets(limit + 1), "sub-over"), refusal);
    assert_eq!(run(&subject, &brackets(100_000), "sub-100k"), refusal);

    // The other recursion in the parser is an array index, and it is bounded too.
    let indices = format!("puts $a({}", "$b(".repeat(100_000));
    assert_eq!(run(&subject, &indices, "sub-index"), refusal);
}
