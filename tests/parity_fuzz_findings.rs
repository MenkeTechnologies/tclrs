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
        error: message_of(&String::from_utf8_lossy(&out.stderr)),
    }
}

/// The error *message* out of tclsh's stderr, without the stack trace under it.
///
/// Every trace line is indented — `    (parsing expression "a")`, `    invoked
/// from within`, `    (file "…" line N)` — and the message is the unindented
/// lines above them. Taking only the first line was enough while every message
/// this file records was one line; `expr`'s are three, since a refusal carries
/// the expression it was reading and a bare word carries the spelling hint, and
/// those lines are part of what `catch` yields rather than part of the trace.
/// Capturing them is what keeps this file comparing tclrs's whole message
/// against tclsh's whole message.
fn message_of(stderr: &str) -> String {
    let mut lines = Vec::new();
    for line in stderr.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            break;
        }
        lines.push(line);
    }
    lines.join("\n")
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

/// A1, **fixed**: reading a variable that was never set raises tclsh's error.
///
/// tclrs collapsed no-such-variable and empty into one `Undef`, so the read
/// succeeded with the empty string. It was the longest-standing divergence, and
/// it was also the only way the differential fuzzer ever produced a hang:
/// `catch {while {$w13 < 1} {}} m` never terminates when `"" < 1` is a true
/// string comparison, where tclsh's error ends the loop through the `catch`.
///
/// The check is fusevm's (`VM::set_undef_hook`, 0.16.0) rather than a frontend
/// op, because a counted loop reads its counter every iteration and an
/// extension op there would cost the loop its JIT trace. fusevm hands the hook
/// the read's chunk and op index, which is what separates `$x` — an error —
/// from `incr x`, which Tcl initialises to zero: both are the same read op on
/// the same name.
#[test]
fn an_unset_variable_read_is_an_error() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    agrees(
        &tclsh,
        "puts <$nosuchvar>",
        err("can't read \"nosuchvar\": no such variable"),
    );
    // Through `catch`, both engines now have the same message to report.
    agrees(
        &tclsh,
        "catch {set x $nosuchvar} m\nputs [string length $m]",
        out("40\n"),
    );
    // The loop that used to hang terminates, because the read ends it.
    agrees(
        &tclsh,
        "catch {while {$w13 < 1} {}} m\nputs $m",
        out("can't read \"w13\": no such variable\n"),
    );
}

/// The other half of that rule: `incr` on a variable that does not exist
/// creates it, and says so *after* a nested script has been compiled.
///
/// The tolerant sites are keyed by the chunk they belong to as well as their op
/// index. Keyed by index alone — or by `Chunk::op_hash`, which ignores the name
/// pool because it keys the JIT's native-code cache — an `eval` would answer
/// for the wrong script and this `incr` would refuse instead of initialising.
#[test]
fn incr_creates_a_variable_that_does_not_exist() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    agrees(&tclsh, "incr fresh\nputs $fresh", out("1\n"));
    agrees(&tclsh, "incr fresh 5\nputs $fresh", out("5\n"));
    // A nested script is a chunk of its own, whose op indices start at zero
    // again; the read below is a different site in a different chunk.
    agrees(
        &tclsh,
        "eval {set q 1}\nincr counter\nputs $counter",
        out("1\n"),
    );
    // And the refusing read still refuses in the same script.
    agrees(
        &tclsh,
        "incr n\ncatch {puts $absent} m\nputs \"$n $m\"",
        out("1 can't read \"absent\": no such variable\n"),
    );
}

/// A4, **fixed**: an argument count is checked when the call is reached, so
/// everything before it has already run — which is where tclsh checks it.
///
/// This was the largest divergence class the fuzzer reported. Arity was
/// resolved while compiling, so a script with one bad call anywhere produced no
/// output at all; now the call fails where it stands (`Compiler::defer`).
#[test]
fn arity_is_reported_where_the_call_is_reached() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    agrees(
        &tclsh,
        "proc f {} {puts body}\nf\nf 1\n",
        Observed {
            stdout: "body\n".to_string(),
            error: "wrong # args: should be \"f\"".to_string(),
        },
    );
    // The other half of the same rule: a call that is never reached is not an
    // error at all, in either engine.
    agrees(
        &tclsh,
        "proc f {} {}\nif {0} {f 1 2 3}\nputs done\n",
        out("done\n"),
    );
    // And an unknown command follows the same rule, at the same moment.
    agrees(
        &tclsh,
        "if {0} {nosuchcommand}\nputs [catch {nosuchcommand} m]\nputs $m\n",
        out("1\ninvalid command name \"nosuchcommand\"\n"),
    );
}

/// A2: an unterminated brace is reported where the input ran out rather than
/// where the brace opened. The message agrees; the line does not.
#[test]
fn deviation_unterminated_brace_reports_the_last_line() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
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
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
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

/// The last of the same coercion, now closed: outside a boolean position `expr`
/// used to take a non-numeric string as zero, because fusevm's bitwise ops
/// coerce through `Value::to_int`. They are extension ops now whenever the
/// compiler cannot prove both operands integral, and each of the five programs
/// below is parity.
#[test]
fn bug_non_numeric_strings_are_coerced_outside_a_boolean_position() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    agrees(
        &tclsh,
        "puts [expr {\"b\" >> 1}]",
        err("cannot use non-numeric string \"b\" as left operand of \">>\""),
    );
    agrees(
        &tclsh,
        "puts [expr {\"b\" & 1}]",
        err("cannot use non-numeric string \"b\" as left operand of \"&\""),
    );
    agrees(
        &tclsh,
        "puts [expr {\"b\" | 1}]",
        err("cannot use non-numeric string \"b\" as left operand of \"|\""),
    );
    agrees(
        &tclsh,
        "puts [expr {~\"b\"}]",
        err("cannot use non-numeric string \"b\" as operand of \"~\""),
    );
    agrees(
        &tclsh,
        "puts [expr {!\"b\"}]",
        err("cannot use non-numeric string \"b\" as operand of \"!\""),
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
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
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
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
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
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
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

/// The rest of the same finding, **fixed**: when it is the *variable* that does
/// not hold an integer, the refusal is `incr`'s wording too.
///
/// It was the numeric hook behind `Op::Add` that answered, in `expr`'s words,
/// because `incr x` and `expr {$x + 1}` lower to the same arithmetic on the same
/// value. An extension op in `incr`'s lowering would have separated them and
/// cost every `incr` loop its compiled trace — `tiers::tests::
/// a_proc_local_counter_loop_reaches_a_compiled_trace` and
/// `bench/counted_loop_proc.tcl` both depend on that trace. The site separates
/// them instead: fusevm hands the hook the chunk and op index
/// (`fusevm::NumericCall`), the compiler records where each `incr` put its
/// arithmetic, and the arithmetic stays a native `Op::Add`.
#[test]
fn incr_reports_its_own_wording_for_a_non_integer_variable() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    for same in [
        "set x abc\nincr x",
        "set x 5\nset y abc\nincr x $y",
        // A double is not an integer to `incr`, even though the addition would
        // have answered 2.5 quite happily.
        "set x 1.5\nincr x",
        "set x 5\nset y 1.5\nincr x $y",
        // An element and a procedure's local reach the same hook by other ops.
        "set a(k) abc\nincr a(k)",
        "proc p {} {set q abc\nincr q}\np",
        // `expr` keeps its own wording — the point of separating them.
        "set x abc\nexpr {$x + 1}",
        "set x abc\nexpr {1 + $x}",
    ] {
        assert_eq!(reference(&tclsh, same), subject(same), "{same}");
    }
    // The variable being *absent* is a different case: `incr` counts from zero.
    agrees(&tclsh, "incr fresh 5\nputs $fresh", out("5\n"));
    // And a promoted integer is still an integer, so this is arithmetic rather
    // than a refusal.
    agrees(
        &tclsh,
        "set y 99999999999999999999\nputs [incr y -1]",
        out("99999999999999999998\n"),
    );
}

