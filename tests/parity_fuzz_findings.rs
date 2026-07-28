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
        // The non-ASCII values matter most here: the radix-prefix test used to
        // slice the first two *bytes* of the text, which is inside a character in
        // `héllo`, and a condition is the first place a value of any shape at all
        // reaches the number parser. That was a panic, not a divergence.
        "héllo",
        "日本語",
        "αβγ",
        "ÜñîçøðÉ",
        "naïve café",
        "é",
        "0é",
        "0éx",
        "0xé",
        "1é",
        "tab\there",
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
    // The `i64` ends themselves are not overflow, and still agree. So does the
    // decimal spelling *as a value*: it is exactly what tclsh prints for it, so
    // the literal is carried through and only an operation on it is refused —
    // a script that merely prints one, or never reaches it, still runs.
    for same in [
        "puts [expr {9223372036854775807 - 1}]",
        "puts [expr {-9223372036854775807 + 1}]",
        "puts [expr {0x7fffffffffffffff}]",
        "puts [expr {99999999999999999999}]",
        "puts 99999999999999999999",
        "set x 99999999999999999999\nputs $x",
        "if {99999999999999999999} {puts T}",
        "set x 9223372036854775808\nif {$x} {puts T}",
        "if {0} {puts [expr {99999999999999999999 + 1}]}\nputs reached",
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

/// **Fixed.** An index whose text holds a non-ASCII character reports
/// `bad index`, as tclsh does, instead of aborting the process.
///
/// Three sites sliced by byte offset into text a script supplies, and each
/// panicked when the offset landed inside a character: the radix-prefix test in
/// `runtime::parse_number` (`&body[..2]`, which a *condition* reaches with any
/// value at all — the crash the boolean rule above exposed), `cmd_string`'s
/// `end±n` split (`rest.split_at(1)`), and `list`'s (`&text[..3]`). A stack of
/// non-ASCII text is in the fuzzer's value pool, so this was one generated index
/// away from being a CRITICAL.
///
/// Run in-process: a panic in `subject` fails the test.
#[test]
fn a_non_ascii_index_reports_bad_index_rather_than_aborting() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    agrees(
        &tclsh,
        "puts [string index abc endé]",
        err("bad index \"endé\": must be integer?[+-]integer? or end?[+-]integer?"),
    );
    agrees(
        &tclsh,
        "puts [lindex {a b c} e€a]",
        err("bad index \"e€a\": must be integer?[+-]integer? or end?[+-]integer?"),
    );
    // Every index-taking command in both families, against a character at each
    // byte offset a slice could have landed on, and the well-formed indices
    // beside them so the fix cannot have been to reject everything.
    for index in [
        "endé", "end€", "end😀", "endé0", "end+é", "end-é", "e€a", "e€ab", "énd", "1é", "0xé",
        "end", "end-1", "end+1", "end-", "endx", "0", "2", "-1",
    ] {
        for program in [
            format!("puts [string index abcdef {index}]"),
            format!("puts [string range abcdef 0 {index}]"),
            format!("puts [string first a abcdef {index}]"),
            format!("puts [string insert abcdef {index} X]"),
            format!("puts [lindex {{a b c}} {index}]"),
            format!("puts [lrange {{a b c}} 0 {index}]"),
            format!("puts [linsert {{a b c}} {index} X]"),
            format!("puts [lreplace {{a b c}} 0 {index}]"),
            format!("puts [lsearch -start {index} {{a b c}} b]"),
        ] {
            assert_eq!(reference(&tclsh, &program), subject(&program), "{program}");
        }
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

// ── crashes found by the cargo-fuzz targets ─────────────────────────────────
//
// A crash is worse than any divergence: none of these can be caught by `catch`,
// because the interpreter thread unwinds or the process aborts before the
// script's own error handling is reached. Each was open in BUGS.md and each is
// fixed below; the tests are here rather than in a unit module because the
// answer that decided each policy is tclsh's, and it is measured rather than
// remembered.

/// **Fixed for tclrs.** A floating-point precision above 65_535 produces the
/// digits, where before it panicked.
///
/// Rust's formatter holds a precision in a `u16` and answers anything larger
/// with `Formatting argument out of range` — a panic, not an error — and
/// `format`'s precision comes straight from the script. tclsh produces every
/// digit asked for, so matching it means generating the digits rather than
/// bounding the precision.
///
/// The policy is to match tclsh exactly, which is possible because a double's
/// decimal expansion is finite: at most 1_074 fraction digits, for the smallest
/// subnormal. Every digit past the expansion is a zero, so formatting at the
/// highest precision Rust accepts and appending zeroes is exact, not an
/// approximation — and this test compares against a live tclsh rather than
/// against that reasoning.
#[test]
fn format_precision_past_the_formatters_ceiling_matches_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    // The lengths, because the answers run to tens of thousands of digits: the
    // four sites BUGS.md named, then the boundary from both sides, then the two
    // the `%g` path turns on — `#` keeps the trailing zeroes the plain form
    // strips, so it is the one that must still produce them.
    for (program, length) in [
        ("format %.65535f 1.0", 65537),
        ("format %.65536f 1.0", 65538),
        ("format %.65536e 1.0", 65542),
        ("format %.65535g 0.0001", 68),
        ("format %.70000g 1e-5", 70),
        ("format %#.70000g 1e-5", 70005),
        ("format %#.65540g 0.5", 65542),
        ("format %.1000000e 1e-5", 1000006),
        ("format %.65536f -0.0", 65539),
    ] {
        agrees(
            &tclsh,
            &format!("puts [string length [{program}]]"),
            out(&format!("{length}\n")),
        );
    }

    // The digits themselves where the answer is short enough to read. The first
    // is the whole exact decimal expansion of the double nearest 1e-5, which is
    // the claim the zero-extension rests on: 65_540 significant digits were
    // asked for and the expansion ran out after 65, so nothing was invented.
    agrees(
        &tclsh,
        "puts [format %.65540g 1e-5]",
        out("1.0000000000000000818030539140313095458623138256371021270751953125e-05\n"),
    );
    agrees(
        &tclsh,
        "puts [string range [format %#.65540g 0.5] 0 8]",
        out("0.5000000\n"),
    );
    agrees(
        &tclsh,
        "puts [string range [format %.65536f -0.0] 0 4]",
        out("-0.00\n"),
    );
    // Nothing here may disturb the ordinary precisions.
    agrees(
        &tclsh,
        "puts [format %.5f|%.3e|%g|%#.0f 1.0 12345.678 0.0001 2.0]",
        out("1.00000|1.235e+04|0.0001|2.\n"),
    );
}

/// **Fixed for tclrs.** A field width or a precision too large to allocate is a
/// Tcl error, where before it aborted the process.
///
/// `format %9223372036854775807d 1` asked the allocator for 9 exabytes, and its
/// refusal is an abort: `memory allocation of 9223372036854775806 bytes failed`,
/// which no `catch` can see. tclsh reports `max size for a Tcl value exceeded`
/// and keeps running, so the policy is tclsh's message, checked here against a
/// live tclsh rather than quoted from memory.
///
/// The size the limit sits at is *not* tclsh's. tclsh 9.0's `Tcl_Size` is
/// 64-bit and `format %4294967296d 1` really does build a 4 GiB string there;
/// tclrs refuses above 2 GiB, which is where `string repeat` already refuses
/// (`src/cmd_string.rs`, the `REPEAT` arm). Below that the two agree, and the
/// widths a script actually writes are far below it.
///
/// The subprocess run is the part that would have failed before the fix: an
/// abort has no exit code, so `code()` is `None` for it.
#[test]
fn format_size_is_refused_rather_than_aborting() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    for program in [
        // The field width, at every conversion that pads with it.
        "puts [format %9223372036854775807d 1]",
        "puts [format %9223372036854775807s x]",
        "puts [format %9223372036854775807c 65]",
        // The precision. BUGS.md named only the floating-point sites; an
        // integer conversion pads on the left and aborted the same way, from a
        // different line.
        "puts [format %.9223372036854775807d 1]",
        "puts [format %.9223372036854775807x 1]",
        "puts [format %.9223372036854775807b 1]",
        "puts [format %.9223372036854775807f 1e-5]",
        "puts [format %.9223372036854775807e 1.0]",
        "puts [format %#.9223372036854775807g 1e-5]",
        // `%g` strips trailing zeroes, so its digits could be produced at a
        // clamped precision and come out right — which is exactly how a
        // precision past any possible result would slip through unrefused.
        "puts [format %.9223372036854775807g 1e-5]",
        // A precision whose spelling does not fit an `i64` at all. It used to
        // read as zero and print `1`.
        "puts [format %.99999999999999999999d 1]",
        "puts [format %.99999999999999999999f 1]",
    ] {
        agrees(&tclsh, program, err("max size for a Tcl value exceeded"));

        // Run the binary too: the failure this replaces was an abort, which a
        // library call cannot report and a test cannot catch.
        let path = case_path(program, "fmtsize");
        std::fs::write(&path, program).expect("write case");
        let status = std::process::Command::new(TCLRS)
            .arg(&path)
            .output()
            .expect("run tclrs")
            .status;
        assert_eq!(
            status.code(),
            Some(1),
            "{program:?} did not exit with a Tcl error — a process killed by a \
             signal has no exit code, which is how the allocator's abort arrived"
        );
    }

    // The width still applies below the limit, and still to every field: the
    // check is against the running total, so a format string cannot walk past
    // the limit one modest field at a time.
    agrees(
        &tclsh,
        "puts [string length [format %100000d 1]]",
        out("100000\n"),
    );
    agrees(
        &tclsh,
        "puts [string length [format %60000d%60000d 1 1]]",
        out("120000\n"),
    );
}

