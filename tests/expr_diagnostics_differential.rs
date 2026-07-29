//! Differential coverage for what `expr` says when it refuses, and for the
//! boolean words it must not refuse at all.
//!
//! Same contract as the other differential suites: every program is run by both
//! tclsh and tclrs and the outputs compared byte for byte, so no expectation
//! here is written by hand. That matters especially for these, because a
//! diagnostic is three lines in tclsh 9.0.4 — the message, the expression it was
//! reading, and for a bare word a spelling hint — and a comparison of only the
//! first line would have called the two identical while a script that prints a
//! caught message saw something different.
//!
//! The programs `catch` and print, rather than letting the error reach stderr,
//! because the caught value is exactly the part a script can observe: tclsh adds
//! a stack trace beyond it that is not part of the result.

use std::path::PathBuf;
use std::process::Command;

/// Expressions whose *refusal* is compared. Each is spliced into a `catch` and
/// printed, so the whole multi-line message is what the two engines are held to.
const REFUSED: &[&str] = &[
    // A bare word, which is the one diagnostic carrying the third `should be` line.
    "a",
    "abc",
    "end",
    "0x",
    "1_",
    "  a  ",
    "1 x",
    "1 nope",
    "1 o",
    "\"a\" bogus",
    "$x y1",
    // The `_@_` marker, which is placed at the parse position on the second line
    // and left unexpanded on the first.
    "1 +",
    "1 + ",
    "1 + * 2",
    "1 2",
    "1 & &",
    "1 ? 2",
    "1 **",
    // No marker: the diagnostic's own text does not end in `at _@_`.
    "",
    "(1",
    "1)",
    "\u{1F600}",
    "1 + \u{1F600}",
    // Multi-byte operands, where a marker inserted at a byte offset would split
    // a character if the offset were used as given.
    "héllo",
    "1 + héllo",
    "\u{1F600} + 1",
    // Function-call parens: an unterminated list is the open paren, a promised
    // argument that never arrives is the argument, and a second operand inside
    // the list is the missing operator between them.
    "sin(",
    "sin(1",
    "sin(1,",
    "sin(,",
    "sin(1,)",
    "sin(1 2",
    "max(1,2",
    "max(1,,2",
    "sin((1",
    "sin(1))",
    "1 + (",
];

/// Boolean words are *operands* in `expr(n)`, so these are answers rather than
/// refusals — and a script sees the word's own spelling, not `1` or `0`.
const BOOLEAN_WORDS: &[&str] = &[
    "yes",
    "no",
    "true",
    "false",
    "on",
    "off",
    "YES",
    "Off",
    "TrUe",
    "t",
    "f",
    "y",
    "n",
    "tr",
    "fa",
    "ye",
    "of",
    "tru",
    "fals",
    // `o` is not one: `on` and `off` both start with it, so it stays a bare word.
    "o",
    // As operands of everything that can take one.
    "true && false",
    "yes || no",
    "!yes",
    "!off",
    "on ? 1 : 2",
    "off ? 1 : 2",
    "yes eq \"yes\"",
    "yes ne \"1\"",
    "yes + 1",
    "no - 1",
    "y * 3",
    "t < 2",
    "yes in {yes no}",
    // A boolean word where a condition is wanted, which is the rule these share
    // with `if` — reached through a different op, and it must agree.
    "1 && yes",
    "0 || t",
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

/// What tclsh prints for one program. The scratch file is named by this test's
/// own index so that a sibling suite running concurrently cannot read it — two
/// tests once shared `usize::MAX` and read each other's output.
fn reference(tclsh: &PathBuf, index: usize, program: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "tclrs-expr-diag-{}-{index}.tcl",
        std::process::id()
    ));
    std::fs::write(&path, program).expect("write program");
    let out = Command::new(tclsh).arg(&path).output().expect("run tclsh");
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "tclsh rejected the program itself:\n{program}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Run the whole batch as one program per engine, so the suite costs two
/// processes rather than two per expression.
fn batch(exprs: &[&str]) -> String {
    let mut program = String::new();
    for e in exprs {
        // The expression is braced, so it reaches `expr` as the script wrote it;
        // `catch` keeps a refusal observable, and the delimiters make an empty
        // result distinguishable from a missing one.
        program.push_str("catch {expr {");
        program.push_str(e);
        program.push_str("}} m\nputs \"<$m>\"\n");
    }
    program
}

#[test]
fn refused_expressions_report_what_tclsh_reports() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    let program = batch(REFUSED);
    let expected = reference(&tclsh, 0, &program);
    let outcome = tclrs::eval(&program).expect("tclrs runs the program");
    assert_eq!(
        outcome.output, expected,
        "a refused expression must carry tclsh's own context lines"
    );
}

#[test]
fn boolean_words_are_operands() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    let program = batch(BOOLEAN_WORDS);
    let expected = reference(&tclsh, 1, &program);
    let outcome = tclrs::eval(&program).expect("tclrs runs the program");
    assert_eq!(
        outcome.output, expected,
        "a boolean word is an operand carrying its own spelling"
    );
}

/// The marker's position is a byte offset into the expression, and inserting it
/// must not split a character — this is the case that panicked before
/// `char_boundary` floored it.
#[test]
fn the_marker_never_splits_a_character() {
    for e in ["héllo + ", "\u{1F600} + ", "日本語 +", "ñ +", "1 + é"] {
        let program = format!("catch {{expr {{{e}}}}} m\nputs $m\n");
        // Not compared against tclsh here — the point is that lowering answers
        // at all rather than aborting the process on a split character.
        let outcome = tclrs::eval(&program);
        assert!(
            outcome.is_ok(),
            "{e:?} should report, not abort: {outcome:?}"
        );
    }
}