/// `format`'s integer conversions name a list as a list. tclrs quotes the value
/// instead, which is the wording for a non-list.
#[test]
fn bug_format_reports_a_list_as_a_quoted_string() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    agrees(
        &tclsh,
        "puts [format %+d {{a b} c}]",
        err("expected integer but got a list"),
    );
}

/// An operand `expr` refuses is worded as tclsh 9.0.4 words it: the value named
/// in place, and which side of the operator it was on.
///
/// Was Tcl 8's wording (`can't use non-numeric string as operand of "+":
/// "abc"`), which carried no side and put the value last. Fixed; the three
/// programs are the same three, now asserted as parity.
#[test]
fn bug_expr_operand_errors_use_the_older_wording() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    agrees(
        &tclsh,
        "puts [expr {1 + \"abc\"}]",
        err("cannot use non-numeric string \"abc\" as right operand of \"+\""),
    );
    agrees(
        &tclsh,
        "puts [expr {\"10\" - \"b\"}]",
        err("cannot use non-numeric string \"b\" as right operand of \"-\""),
    );
    agrees(
        &tclsh,
        "puts [expr {1.0 % 2}]",
        err("cannot use floating-point value \"1.0\" as left operand of \"%\""),
    );
}

/// **Fixed.** `**` keeps an integral result for integral operands even when the
/// exponent is negative, so the true value truncates toward zero: `2 ** -1` is 0.
/// Only ±1 survives, and a zero base has no value at all there.
#[test]
fn integer_exponentiation_stays_integral_for_a_negative_exponent() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
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

/// **Fixed.** An integer beyond `i64` is a value, not a refusal: Tcl 9's
/// integers are arbitrary precision and these now are too.
///
/// This finding has been through every stage. The spelling first became a
/// double, answering `1e+20` for a value the script never wrote; then it became
/// `integer value too large to represent`, which was honest but still not
/// tclsh's answer; it now promotes and answers exactly. The arithmetic reaches
/// a `BigInt` only after fusevm's checked `i64` path has already overflowed, so
/// nothing on a hot loop's path changed to get here.
#[test]
fn integers_beyond_i64_promote_to_arbitrary_precision() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    for same in [
        // The four shapes the earlier stages each got wrong in turn.
        "puts [expr {99999999999999999999 + 1}]",
        "puts [expr {99999999999999999999 % 3}]",
        "set x 99999999999999999999\nputs [expr {$x + 1}]",
        "puts [expr {0x10000000000000000}]",
        // Growth, and the way back down.
        "puts [expr {2 ** 100}]",
        "puts [expr {9223372036854775807 * 3}]",
        "puts [expr {(2 ** 100) / (2 ** 100)}]",
        "puts [expr {(2 ** 100) - (2 ** 100)}]",
        // Floored division and remainder hold at width, which truncation would
        // get wrong in the negative cases.
        "puts [expr {-99999999999999999999 / 7}]",
        "puts [expr {-99999999999999999999 % 7}]",
        "puts [expr {99999999999999999999 / -7}]",
        "puts [expr {99999999999999999999 % -7}]",
        // Ordering is exact rather than through a double, which is observable:
        // these three would all be equal if either side became an `f64`.
        "puts [expr {99999999999999999999 < 1e20}]",
        "puts [expr {99999999999999999999 == 1e20}]",
        "puts [expr {1e20 == 100000000000000000000}]",
        "puts [expr {100000000000000000001 > 100000000000000000000}]",
        // A double operand still makes the result a double.
        "puts [expr {99999999999999999999 + 0.5}]",
        // The bitwise operators are two's complement over an infinite sign
        // extension, as Tcl's are.
        "puts [expr {99999999999999999999 & 255}]",
        "puts [expr {~99999999999999999999}]",
        // The `i64` ends themselves are not overflow, and the literal is still
        // spelled as the script wrote it where `eq` can see it.
        "puts [expr {9223372036854775807 - 1}]",
        "puts [expr {0x7fffffffffffffff}]",
        "puts [expr {0x10000000000000000 eq \"0x10000000000000000\"}]",
        "puts [expr {99999999999999999999}]",
        "puts 99999999999999999999",
        "if {99999999999999999999} {puts T}",
        // `incr` promotes through the same path.
        "set x 9223372036854775807\nincr x\nputs $x",
        "set y 99999999999999999999\nputs [incr y -1]",
    ] {
        assert_eq!(reference(&tclsh, same), subject(same), "{same}");
    }
    // What tclsh itself refuses stays refused: an operand of `-integer` must be
    // a machine integer there, and saturating to one would sort by a value the
    // script never wrote.
    agrees(
        &tclsh,
        "puts [lsort -integer {99999999999999999999 5}]",
        err("integer value too large to represent"),
    );
}

/// **Fixed.** A character `expr` cannot use is named as a character, in tclsh's
/// own wording: `invalid character "Ü"`, not the `Ã` that is the lead byte of its
/// UTF-8 encoding.
#[test]
fn an_unusable_character_in_an_expression_is_named_as_a_character() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    agrees(
        &tclsh,
        "puts [expr {Ü}]",
        err("invalid character \"Ü\"\nin expression \"Ü\""),
    );
    agrees(
        &tclsh,
        "puts [expr {1 + αβγ}]",
        err("invalid character \"α\"\nin expression \"1 + αβγ\""),
    );
    // The same wording covers the ASCII characters, which were named correctly
    // before but with the other message.
    agrees(
        &tclsh,
        "puts [expr {@}]",
        err("invalid character \"@\"\nin expression \"@\""),
    );
    for same in [
        "puts [expr {1 + @}]",
        "puts [expr {é}]",
        "puts [expr {日}]",
        "puts [expr {\u{1F600}}]",
    ] {
        assert_eq!(reference(&tclsh, same), subject(same), "{same}");
    }
}

