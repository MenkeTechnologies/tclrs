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
    // The RUN the named word is read from. tclsh names the whole run of word
    // characters the leftover sits in, not just the leftover: `1a` is
    // `invalid bareword "1a"` where naming from the parse position said `a`.
    // The run stops at anything that is not a word character, and a run a `.`
    // introduces is the tail of a decimal number, where tclsh names the
    // leftover alone — which is the one place the run and the word disagree.
    "1a",
    "3q",
    "007z",
    "12abc",
    "1e",
    "1e+",
    "1e1e",
    "1e10x",
    "1_5a",
    "1+2a",
    "2**3a",
    "1.5a",
    ".5a",
    "0.a",
    "1.a",
    "2.a",
    "12.5x",
    "1.5e3a",
    "1.5e3z",
    "1e3.5a",
    "1.5_",
    "1y",
    "1true",
    "0true",
    "2no",
    // The radix guess appended after the `should be` line. It is about the digit
    // that FOLLOWS the prefix, so it appears only when that character is absent
    // or is not a digit of the radix — `0b`, `0b2`, `0bz` and `0b_101` get one,
    // `0b1z` and `0b1_1z` do not. Only `0b` and `0o`, and only in lower case.
    "0b_101",
    "0b2",
    "0b",
    "0bz",
    "0b1z",
    "0b1_1z",
    "0B_101",
    "0B2",
    "0o_17",
    "0o8",
    "0o9",
    "0o",
    "0o1z",
    "0o7z",
    "0O8",
    "0x_1f",
    "0x1z",
    "0x1fz",
    "0xzz",
    "0X",
    "0d_9",
    "0dz",
    "0d",
    "0x_10",
    "0x1_",
    // The WINDOW tclsh quotes the expression through. Anything 25 bytes or
    // longer is cut to 22 with `...` for the rest, and the cut is applied to
    // three pieces independently: what precedes the error (cut from the left),
    // the word or character the error names (cut from the right, and cut in the
    // message and hint too), and what follows it (cut from the right). Quoting
    // the expression whole was wrong for every diagnostic on an expression this
    // long, not only the one the fuzzer happened to reduce to.
    "1 + 1 + 1 + 1 + 1 + 1 + 1 + zzz9 + 1 + 1 + 1 + 1 + 1 + 1 + 1",
    "1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + * 1 + 1 + 1 + 1 + 1 + 1 + 1",
    "(1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1",
    "zzz + $bbbbbbbbbbbbbbbbbbbb",
    "zzz + $bbbbbbbbbbbbbbbbbbbbb",
    "$aaaaaaaaaaaaaaaaaaaaaaaa + zzz",
    "$aaaaaaaaaaaaaaaaaaaaaaaaa + zzz",
    // A named word longer than the window is cut wherever it appears.
    "1 in xxxxxxxxxxxxxxxxxxxxxxxx",
    "1 in xxxxxxxxxxxxxxxxxxxxxxxxx",
    "1 in xxxxxxxxxxxxxxxxxxxxxxxxx\u{65e5}",
    "1 in xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    // The cut is in BYTES, so where it lands inside a character it moves back to
    // that character's start. These are the widths that make it land there:
    // 2 bytes for the accented Latin, 3 for the CJK, 4 for the emoji.
    "[string length {na\u{ef}ve caf\u{e9}}] in \u{65e5}a",
    "na\u{ef}ve + caf\u{e9} + na\u{ef}ve + caf\u{e9} + zzz",
    "zzz + na\u{ef}ve + caf\u{e9} + na\u{ef}ve + caf\u{e9}",
    "\u{65e5}\u{672c}\u{8a9e} + \u{65e5}\u{672c}\u{8a9e} + \u{65e5}\u{672c}\u{8a9e} + zzz",
    "zzz + \u{65e5}\u{672c}\u{8a9e} + \u{65e5}\u{672c}\u{8a9e} + \u{65e5}\u{672c}\u{8a9e}",
    "zzz + \u{65e5}\u{672c}\u{8a9e}\u{65e5}\u{672c}\u{8a9e}\u{65e5}\u{672c}\u{8a9e}\u{65e5}\u{672c}\u{8a9e}",
    "\u{65e5}\u{672c}\u{8a9e}\u{65e5}\u{672c}\u{8a9e}\u{65e5}\u{672c}\u{8a9e}\u{65e5}\u{672c}\u{8a9e} + zzz",
    "\u{e9} + \u{e9} + \u{e9} + \u{e9} + \u{e9} + \u{e9} + \u{e9} + \u{e9} + \u{e9} + zzz",
    "zzz + \u{e9} + \u{e9} + \u{e9} + \u{e9} + \u{e9} + \u{e9} + \u{e9} + \u{e9} + \u{e9}",
    "\u{1F600} + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1",
    "1 + 1 + 1 + 1 + 1 + 1 + 1 + \u{1F600}",
    // `invalid character` names one character, and that character is the middle
    // piece just as a word is — counting it as the head of the tail made this
    // diagnostic's budget look one byte wider than every other one's.
    "@",
    "1 + @",
    "@ $bbbbbbbbbbbbbbbbbbbbbb",
    "@ $bbbbbbbbbbbbbbbbbbbbbbb",
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
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
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
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
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

/// The operands whose refusal names the text the SCRIPT wrote rather than the
/// number it parses to.
///
/// tclsh reports `cannot use floating-point value "inf" as operand of "~"` for
/// `expr {~inf}` and `"1.50"` for `expr {~1.50}` — the literal, verbatim, in
/// whatever case and with whatever trailing zeros it was written. The unary
/// arm read the parsed double back instead and answered `"Inf"` and `"1.5"`,
/// while the binary arm (`expr {infinity | 1}`) already carried the spelling.
///
/// A COMPUTED operand has no spelling to carry and is named canonically, which
/// is why `~-inf` is `"-Inf"` in both engines — the `-` makes it a computation.
/// Both halves are in the list, so a fix that simply stopped canonicalising
/// everywhere would fail here.
const OPERAND_SPELLINGS: &[&str] = &[
    "~inf",
    "~Inf",
    "~INF",
    "~InF",
    "~infinity",
    "~Infinity",
    "~nan",
    "~NaN",
    "~1.5",
    "~1.50",
    "~1e3",
    "~0.10",
    // Computed, so the canonical spelling is the right answer.
    "~-inf",
    "~(1/0.0)",
    "~(0.0/0.0)",
    // The binary arm, which already agreed — kept so a change there is caught.
    "infinity | 1",
    "1 | infinity",
    "inf % 2",
    "1.50 << 1",
];

#[test]
fn a_refused_operand_is_named_as_the_script_spelled_it() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    let program = batch(OPERAND_SPELLINGS);
    let expected = reference(&tclsh, 3, &program);
    let outcome = tclrs::eval(&program).expect("tclrs runs the program");
    assert_eq!(
        outcome.output, expected,
        "a refused operand carries the literal's own spelling"
    );
}