/// **Fixed for tclrs.** Nesting in an expression is bounded by
/// [`tclrs::expr::MAX_EXPR_DEPTH`] rather than by the stack, so the deepest
/// expression reports a Tcl error instead of aborting the process.
///
/// `src/parser.rs` has bounded the command language's recursion at
/// `MAX_NESTING_DEPTH` for exactly this reason; `src/expr.rs` did not, and
/// `expr {((…1…))}` overflowed the stack between 7_500 and 8_000 parentheses on
/// the stack the binary gives it, a chain of unary operators between 100_000 and
/// 150_000.
///
/// Unlike the command parser's limit, this one is below what the reference
/// interpreter survives: tclsh 9.0.4 parses expressions with an explicit stack
/// (`tclCompExpr.c`) rather than by recursion and answers 1_000_000 nested
/// parentheses without complaint, which this test asserts rather than assumes.
/// So the bound is a divergence, and a deliberate one — a clean Tcl error for an
/// input no script writes, in place of a process that dies with nothing to
/// report.
///
/// Run as subprocesses: a stack overflow aborts rather than panicking, so it
/// cannot be caught in-process, and the depths involved need more stack than a
/// test thread has.
#[test]
fn expr_nesting_is_refused_rather_than_exhausting_the_stack() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    let run = |binary: &PathBuf, program: &str, label: &str| -> (Option<i32>, String) {
        let path = case_path(label, "exprnest");
        std::fs::write(&path, program).expect("write case");
        let out = std::process::Command::new(binary)
            .arg(&path)
            .output()
            .expect("run");
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .chain(String::from_utf8_lossy(&out.stderr).lines())
                .next()
                .unwrap_or("")
                .to_string(),
        )
    };
    let parens = |depth: usize| {
        format!(
            "puts [expr {{{}1{}}}]",
            "(".repeat(depth),
            ")".repeat(depth)
        )
    };
    let unary = |depth: usize| format!("puts [expr {{{}1}}]", "-".repeat(depth));
    let subject = PathBuf::from(TCLRS);
    let limit = tclrs::expr::MAX_EXPR_DEPTH;
    let refusal = (
        Some(1),
        "too many nested subexpressions (infinite loop?)".to_string(),
    );

    // At the limit both engines still answer, and that parity is what keeps the
    // limit from being set somewhere ordinary code would reach.
    assert_eq!(run(&tclsh, &parens(limit), "ref-at"), (Some(0), "1".into()));
    assert_eq!(
        run(&subject, &parens(limit), "sub-at"),
        (Some(0), "1".into())
    );
    // The unary chain too, against tclsh rather than against a written-down
    // sign: whether `limit` negations leave the operand negative depends on
    // whether `limit` is even, which is not what this test is about.
    assert_eq!(
        run(&subject, &unary(limit), "sub-unary-at"),
        run(&tclsh, &unary(limit), "ref-unary-at")
    );

    // One past it is the refusal, and so are the two depths that used to abort.
    assert_eq!(run(&subject, &parens(limit + 1), "sub-over"), refusal);
    assert_eq!(run(&subject, &parens(8_000), "sub-8k"), refusal);
    assert_eq!(run(&subject, &unary(150_000), "sub-unary-150k"), refusal);

    // tclsh has no such bound, which is why this is recorded as a divergence
    // rather than as parity. If tclsh ever grows one, this stops being true and
    // the reasoning above needs re-measuring.
    assert_eq!(
        run(&tclsh, &parens(1_000_000), "ref-1m"),
        (Some(0), "1".into()),
        "tclsh no longer parses 1_000_000 nested parentheses"
    );

    // The function-argument and ternary descents are bounded by the same
    // counter, so neither is a way around it.
    let calls = format!(
        "puts [expr {{{}1{}}}]",
        "abs(".repeat(8_000),
        ")".repeat(8_000)
    );
    assert_eq!(run(&subject, &calls, "sub-calls"), refusal);
    let ternaries = format!(
        "puts [expr {{{}1{}}}]",
        "1?".repeat(8_000),
        ":0".repeat(8_000)
    );
    assert_eq!(run(&subject, &ternaries, "sub-ternary"), refusal);
}