/// Code that never executes costs a script nothing — **fixed**, in both halves.
///
/// A command's failure is lowered where the command stands, and a body's own
/// parse failure is lowered as the body, so neither is a verdict on the script.
/// What stays eager is the one class tclsh reports eagerly too: an unbalanced
/// brace, because brace counting is how the enclosing script delimits the word
/// at all, and neither engine can read past it.
#[test]
fn unreachable_code_costs_nothing() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    // An argument count, a command name and an ensemble subcommand are all
    // resolved when the command is reached, as tclsh resolves them.
    agrees(&tclsh, "if {0} {incr}\nputs reached", out("reached\n"));
    agrees(
        &tclsh,
        "if {0} {nosuchcommand}\nputs reached",
        out("reached\n"),
    );
    agrees(
        &tclsh,
        "if {0} {string bogus x}\nputs reached",
        out("reached\n"),
    );
    // An expression that cannot be parsed, in a branch that is never taken.
    agrees(
        &tclsh,
        "if {0} {puts [expr {1 +}]}\nputs reached",
        out("reached\n"),
    );
    // A `switch` arm that is never selected: its body is a braced word, and
    // nothing in it is parsed until the arm is picked.
    agrees(
        &tclsh,
        "switch -- x {*b {puts \"a}}\nputs reached",
        out("reached\n"),
    );
    // A body that will not parse, everywhere a body can be: a loop that never
    // iterates, a procedure never called, a `catch` that traps it.
    agrees(
        &tclsh,
        "while {0} {puts \"a}\nputs reached",
        out("reached\n"),
    );
    agrees(
        &tclsh,
        "foreach x {} {puts \"a}\nputs reached",
        out("reached\n"),
    );
    agrees(
        &tclsh,
        "proc p {} {puts \"a}\nputs reached",
        out("reached\n"),
    );
    // And it still fails when it *is* reached, with the message it always had.
    agrees(
        &tclsh,
        "proc p {} {puts [expr {a}]}\np",
        err("invalid bareword \"a\"\nin expression \"a\";\nshould be \"$a\" or \"{a}\" or \"a(...)\" or ..."),
    );
    // The condition runs first: tclsh evaluates it, then fails only on entry.
    agrees(
        &tclsh,
        "if {[puts hi; expr 0]} {puts \"a}\nputs reached",
        out("hi\nreached\n"),
    );
    // Eager, in both engines: an unbalanced brace is not a body's failure but
    // the enclosing script's, since it is what delimits the body's word.
    agrees(
        &tclsh,
        "if {0} {puts {unclosed}\nputs reached",
        err("missing close-brace"),
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
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
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
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
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
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
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
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
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
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
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
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
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
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
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
/// `dict with` is recognised by the reference subcommand table and refused when
/// the command runs, so `catch` captures the refusal and the script carries on
/// with it as a value — the harness calls the case a divergence rather than a
/// skip, because tclrs exited 0. The refusals decided while *compiling* —
/// `string wordstart` beyond ASCII — are not catchable and do make the case a
/// skip. Both halves are pinned here so the distinction cannot drift without a
/// test failing.
#[test]
fn bug_a_runtime_refusal_is_caught_where_tclsh_answers() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    // `dict info` is the standing example: tclsh reports its hash-table
    // statistics and the refusal here is decided when the command runs, so
    // `catch` sees a message and the script goes on.
    diverges(
        &tclsh,
        "catch {dict info {a 1}} m; puts m:[lindex [split $m \\n] 0]",
        out("m:1 entries in table, 4 buckets\n"),
        out("m:dict info is not supported yet\n"),
    );
    // `dict with`, `lsearch -sorted` and `lsort -nocase` stood here as examples
    // of a catchable run-time refusal until each landed; they are the same
    // programs, asserted as agreements now.
    agrees(
        &tclsh,
        "set d {a 1}\ncatch {dict with d {}} m; puts m:$m",
        out("m:\n"),
    );
    agrees(
        &tclsh,
        "catch {lsearch -sorted {a} b} m; puts m:$m",
        out("m:-1\n"),
    );
    agrees(
        &tclsh,
        "catch {lsort -nocase {a}} m; puts m:$m",
        out("m:a\n"),
    );
    // `string is punct` used to sit here as the compile-time half of the
    // distinction — refused before `catch` could run. It is answered now, from
    // the category tables the class needs, so it belongs on the other side.
    agrees(
        &tclsh,
        "catch {string is punct a} m; puts m:$m",
        out("m:0\n"),
    );
    // The refusal that remains is narrower and still catchable, because it is
    // decided when the character is read rather than when the script is: U+20C1
    // is one of the 4804 code points tclsh 9.0.4 categorises and Unicode 16.0
    // does not.
    diverges(
        &tclsh,
        "catch {string is punct [format %c 0x20C1]} m; puts m:$m",
        out("m:0\n"),
        out(
            "m:string is punct: U+20C1 is categorised by tclsh 9.0.4 and not by \
             Unicode 16.0, which is the table this build carries\n",
        ),
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
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    agrees(
        &tclsh,
        "puts [expr {\"nan\" + 1}]",
        err("cannot use non-numeric floating-point value \"nan\" as left operand of \"+\""),
    );
}

/// A left shift past the word width promotes — **fixed**, and pinned so it
/// stays that way.
///
/// This one moved twice. It wrapped silently first — `1 << 63` answered
/// `i64::MIN` and `1 << 64` answered 1, the one place a value changed instead
/// of being refused — then became `integer value too large to represent` while
/// this frontend had no bignum. It now answers exactly, as tclsh does, and the
/// shift is a `BigInt` operation rather than an `i64` one whenever a bit would
/// leave the word.
#[test]
fn expr_left_shift_past_the_word_width_promotes() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    agrees(
        &tclsh,
        "puts [expr {1 << 63}]",
        out("9223372036854775808\n"),
    );
    agrees(
        &tclsh,
        "puts [expr {1 << 64}]",
        out("18446744073709551616\n"),
    );
    // A distance far past the word, and the round trip back down.
    agrees(&tclsh, "puts [expr {(1 << 200) >> 200}]", out("1\n"));
    // A right shift still saturates rather than wrapping its distance.
    agrees(&tclsh, "puts [expr {-1 >> 100}]", out("-1\n"));
    // The shifts that do fit still answer, and a right shift at any distance
    // does: only a left shift can leave the word.
    agrees(
        &tclsh,
        "puts [expr {1 << 62}]",
        out("4611686018427387904\n"),
    );
    agrees(&tclsh, "puts [expr {-1 >> 200}]", out("-1\n"));
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
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    agrees(&tclsh, "puts [expr {inf > 1}]", out("1\n"));
    agrees(&tclsh, "puts [expr {nan == nan}]", out("0\n"));
    // Only these three spellings, in any case, and nothing that merely starts
    // with one: `f64::from_str` takes exactly the set tclsh's lexer does.
    agrees(&tclsh, "puts [expr {inFiniTy}]", out("Inf\n"));
    agrees(&tclsh, "puts [expr {-inf}]", out("-Inf\n"));
    agrees(
        &tclsh,
        "puts [expr {infx}]",
        err("invalid bareword \"infx\"\nin expression \"infx\";\nshould be \"$infx\" or \"{infx}\" or \"infx(...)\" or ..."),
    );
    agrees(
        &tclsh,
        "puts [expr {nano}]",
        err("invalid bareword \"nano\"\nin expression \"nano\";\nshould be \"$nano\" or \"{nano}\" or \"nano(...)\" or ..."),
    );
    // The runtime parser has both, which is what makes this `expr::parse_number`
    // rather than the number grammar as a whole.
    agrees(&tclsh, "puts [string is double inf]", out("1\n"));
}

