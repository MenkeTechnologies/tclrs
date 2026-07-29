//! Differential execution for `expr`'s lexer: the numeric literal grammar and
//! the wording of what it refuses.
//!
//! Same contract as the other differential suites — every program is run by both
//! tclsh and tclrs and compared byte for byte, so no expectation here is written
//! by hand. That matters more for this corner than for most, because the rule
//! being pinned is not one a reading of `expr(n)` yields: Tcl 9 allows `_`
//! between two digits and nowhere else, so `1_000_000` and even `1__0` are
//! numbers while `0x_10`, `1_`, `1e_10` and `1_.5` are bare words — and the word
//! the refusal *names* is the run the separator sits in, which for `1.5_` is
//! `5_` rather than the whole literal.
//!
//! Each program is `catch`ed and prints the code and the message, so a program
//! that tclsh refuses is still comparable rather than being an aborted run.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Literals that are numbers, and literals that are not. Split only for reading;
/// every one is compared the same way.
const LITERALS: &[&str] = &[
    // ── the separator between digits, which is where it is allowed ──────────
    "1_0",
    "1_000_000",
    "1__0",
    "00_7",
    "0x1_0",
    "0b1_0",
    "0o1_7",
    "0d1_9",
    "1_0.5",
    "1.5_0",
    ".5_0",
    "1e1_0",
    "1_0e1_0",
    "12_34.56_78e9_0",
    // ── the same separator where it is not ──────────────────────────────────
    "0x_10",
    "0b_10",
    "0o_17",
    "0d_9",
    "0x_",
    "1_",
    "0x1_",
    "0x1_0_",
    "1_.5",
    "1._5",
    "1.5_",
    "1e_10",
    "1e10_",
    "1_e10",
    ".5_",
    "_1",
    "_",
    "_abc",
    // ── the radix prefixes with and without digits ──────────────────────────
    "0x10",
    "0d9",
    "0x",
    "0b",
    "0o",
    "0d",
    "0xg",
    // ── a point that leads somewhere, and one that does not ─────────────────
    "1.",
    "1.e5",
    ".5",
    ".",
    ".x",
    "1.x",
    // ── trailing text once the expression is complete ───────────────────────
    "1 x",
    "1 abc",
    "1 e5",
    "1 1",
    "1 $x",
    "1 \"a\"",
    "1.5x",
    "(1)x",
    // ── the letter-spelled operators, which must keep working ───────────────
    "1 eq 2",
    "1 ne 2",
    "2 lt 10",
    "2 gt 10",
    "1 in {1 2}",
    "1 ni {2}",
    "inf",
    "inf > 1",
    // ── arithmetic on separated literals, so the value is compared too ──────
    "1_0 + 1",
    "0x1_0 * 2",
    "1_0.5 + 0.5",
    "1_0 eq \"1_0\"",
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

/// The program each literal is compared through. `catch` keeps a refusal
/// comparable: the completion code and the message are printed rather than
/// ending the run, so tclsh's runtime refusal and this frontend's are read the
/// same way.
fn program(literal: &str) -> String {
    format!("puts [list [catch {{expr {{{literal}}}}} m] $m]\n")
}

fn reference(tclsh: &PathBuf, src: &str) -> String {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "tclrs-exprlex-{}-{}.tcl",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path, src).expect("write program");
    let out = Command::new(tclsh).arg(&path).output().expect("run tclsh");
    let _ = std::fs::remove_file(&path);
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// What this frontend answers. A literal it refuses while *compiling* never
/// reaches `catch`, so the refusal arrives as an error rather than as output;
/// both are reduced to the message so the two engines are compared on what they
/// said, not on which channel carried it.
fn subject(src: &str) -> String {
    match tclrs::eval(src) {
        Ok(outcome) => outcome.output,
        Err(e) => e.to_string(),
    }
}

/// The first line of a refusal, which is the part the two engines are held to
/// here.
///
/// Two differences are stripped rather than compared, because each is a
/// separate class already recorded in BUGS.md and neither is this suite's
/// subject: tclsh appends the expression and a suggestion (`in expression
/// "1 _@_$x"; should be "$x" or ...`), and this frontend appends ` (line N)`.
/// What is left is the diagnostic itself — the wording and the word it names,
/// which is what the lexer decides.
fn message_of(text: &str) -> String {
    let mut line = text.trim_end_matches('\n');
    // `catch` yields a braced list once the message spans lines.
    line = line.strip_prefix('{').unwrap_or(line);
    if let Some(rest) = line.strip_prefix("1 ") {
        line = rest;
    }
    let first = line.lines().next().unwrap_or_default().trim();
    let first = first.strip_prefix('{').unwrap_or(first);
    // ` (line N)`, which this frontend carries through the library.
    match first.rfind(" (line ") {
        Some(at) if first.ends_with(')') => first[..at].trim().to_string(),
        _ => first.trim_end_matches('}').trim().to_string(),
    }
}

#[test]
fn expr_literal_grammar_matches_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };

    let mut failures = Vec::new();
    for literal in LITERALS {
        let src = program(literal);
        let expected = reference(&tclsh, &src);
        let actual = subject(&src);
        // Identical output is parity outright. Otherwise the two must at least
        // have refused with the same wording — the remaining difference being
        // *when* the refusal happens, which is the compile-time deferral class
        // recorded in BUGS.md and not this suite's subject.
        if actual == expected {
            continue;
        }
        let (want, got) = (message_of(&expected), message_of(&actual));
        if want == got {
            continue;
        }
        failures.push(format!(
            "expr {{{literal}}}\n  tclsh: {want:?}\n  tclrs: {got:?}"
        ));
    }

    assert!(
        failures.is_empty(),
        "{} of {} literals diverge:\n\n{}",
        failures.len(),
        LITERALS.len(),
        failures.join("\n\n")
    );
}

/// The separated spellings are the same *numbers* as the plain ones, which is
/// the half of the rule the message comparison above cannot see.
#[test]
fn separated_literals_are_the_numbers_they_spell() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };

    let src = "\
puts [expr {1_0 == 10}]
puts [expr {1__0 == 10}]
puts [expr {0x1_0 == 16}]
puts [expr {0b1_0 == 2}]
puts [expr {0o1_7 == 15}]
puts [expr {0d1_9 == 19}]
puts [expr {1_0.5 == 10.5}]
puts [expr {1e1_0 == 1e10}]
puts [expr {12_34.56_78e9_0}]
puts [expr {1_000_000 + 1}]
";
    assert_eq!(subject(src), reference(&tclsh, src));
}