/// **Fixed for tclrs.** The "followed by junk" diagnostic no longer panics when
/// its twenty-byte cap lands inside a multi-byte character.
///
/// The reference implementation quotes twenty *bytes* of whatever followed a
/// close-brace or close-quote where a separator belonged — `TclFindElement`'s
/// loop is `while ((p2 < limit) && !TclIsSpaceProc(*p2) && (p2 < p+20))`. A
/// continuation byte is not a space, so the walk runs through a multi-byte
/// character, and slicing a Rust `str` at the resulting offset is
/// `byte index N is not a char boundary` — a panic on the interpreter thread,
/// which no `catch` can see.
///
/// Found by the `vm` cargo-fuzz target, from a generated `dict merge` whose
/// argument the fuzzer had filled with high bytes; minimised to the one line
/// below. Both copies of the walk had it — `src/list.rs` and `src/assoc.rs` —
/// and there is one copy now (`list::junk_prefix`), which is why all four
/// callers are checked here.
///
/// The policy is to drop the partial character rather than to reach past it,
/// because that is what tclsh prints: nineteen `x` and nothing after them, for
/// a cap that falls between the two bytes of `é`. Measured, not reasoned —
/// `agrees` runs tclsh on each of these.
#[test]
fn a_split_character_in_the_junk_diagnostic_does_not_panic() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    // Nineteen bytes of filler puts the twenty-byte cap between the two bytes
    // of the `é` that follows.
    let junk = format!("{}é", "x".repeat(19));
    for (program, quoted_in) in [
        (format!("puts [llength {{\"a\"{junk}}}]"), "quotes"),
        (format!("puts [llength {{{{a}}{junk}}}]"), "braces"),
        (format!("array set A {{\"a\"{junk}}}"), "quotes"),
    ] {
        agrees(
            &tclsh,
            &program,
            err(&format!(
                "list element in {quoted_in} followed by \"{}\" instead of space",
                "x".repeat(19)
            )),
        );
    }
    for (program, quoted_in) in [
        (format!("puts [dict size {{\"a\"{junk}}}]"), "quotes"),
        (format!("puts [dict get {{{{a}}{junk}}} k]"), "braces"),
    ] {
        agrees(
            &tclsh,
            &program,
            err(&format!(
                "dict element in {quoted_in} followed by \"{}\" instead of space",
                "x".repeat(19)
            )),
        );
    }

    // A character that fits inside the cap is still quoted whole, so the fix is
    // a boundary rule and not a blanket truncation to ASCII.
    agrees(
        &tclsh,
        "puts [llength {\"a\"éxx}]",
        err("list element in quotes followed by \"éxx\" instead of space"),
    );
}