/// A shift by a negative count raises, as `expr(n)` says it must: "It is
/// illegal to shift by a negative number of bits."
///
/// Was 0 for both operators, because fusevm masks the distance to six bits and
/// -1 masks to 63. Fixed; the same two programs are now parity.
#[test]
fn bug_expr_negative_shift_count_answers_zero() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    agrees(
        &tclsh,
        "puts [expr {10 << -1}]",
        err("negative shift argument"),
    );
    agrees(
        &tclsh,
        "puts [expr {10 >> -2}]",
        err("negative shift argument"),
    );
}

/// `expr`'s always-string operators compare the operands as they were written —
/// **fixed**, and pinned here so it stays that way.
///
/// `expr(n)` is explicit that `eq`, `ne`, `lt`, `gt`, `le` and `ge` "compare
/// operands as strings", which is the whole reason the operators exist next to
/// `==` and `<`. tclrs used to intern a bare numeric literal as a number and
/// compare the numbers, so `1.0 eq 1` was true where tclsh says false and
/// `2.5e-3 gt 123456789` was false where tclsh compares `"2"` against `"1"` and
/// says true. A numeric literal now carries the text the script wrote
/// (`expr::Expr::Int`/`Float`), the comparison is a frontend op over Tcl's
/// string form of each side (`compiler::ext::STR_CMP`), and both engines agree.
#[test]
fn expr_string_operators_compare_operands_as_written() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    agrees(&tclsh, "puts [expr {1.0 eq 1}]", out("0\n"));
    agrees(&tclsh, "puts [expr {2.5e-3 gt 123456789}]", out("1\n"));
    agrees(&tclsh, "puts [expr {010 eq 10}]", out("0\n"));
    agrees(&tclsh, "puts [expr {1e3 eq 1000.0}]", out("0\n"));
    agrees(&tclsh, "puts [expr {0x10 eq 16}]", out("0\n"));
    // A value, rather than a literal, was never the part that was wrong.
    agrees(&tclsh, "set x 1.0\nputs [expr {$x eq 1}]", out("0\n"));
    // And an `expr` result compares as the string Tcl prints for it, which is
    // what says the result of one expression keeps Tcl's formatting when it
    // reaches the next.
    agrees(
        &tclsh,
        "puts [expr {[expr {1.0 + 1}] eq \"2.0\"}]",
        out("1\n"),
    );
    // Quoted, both engines compare as strings — which is what the operator means.
    agrees(
        &tclsh,
        "puts [expr {\"2.5e-3\" gt \"123456789\"}]",
        out("1\n"),
    );
}

/// `expr`'s literal number grammar takes the whole integer grammar — **fixed**.
///
/// The `0d` prefix and `_` as a digit separator were the runtime parser's and
/// not `expr::parse_number`'s, so `expr {0d9}`, `expr {1_0}`, `expr {0x1_0}` and
/// `expr {0b1_0}` all reported `extra characters after expression` where tclsh
/// answers 9, 10, 16 and 2. The separator is scanned as part of the literal and
/// dropped before the parse, so the value is the number and the text is still
/// what the script wrote — which is what the string operators above compare.
#[test]
fn expr_literal_grammar_takes_the_integer_grammar() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    for program in [
        "puts [expr {0d9}]",
        "puts [expr {1_0}]",
        "puts [expr {0x1_0}]",
        "puts [expr {0b1_0}]",
        "puts [expr {0o1_7}]",
        "puts [expr {1_0 + 1}]",
        "puts [expr {0d09 + 1}]",
        "puts [expr {1_0.5}]",
        "puts [expr {1_000_000}]",
        // The separator is part of the text, so it is what `eq` sees.
        "puts [expr {1_0 eq 10}]",
    ] {
        assert_eq!(reference(&tclsh, program), subject(program), "{program}");
    }
}

/// Zero divided by a floating-point zero is `NaN` rather than a domain error.
///
/// tclsh reports `domain error: argument not in valid range` for `0 / 0.0` and
/// for `0.0 / 0.0`; tclrs answers `NaN`. The non-zero numerator agrees — `3 /
/// -0.0` is `-Inf` in both — so this is the indeterminate form specifically.
#[test]
fn bug_expr_zero_over_float_zero_is_nan_not_a_domain_error() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    agrees(
        &tclsh,
        "puts [expr {0 / -0.0}]",
        err("domain error: argument not in valid range"),
    );
    agrees(&tclsh, "puts [expr {3 / -0.0}]", out("-Inf\n"));
}

/// `format`'s `-` flag against `0`, which tclsh resolves three different ways —
/// **fixed**, and pinned here so each way stays pinned.
///
/// None of the three is C's: C99 says `-` always overrides `0`. tclsh keeps the
/// zeroes on the left for the integer conversions, drops the `0` for spaces on
/// the right for the floating ones, and keeps the `0` as the fill but moves it
/// right for `%s` and `%c`. Only the integer case was right here, so `%-08.2f`
/// came out `00001.50` and `%-08s ab` came out `000000ab` — a wrong value, not
/// a wrong message. Every flag combination is swept in
/// `tests/string_differential.rs`; these four are the shapes that named the
/// three rules.
#[test]
fn format_minus_against_zero_follows_tcls_three_rules() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    // Floating: the `0` is dropped, spaces on the right.
    agrees(&tclsh, "puts [format %-08.2f 1.5]", out("1.50    \n"));
    // String and character: the `0` is kept as the fill, and moves right.
    agrees(&tclsh, "puts [format %-08s ab]", out("ab000000\n"));
    agrees(&tclsh, "puts [format %-06c 65]", out("A00000\n"));
    // Integer: the `0` wins and the zeroes stay left, which always agreed.
    agrees(&tclsh, "puts [format %-08d 5]", out("00000005\n"));
    agrees(&tclsh, "puts [format %-08x 255]", out("000000ff\n"));
    // Without the `0` flag every conversion left-justifies with spaces, as C does.
    agrees(&tclsh, "puts |[format %-8d 5]|", out("|5       |\n"));
    agrees(&tclsh, "puts |[format %-8s ab]|", out("|ab      |\n"));
}

// ── fixed by the four-run campaign (seeds 1001/2002 at depth 4, 3003/4004 at
//    depth 6; 4000 cases each) ────────────────────────────────────────────────
//
// Every case below cites the seed and case number of a divergence in that
// campaign, reduced to the one statement that carried it. The expectation is a
// live tclsh's, as everywhere else in this file.

/// `expr`'s operand diagnostics name the operand and the side it is on.
///
/// The largest class in the campaign: 494 of the 1420 run-time `message`
/// divergences and 123 of the 413 `stdout` ones. tclrs answered with Tcl 8's
/// wording, `can't use non-numeric string as operand of "+": "a"`; 9.0.4 says
/// which side the operand was on and quotes it in place.
///
/// Reduced from seed 1001 case 02616 (`if {"a" + 1000}`), seed 3003 case 01242
/// (`if {-7 + "a"}`) and seed 3003 case 03520 (`if {("a") + "b"}`).
#[test]
fn fixed_expr_names_the_side_of_a_non_numeric_operand() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    agrees(
        &tclsh,
        "puts [expr {\"a\" + 1000}]",
        err("cannot use non-numeric string \"a\" as left operand of \"+\""),
    );
    agrees(
        &tclsh,
        "puts [expr {-7 + \"a\"}]",
        err("cannot use non-numeric string \"a\" as right operand of \"+\""),
    );
    // Every operator words it the same way, and the unary ones name no side.
    agrees(
        &tclsh,
        "puts [expr {\"b\" / 8}]",
        err("cannot use non-numeric string \"b\" as left operand of \"/\""),
    );
    agrees(
        &tclsh,
        "puts [expr {1 ** \"a\"}]",
        err("cannot use non-numeric string \"a\" as right operand of \"**\""),
    );
    agrees(
        &tclsh,
        "puts [expr {~\"a\"}]",
        err("cannot use non-numeric string \"a\" as operand of \"~\""),
    );
    agrees(
        &tclsh,
        "puts [expr {+\"a\"}]",
        err("cannot use non-numeric string \"a\" as operand of \"+\""),
    );
}

/// An operand that could be a list is named `a list`, never quoted.
///
/// The same screen `incr` and `format` already used
/// (`crate::list::looks_like_a_list`), which `expr` was not applying at all:
/// tclrs quoted the text. A one-element list is *not* one — `{a}` is quoted —
/// so the screen is the element-count one and not "contains a space".
///
/// Reduced from seed 3003 case 01242 (`"10" ni {a b c}` in an arithmetic
/// context) and the `expected integer but got a list` group below.
#[test]
fn fixed_expr_names_a_list_operand_as_a_list() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    agrees(
        &tclsh,
        "set x {a b c}\nputs [expr {$x + 1}]",
        err("cannot use a list as left operand of \"+\""),
    );
    agrees(
        &tclsh,
        "set x {1 2 3}\nputs [expr {~$x}]",
        err("cannot use a list as operand of \"~\""),
    );
    // One element, and zero: both are quoted rather than named as a list.
    agrees(
        &tclsh,
        "set x {a}\nputs [expr {$x + 1}]",
        err("cannot use non-numeric string \"a\" as left operand of \"+\""),
    );
    agrees(
        &tclsh,
        "set x { }\nputs [expr {$x + 1}]",
        err("cannot use non-numeric string \" \" as left operand of \"+\""),
    );
}

/// The bitwise operators take integers, and refuse everything else.
///
/// fusevm's `Op::BitAnd` and friends coerce through `Value::to_int`, which reads
/// `1.5` as 1 and `"abc"` as 0, so tclrs *answered* where tclsh refuses. These
/// are wrong answers rather than wrong wording — the worst class in the
/// campaign — and account for about 30 of the 413 `stdout` divergences.
///
/// Reduced from seed 1001 case 02843 (`(3.14) le ...` beside `%`), seed 3003
/// case 03520 (`-2 >> 3` under a float) and the `& | ^ << >> ~` rows of the
/// `stdout` histogram.
#[test]
fn fixed_bitwise_operators_refuse_non_integers() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    agrees(
        &tclsh,
        "puts [expr {0.5 | 2}]",
        err("cannot use floating-point value \"0.5\" as left operand of \"|\""),
    );
    agrees(
        &tclsh,
        "puts [expr {\"abc\" & 1}]",
        err("cannot use non-numeric string \"abc\" as left operand of \"&\""),
    );
    agrees(
        &tclsh,
        "puts [expr {2 ^ 1.5}]",
        err("cannot use floating-point value \"1.5\" as right operand of \"^\""),
    );
    agrees(
        &tclsh,
        "puts [expr {~1.5}]",
        err("cannot use floating-point value \"1.5\" as operand of \"~\""),
    );
    agrees(
        &tclsh,
        "puts [expr {1.0 << 2}]",
        err("cannot use floating-point value \"1.0\" as left operand of \"<<\""),
    );
    // Two integers still compute, and still through the native op when the
    // compiler can prove both operands integral.
    agrees(&tclsh, "puts [expr {12 & 10}]", out("8\n"));
    agrees(&tclsh, "puts [expr {-1 & 3}]", out("3\n"));
    agrees(&tclsh, "set a 12\nputs [expr {$a | 3}]", out("15\n"));
}

/// A shift distance is checked, and a right shift saturates.
///
/// fusevm masks the distance to six bits, so `1 << 64` answered 0 and `1 << -1`
/// answered 0 as well. tclsh refuses a negative distance outright and treats a
/// right shift as arithmetic at any distance.
///
/// Reduced from the `stdout` rows `tclsh=18446744073709551616 tclrs=0`,
/// `tclsh=0 tclrs=4294967295` and `tclsh=m:negative shift argument tclrs=xyz`.
#[test]
fn fixed_shift_distance_is_checked_and_saturates() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    agrees(
        &tclsh,
        "puts [expr {1 << -1}]",
        err("negative shift argument"),
    );
    agrees(
        &tclsh,
        "puts [expr {1 >> -1}]",
        err("negative shift argument"),
    );
    agrees(&tclsh, "puts [expr {1 >> 200}]", out("0\n"));
    agrees(&tclsh, "puts [expr {-1 >> 200}]", out("-1\n"));
    agrees(&tclsh, "puts [expr {-1 >> 62}]", out("-1\n"));
    agrees(
        &tclsh,
        "puts [expr {1 << 62}]",
        out("4611686018427387904\n"),
    );
    agrees(&tclsh, "puts [expr {0 << 200}]", out("0\n"));
}

/// `%` refuses a double, names it, and checks its left operand first.
///
/// 168 of the run-time `message` divergences and 29 of the `stdout` ones.
/// tclrs's wording carried neither the value nor the side, and it parsed both
/// operands before complaining, so `expr {1.5 % "a"}` blamed the string where
/// tclsh blames the float.
///
/// Reduced from seed 1001 case 00753 (`expr {1e300 % $w11}`), seed 3003 case
/// 00876 (`$s2 % "1.0"`) and seed 3003 case 01084 (`1.0e-7 % (100)`).
#[test]
fn fixed_modulo_names_the_double_operand_and_checks_the_left_one_first() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    agrees(
        &tclsh,
        "set y 1.0e-7\nputs [expr {$y % 100}]",
        err("cannot use floating-point value \"1.0e-7\" as left operand of \"%\""),
    );
    agrees(
        &tclsh,
        "set s2 1\nputs [expr {$s2 % \"1.0\"}]",
        err("cannot use floating-point value \"1.0\" as right operand of \"%\""),
    );
    // The left operand decides even when the right one is worse.
    agrees(
        &tclsh,
        "puts [expr {1.5 % \"a\"}]",
        err("cannot use floating-point value \"1.5\" as left operand of \"%\""),
    );
    agrees(
        &tclsh,
        "puts [expr {\"a\" % 1.5}]",
        err("cannot use non-numeric string \"a\" as left operand of \"%\""),
    );
}