/// A refusal decided at *run* time is catchable, so a `catch` around it sees a
/// message where tclsh saw an answer.
///
/// `lsearch -sorted` and `lsort -nocase` are recognised by the reference option
/// parser and refused when the command runs, so `catch` captures the refusal and
/// the script carries on with it as a value — the harness calls the case a
/// divergence rather than a skip, because tclrs exited 0. The refusals decided
/// while *compiling* — `string is punct`, `string wordstart` — are not catchable
/// and do make the case a skip. Both halves are pinned here so the distinction
/// cannot drift without a test failing.
#[test]
fn bug_a_runtime_refusal_is_caught_where_tclsh_answers() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    diverges(
        &tclsh,
        "catch {lsearch -sorted {a} b} m; puts m:$m",
        out("m:-1\n"),
        out("m:lsearch -sorted is not supported yet\n"),
    );
    diverges(
        &tclsh,
        "catch {lsort -nocase {a}} m; puts m:$m",
        out("m:a\n"),
        out("m:lsort -nocase is not supported yet\n"),
    );
    // Decided while compiling, so `catch` never runs at all.
    diverges(
        &tclsh,
        "catch {string is punct a} m; puts m:$m",
        out("m:0\n"),
        err("the \"punct\" character class needs Unicode category tables, which are not built yet"),
    );
}