/// An exponent too large to apply is its own diagnostic, not the overflow.
///
/// Eight `stdout` divergences recorded `tclsh=m:exponent too large
/// tclrs=m:integer value too large to represent`.
#[test]
fn fixed_oversized_exponent_is_reported_as_one() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    agrees(
        &tclsh,
        "puts [expr {2 ** 9999999999}]",
        err("exponent too large"),
    );
}

/// `format` and `string repeat` name a list operand as a list.
///
/// 233 run-time `message` divergences and 58 `stdout` ones: the wording was
/// already right for a plain string and `crate::runtime::named` already existed
/// for it; `src/cmd_string.rs` was formatting the text directly instead of
/// calling it.
///
/// Reduced from seed 1001 case 00079 (`format %X [list {a b c} {}]`), case 00495
/// (`format %b [list 0 "\{"]`) and case 02843 (`format %c $s2` with `s2` a
/// three-element list).
#[test]
fn fixed_format_names_a_list_argument_as_a_list() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    agrees(
        &tclsh,
        "puts [format %X [list {a b c} {}]]",
        err("expected integer but got a list"),
    );
    agrees(
        &tclsh,
        "set s2 {1 2 3}\nputs [format %c $s2]",
        err("expected integer but got a list"),
    );
    agrees(
        &tclsh,
        "puts [format %f {a b c}]",
        err("expected floating-point number but got a list"),
    );
    agrees(
        &tclsh,
        "set L {a b c}\nputs [string repeat a $L]",
        err("expected integer but got a list"),
    );
    // A value that is not a list is still quoted, and still cut at 50 bytes.
    agrees(
        &tclsh,
        "puts [format %d abc]",
        err("expected integer but got \"abc\""),
    );
    agrees(
        &tclsh,
        "puts [format %d [string repeat q 80]]",
        err(&format!("expected integer but got \"{}\"", "q".repeat(50))),
    );
}

/// `lsearch -increasing` and `-decreasing` are accepted.
///
/// 14 `stdout` divergences: they describe the order a `-sorted` or `-bisect`
/// search would binary-search in and have no other effect, so tclsh answers the
/// same linear search with or without them, and refusing them turned a working
/// search into an error.
///
/// Reduced from seed 4004 case 03552 (`lsearch -increasing $s3 *b`) and seed
/// 3003 case 03520 (`lsearch -increasing {x]y} a*b*c`).
#[test]
fn fixed_lsearch_accepts_the_sort_order_options() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    agrees(&tclsh, "puts [lsearch -increasing {a b c} *b]", out("1\n"));
    agrees(&tclsh, "puts [lsearch -decreasing {a b c} *b]", out("1\n"));
    agrees(
        &tclsh,
        "puts [lsearch -increasing {x]y} a*b*c]",
        out("-1\n"),
    );
    agrees(
        &tclsh,
        "puts [lsearch -decreasing -all {a b a} a]",
        out("0 2\n"),
    );
}

/// `incr` on a variable that does not exist counts from zero.
///
/// tclrs read the absent variable as `Undef` and handed it to `Op::Add`, which
/// refused it as a non-numeric operand, so `proc p {} {incr n; return $n}` was an
/// error where tclsh answers 1. `incr` keeps its native `Op::Add` — an extension
/// op there would cost `bench/counted_loop_proc.tcl` its trace — so the zero is
/// read in the numeric hook, which is reached only for an operand the VM cannot
/// compute on natively.
///
/// Reduced from seed 3003 case 03567 (`proc p7 {} {... incr s1 1 ...}`), seed
/// 3003 case 03261 and seed 2002 case 02599.
#[test]
fn fixed_incr_on_an_absent_variable_counts_from_zero() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    agrees(&tclsh, "incr g 5\nputs $g", out("5\n"));
    agrees(&tclsh, "incr h\nputs $h", out("1\n"));
    agrees(
        &tclsh,
        "proc p {} {incr n; return $n}\nputs [p]",
        out("1\n"),
    );
    // A variable that exists and holds the empty string is *not* this case: it
    // is still refused, in the wording
    // `bug_incr_reports_an_expr_error_for_a_non_integer_variable` pins.
    assert_eq!(subject("set e {}\nincr e").stdout, "");
}

/// `expr` answers with the number an operand spells, not the text.
///
/// `expr {007}` is 7 and `expr {0x10}` is 16 in tclsh; tclrs passed the string
/// through, so `expr {$x}` and `expr {+$x}` answered `007` and `0x10`. An
/// integer too large for an `i64` is the one value whose text is the only
/// representation this frontend has, and it still passes through.
///
/// Reduced from the `stdout` rows `tclsh=s1=a tclrs=0o5` and `tclsh=0
/// tclrs=0o37777777777`.
#[test]
fn fixed_expr_answers_with_the_canonical_number() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    agrees(&tclsh, "set x 007\nputs [expr {$x}]", out("7\n"));
    agrees(&tclsh, "set x 0x10\nputs [expr {$x}]", out("16\n"));
    agrees(&tclsh, "set x 0o17\nputs [expr {+$x}]", out("15\n"));
    agrees(&tclsh, "set x { 42 }\nputs [expr {$x}]", out("42\n"));
    agrees(&tclsh, "set x abc\nputs [expr {$x}]", out("abc\n"));
    agrees(
        &tclsh,
        "puts [expr {99999999999999999999}]",
        out("99999999999999999999\n"),
    );
}

/// A NaN result is a domain error, and a NaN operand is a refusal.
///
/// Four run-time `message` divergences recorded `tclsh=domain error: argument
/// not in valid range tclrs=` — tclrs answered `NaN` where tclsh reports.
#[test]
fn fixed_nan_is_reported_rather_than_answered() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    agrees(
        &tclsh,
        "puts [expr {0.0/0.0}]",
        err("domain error: argument not in valid range"),
    );
    agrees(
        &tclsh,
        "puts [expr {\"nan\"+0}]",
        err("cannot use non-numeric floating-point value \"nan\" as left operand of \"+\""),
    );
    // A unary operator names no side, and `!` follows the operand rule here
    // rather than the boolean rule a *condition* follows for the same value.
    agrees(
        &tclsh,
        "puts [expr {!\"nan\"}]",
        err("cannot use non-numeric floating-point value \"nan\" as operand of \"!\""),
    );
    agrees(
        &tclsh,
        "puts [expr {+\"nan\"}]",
        err("cannot use non-numeric floating-point value \"nan\" as operand of \"+\""),
    );
    // A NaN in a *condition* is the boolean rule's diagnostic rather than an
    // operand refusal — and tclsh gives two different ones for it depending on
    // whether the condition was compiled, so what is pinned here is only that
    // `!` does not take that path. See `bug_a_nan_condition_has_two_diagnostics`.
    assert_eq!(
        subject("if {\"nan\"} {puts a}").error,
        "floating point value is Not a Number"
    );
}

/// tclsh reports a NaN condition two different ways, and tclrs has one.
///
/// `if {"nan"} {puts a}` at the top level of a script is `domain error: argument
/// not in valid range`; the same command inside a `catch` body or a procedure —
/// which is where tclsh compiles it — is `floating point value is Not a Number`.
/// tclrs compiles everything, so it gives the second everywhere, and only the
/// uncompiled spelling diverges. Not a defect this branch introduced: it is the
/// reference interpreter disagreeing with itself, recorded here because the
/// difference decides which of the two a test may assert.
#[test]
fn bug_a_nan_condition_has_two_diagnostics() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    diverges(
        &tclsh,
        "if {\"nan\"} {puts a}",
        err("domain error: argument not in valid range"),
        err("floating point value is Not a Number"),
    );
    // Compiled by tclsh, and then the two agree.
    agrees(
        &tclsh,
        "catch {if {\"nan\"} {puts a}} m\nputs $m",
        out("floating point value is Not a Number\n"),
    );
}

/// `expr`'s compile-time diagnostics are the reference interpreter's.
///
/// tclsh lexes an expression before it parses one, so its refusals name the
/// token rather than the parser's position: `invalid bareword "a"`, `missing
/// operand at _@_` (the marker is literal on the first line; the position is on
/// the second), `missing operator at _@_`, and the two unbalanced-paren
/// diagnostics. 227 of the compile-time `message` divergences are these three
/// wordings.
///
/// Reduced from the compile-time histogram's top rows: `tclsh=invalid bareword
/// "_" tclrs=invalid bare word "_" in expression` (118), `tclsh=missing operand
/// at _@_ tclrs=premature end of expression` (55) and `tclsh=missing operand at
/// _@_ tclrs=invalid character "_"` (54).
#[test]
fn fixed_expr_compile_time_diagnostics_match_the_reference() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    for (program, message) in [
        ("puts [expr {a}]", "invalid bareword \"a\"\nin expression \"a\";\nshould be \"$a\" or \"{a}\" or \"a(...)\" or ..."),
        ("puts [expr {1 + a}]", "invalid bareword \"a\"\nin expression \"1 + a\";\nshould be \"$a\" or \"{a}\" or \"a(...)\" or ..."),
        ("puts [expr {0x}]", "invalid bareword \"0x\"\nin expression \"0x\";\nshould be \"$0x\" or \"{0x}\" or \"0x(...)\" or ..."),
        ("puts [expr {1 + }]", "missing operand at _@_\nin expression \"1 + _@_\""),
        ("puts [expr {1 &&}]", "missing operand at _@_\nin expression \"1 &&_@_\""),
        // An operator where an operand belongs, which tclrs called an invalid
        // character.
        ("puts [expr {*1}]", "missing operand at _@_\nin expression \"_@_*1\""),
        ("puts [expr {&1}]", "missing operand at _@_\nin expression \"_@_&1\""),
        ("puts [expr {1 ? 2 : }]", "missing operand at _@_\nin expression \"1 ? 2 : _@_\""),
        ("puts [expr {1 2}]", "missing operator at _@_\nin expression \"1 _@_2\""),
        ("puts [expr {1..2}]", "missing operator at _@_\nin expression \"1._@_.2\""),
        ("puts [expr {1 ? 2}]", "missing operator \":\" at _@_\nin expression \"1 ? 2_@_\""),
        ("puts [expr {(1}]", "unbalanced open paren\nin expression \"(1\""),
        ("puts [expr {1 + (}]", "unbalanced open paren\nin expression \"1 + (\""),
        ("puts [expr {(1))}]", "unbalanced close paren\nin expression \"(1))\""),
        ("puts [expr {}]", "empty expression\nin expression \"\""),
        ("puts [expr {   }]", "empty expression\nin expression \"   \""),
        // Still an invalid character when it really is no token.
        ("puts [expr {1 @ 2}]", "invalid character \"@\"\nin expression \"1 @ 2\""),
        ("puts [expr {1 ; 2}]", "invalid character \";\"\nin expression \"1 ; 2\""),
        ("puts [expr {$}]", "invalid character \"$\"\nin expression \"$\""),
        ("puts [expr {=1}]", "incomplete operator \"=\"\nin expression \"=1\""),
    ] {
        agrees(&tclsh, program, err(message));
    }
}

/// `#` starts a comment inside an expression.
///
/// Found while matching the diagnostics above: `expr {#1}` is `empty expression`
/// in tclsh, which is only explicable if the `#` opened a comment. It does, and
/// the comment runs to the end of the line.
#[test]
fn fixed_expr_takes_a_hash_comment() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    agrees(&tclsh, "puts [expr {1 #c}]", out("1\n"));
    agrees(&tclsh, "puts [expr {1 + # c\n2}]", out("3\n"));
    agrees(
        &tclsh,
        "puts [expr {#1}]",
        err("empty expression\nin expression \"#1\""),
    );
}

/// `in` and `ni` split the list on Tcl's string form — **fixed**.
///
/// The membership test is string equality against the list's elements, so what
/// the list *is* decides the answer. The haystack was split through fusevm's
/// `as_str_cow`, which spells a double the VM's way: the literal list `3.0`
/// became the one-element list `3`, and `expr {$x in 3.0}` with `x` of 3 was
/// true where tclsh says false. It only surfaced once a double literal could
/// reach the operator as a `Value::Float` at all.
#[test]
fn in_and_ni_test_membership_on_tcls_string_form() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    for program in [
        "set x 3\nputs [expr {$x in 3.0}]",
        "set x 3\nputs [expr {$x ni 3.0}]",
        "puts [expr {3.0 in 3.0}]",
        "puts [expr {1 in {01}}]",
        "puts [expr {1.5 in {1.5 2}}]",
        "puts [expr {1e10 in 1e10}]",
        "set l {1.0 2.0}\nputs [expr {1 in $l}]",
    ] {
        assert_eq!(reference(&tclsh, program), subject(program), "{program}");
    }
}