/// A quoted `nan` is a usable number in arithmetic where tclsh refuses it.
///
/// `Tcl_GetDoubleFromObj` rejects a NaN read from a string operand —
/// `cannot use non-numeric floating-point value "nan" as left operand of "+"` —
/// and tclrs answers `NaN`.
#[test]
fn bug_expr_accepts_a_quoted_nan_as_an_arithmetic_operand() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    diverges(
        &tclsh,
        "puts [expr {\"nan\" + 1}]",
        err("cannot use non-numeric floating-point value \"nan\" as left operand of \"+\""),
        out("NaN\n"),
    );
}

/// A left shift past the word width wraps silently.
///
/// Everywhere else an operation that leaves `i64` reports `integer value too
/// large to represent` rather than wrapping — that is the documented stance on
/// the missing bignum (BUGS.md, "Arbitrary-precision integers"). `<<` does not
/// take part: `1 << 63` answers `i64::MIN` and `1 << 64` answers 1, where tclsh
/// promotes and answers exactly. This is the one place a value silently changes
/// rather than being refused.
#[test]
fn bug_expr_left_shift_past_the_word_width_wraps() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    diverges(
        &tclsh,
        "puts [expr {1 << 63}]",
        out("9223372036854775808\n"),
        out("-9223372036854775808\n"),
    );
    diverges(
        &tclsh,
        "puts [expr {1 << 64}]",
        out("18446744073709551616\n"),
        out("1\n"),
    );
}

/// `nan` and `inf` are floating-point literals to `expr(n)` and barewords to
/// `expr::parse_number`.
///
/// The same shape as the `0d9` / `1_0` entry above: the runtime's number parser
/// takes them — `string is double inf` is 1, and `expr {"inf" + 0.0}` works —
/// but `expr`'s own literal parser does not, so a bare `inf` in an expression is
/// `invalid bare word "inf" in expression` where tclsh evaluates it. It is the
/// single largest divergence class in the widened run: 342 of 2000 cases.
#[test]
fn bug_expr_literal_grammar_lacks_nan_and_inf() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    diverges(
        &tclsh,
        "puts [expr {inf > 1}]",
        out("1\n"),
        err("invalid bare word \"inf\" in expression"),
    );
    diverges(
        &tclsh,
        "puts [expr {nan == nan}]",
        out("0\n"),
        err("invalid bare word \"nan\" in expression"),
    );
    // The runtime parser has both, which is what makes this `expr::parse_number`
    // rather than the number grammar as a whole.
    agrees(&tclsh, "puts [string is double inf]", out("1\n"));
}