/// A refused operand is quoted by its **spelling** — **fixed**, and pinned here
/// so it stays that way.
///
/// tclsh keeps an operand's original string representation and quotes that, so
/// `expr {1e300 % 2}` names `1e300`. A literal used to reach the operator as an
/// `Op::LoadFloat` with no spelling left, and was named by
/// `runtime::format_double` instead — right for a *computed* double, wrong for
/// a written one. `Compiler::numeric_operand` now pushes the spelling itself
/// for a literal the formatter would not reproduce, and the numeric hook parses
/// it back on the way into the operation.
///
/// The claim this test used to make — that the two agreed for `1.0e-7` because
/// it "is already canonical" — was wrong: `format_double(1e-7)` is `1e-7`, and
/// that case is in the committed corpus (`message-compile-time-8af73dba`).
#[test]
fn a_refused_operand_is_quoted_by_its_spelling() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    for program in [
        // Exponential forms, whose canonical spelling differs.
        "puts [expr {1e300 % 2}]",
        "puts [expr {2.5e-3 % 2}]",
        "puts [expr {1e10 % 3}]",
        "puts [expr {1.0e-7 >> 1}]",
        "puts [expr {1e10 << 1}]",
        // A non-finite literal is refused as an operand, and named as written:
        // lower-case `nan`, not the `NaN` the formatter prints.
        "puts [expr {nan + 1}]",
        "puts [expr {nan * 2}]",
        "puts [expr {nan / 2}]",
        "puts [expr {nan | 1}]",
        "puts [expr {2 ** nan}]",
        // A canonical spelling is unaffected, and stays a native operand.
        "puts [expr {1.5 % 2}]",
        "puts [expr {0.5 | 1}]",
        // The same value reached through a variable was always quoted right.
        "set y 1e300\nputs [expr {$y % 2}]",
        "set n nan\nputs [expr {$n + 1}]",
        // An infinity is a valid operand, not a refused one, and still prints
        // as the formatter spells it rather than as the script wrote it.
        "puts [expr {inf}]",
        "puts [expr {inf + 1}]",
        "puts [expr {inf > 1}]",
    ] {
        assert_eq!(reference(&tclsh, program), subject(program), "{program}");
    }
}

/// `string replace` on an empty subject aborted the process.
///
/// `string replace {} -5 3` reached the tail computation with `last` clamped to
/// the empty subject's `end` of -1, cast that to `usize`, and `last + 1`
/// overflowed: `attempt to add with overflow`, a panic no `catch` can see. It is
/// the `panic` case of the four-run campaign (seed 3003 case 02453), and it was
/// still reachable at the tag the campaign ran against.
///
/// The whole first/last matrix is checked, not just the crashing input, because
/// the fix moves where the clamp happens and every row of it goes through that
/// line.
#[test]
fn fixed_string_replace_on_an_empty_subject_does_not_abort() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    // The crash itself, both with and without a replacement.
    agrees(&tclsh, "puts [string replace {} -5 3]", out("\n"));
    agrees(&tclsh, "puts [string replace {} -5 3 X]", out("X\n"));
    // The matrix around it. tclsh compiles a braced `catch` body, and `string
    // replace` has a bytecode form whose edge cases differ from the interpreted
    // command's, so the comparison is made where tclrs's own path is: compiled.
    agrees(
        &tclsh,
        "foreach spec {{{} -5 3} {{} 0 0} {{} -1 -1} {{} 1 2} {abc -5 3} \
         {abc -5 -1} {abc -5 0} {abc 1 10} {abc 2 1} {abc 0 2} {{} end end} \
         {{} end 0} {{} -2 -1} {abc -1 1} {abc end end}} {\n\
         set s [lindex $spec 0]\n\
         set f [lindex $spec 1]\n\
         set l [lindex $spec 2]\n\
         catch {string replace $s $f $l} r1\n\
         catch {string replace $s $f $l X} r2\n\
         puts \"[list $spec] -> [list $r1] [list $r2]\"\n\
         }",
        out("{{} -5 3} -> {} X\n\
             {{} 0 0} -> {} {}\n\
             {{} -1 -1} -> {} {}\n\
             {{} 1 2} -> {} {}\n\
             {abc -5 3} -> {} X\n\
             {abc -5 -1} -> abc abc\n\
             {abc -5 0} -> bc Xbc\n\
             {abc 1 10} -> a aX\n\
             {abc 2 1} -> abc abc\n\
             {abc 0 2} -> {} X\n\
             {{} end end} -> {} {}\n\
             {{} end 0} -> {} X\n\
             {{} -2 -1} -> {} {}\n\
             {abc -1 1} -> c Xc\n\
             {abc end end} -> ab abX\n"),
    );
}

/// `if`'s `else` keyword is optional, and its refusals are the command's.
///
/// Two defects in one handler, both found by the fuzzer's seed-1 case 00196
/// (`tests/fuzz_corpus/message-compile-time-7405ece0.tcl`), whose `if` sat in a
/// `switch` arm that never ran:
///
/// * The grammar was read from the synopsis in `if(n)` rather than from
///   `Tcl_IfObjCmd`. The interpreter takes the word after the last body as the
///   else script *whatever it says* — `if {$x} {a} {b}` is ordinary Tcl — and
///   answers `extra words after "else" clause in "if" command` when more than
///   one word is left. tclrs refused the whole form with a wording
///   (`expected "elseif" or "else", got …`) the interpreter has no equivalent
///   of.
/// * The two arity diagnostics quote the word they stopped at, which tclrs
///   answered with the literal `"if"` in every position.
///
/// Both are compile-time in tclrs and run-time in tclsh, so the refusals also
/// had to move: they are decided before the handler emits anything now, which
/// is what lets `Compiler::defer` carry them to the point the command runs.
#[test]
fn fixed_if_takes_the_interpreters_grammar_and_wording() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    // The keyword-less else, in each position it can stand.
    agrees(&tclsh, "if {0} {puts a} {puts b}", out("b\n"));
    agrees(&tclsh, "if {1} {puts a} {puts b}", out("a\n"));
    agrees(&tclsh, "if {0} then {puts a} {puts b}", out("b\n"));
    agrees(
        &tclsh,
        "if {0} {puts a} elseif {0} {puts b} {puts c}",
        out("c\n"),
    );
    agrees(&tclsh, "puts [if {0} {expr 5} {expr 6}]", out("6\n"));
    // The word quoted by each arity refusal.
    for program in [
        "if",
        "if {1}",
        "if {1} then",
        "if {1} {puts a} elseif",
        "if {1} {puts a} elseif {2}",
        "if {1} {puts a} else",
        "if {1} {puts a} else {puts b} extra",
        "if {1} {puts a} bogus {puts b}",
        "if {0} {} else {} junk",
    ] {
        let expected = reference(&tclsh, program);
        assert!(
            expected.error.starts_with("wrong # args: "),
            "{program}: tclsh no longer reports an arity error: {expected:?}"
        );
        assert_eq!(subject(program), expected, "{program}");
    }
    // Reached only when the command is: the case the fuzzer minimised had its
    // `if` inside a `switch` arm the subject never selected.
    agrees(
        &tclsh,
        "switch -- 3 {* {if {1} {puts a} else {puts b} extra} default {puts d}}",
        out("d\n"),
    );
    agrees(&tclsh, "puts [catch {if {1} {} else {} junk}]", out("1\n"));
}