/// A shift by a negative count answers 0 instead of raising.
///
/// `expr(n)`: "It is illegal to shift by a negative number of bits." tclsh
/// reports `negative shift argument`; tclrs answers 0 for `<<` and for `>>`.
#[test]
fn bug_expr_negative_shift_count_answers_zero() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    diverges(
        &tclsh,
        "puts [expr {10 << -1}]",
        err("negative shift argument"),
        out("0\n"),
    );
    diverges(
        &tclsh,
        "puts [expr {10 >> -2}]",
        err("negative shift argument"),
        out("0\n"),
    );
}

/// `expr`'s always-string operators compare numerically when both operands are
/// numeric *literals*.
///
/// `expr(n)` is explicit that `eq`, `ne`, `lt`, `gt`, `le` and `ge` "compare
/// operands as strings", which is the whole reason the operators exist next to
/// `==` and `<`. tclrs interns a bare numeric literal as a number and the
/// comparison then runs on the numbers, so `1.0 eq 1` is true where tclsh says
/// false and `2.5e-3 gt 123456789` is false where tclsh compares `"2"` against
/// `"1"` and says true. Quoting either operand restores the string comparison in
/// both engines, so the defect is in what the literal became, not in the
/// operator.
#[test]
fn bug_expr_string_operators_compare_numeric_literals_as_numbers() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    diverges(&tclsh, "puts [expr {1.0 eq 1}]", out("0\n"), out("1\n"));
    diverges(
        &tclsh,
        "puts [expr {2.5e-3 gt 123456789}]",
        out("1\n"),
        out("0\n"),
    );
    // Quoted, both engines compare as strings — which is what the operator means.
    agrees(
        &tclsh,
        "puts [expr {\"2.5e-3\" gt \"123456789\"}]",
        out("1\n"),
    );
}

/// Zero divided by a floating-point zero is `NaN` rather than a domain error.
///
/// tclsh reports `domain error: argument not in valid range` for `0 / 0.0` and
/// for `0.0 / 0.0`; tclrs answers `NaN`. The non-zero numerator agrees — `3 /
/// -0.0` is `-Inf` in both — so this is the indeterminate form specifically.
#[test]
fn bug_expr_zero_over_float_zero_is_nan_not_a_domain_error() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    diverges(
        &tclsh,
        "puts [expr {0 / -0.0}]",
        err("domain error: argument not in valid range"),
        out("NaN\n"),
    );
    agrees(&tclsh, "puts [expr {3 / -0.0}]", out("-Inf\n"));
}

/// `format`'s `-` flag does not override `0` for the conversions where tclsh
/// says it does.
///
/// tclsh applies `-` for `e`, `f`, `g` and `s` — `format %-08.2f 1.5` is
/// `1.50    ` — and keeps the zero padding for `d`, `i`, `x` and `o`, where
/// `format %-08d 5` is `00000005` in both engines. tclrs zero-pads on the left
/// for every conversion, so the floating-point and string cases come out
/// `00001.50` and `000000ab`.
#[test]
fn bug_format_minus_flag_does_not_override_zero_padding() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    diverges(
        &tclsh,
        "puts [format %-08.2f 1.5]",
        out("1.50    \n"),
        out("00001.50\n"),
    );
    diverges(
        &tclsh,
        "puts [format %-08s ab]",
        out("ab000000\n"),
        out("000000ab\n"),
    );
    // The integer conversions already agree, which is why this is `-` against
    // `0` rather than the padding as a whole.
    agrees(&tclsh, "puts [format %-08d 5]", out("00000005\n"));
    agrees(&tclsh, "puts [format %-08x 255]", out("000000ff\n"));
}
