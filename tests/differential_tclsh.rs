//! Differential test: word splitting must agree with the reference
//! interpreter, character for character.
//!
//! Each case is an argument list handed to a Tcl proc that prints its arguments
//! separated by an ASCII record separator. Whatever tclsh reports is the
//! expected value — no expectation here is written by hand, so a wrong guess
//! about the grammar cannot be baked in.
//!
//! Cases are limited to text with no variable or command substitution, since
//! those need a runtime (phase 2), and to words without `{*}`, whose expansion
//! is list parsing rather than word parsing (phase 3).
//!
//! The test reports a skip when no tclsh is installed, so a machine without Tcl
//! still runs the rest of the suite.

use std::path::PathBuf;
use std::process::Command;

const SEP: char = '\u{1e}';

const CASES: &[&str] = &[
    "a b c",
    "  a   b  ",
    "{a b} c",
    "{a {b c} d}",
    "{a\\}b}",
    "{}",
    "\"a b;c\"",
    "\"a\nb\"",
    "ab\"cd",
    "ab{cd",
    "a\\ b",
    "\\x41BC",
    "\\1011",
    "\\u00411",
    "\\U000041B",
    "\\q\\;\\$",
    "\\a\\b\\f\\n\\r\\t\\v",
    "\\u{1F600}",
    "a\\\n   b",
    "\"a\\\n   b\"",
    "{a\\\n   b}",
    "héllo {wörld 日本語}",
    "\\é",
    "{[list a]} {$x}",
    "\"\"",
    "{} {}",
    "a#b #c",
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

/// Run one argument list through tclsh and return the words it produced.
fn reference_words(tclsh: &PathBuf, args: &str) -> Vec<String> {
    let script = format!(
        "proc __p args {{\n    foreach a $args {{ puts -nonewline $a ; puts -nonewline \"\\x1e\" }}\n}}\n__p {args}\n"
    );
    let path = std::env::temp_dir().join(format!("tclrs-diff-{}.tcl", std::process::id()));
    std::fs::write(&path, script).expect("write case script");
    let out = Command::new(tclsh).arg(&path).output().expect("run tclsh");
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "tclsh rejected case {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut words: Vec<String> = stdout.split(SEP).map(str::to_string).collect();
    words.pop(); // trailing separator
    words
}

/// The literal words tclrs produces for the same argument list.
fn parsed_words(args: &str) -> Vec<String> {
    let src = format!("__p {args}\n");
    let script = tclrs::parse(&src).unwrap_or_else(|e| panic!("tclrs rejected {args:?}: {e}"));
    let command = script
        .commands
        .first()
        .unwrap_or_else(|| panic!("tclrs found no command in {args:?}"));
    command.words[1..]
        .iter()
        .map(|w| {
            w.as_literal()
                .unwrap_or_else(|| panic!("word {w:?} in {args:?} is not literal"))
                .to_string()
        })
        .collect()
}

#[test]
fn word_splitting_matches_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };

    let mut failures = Vec::new();
    for case in CASES {
        let expected = reference_words(&tclsh, case);
        let actual = parsed_words(case);
        if expected != actual {
            failures.push(format!(
                "case {case:?}\n  tclsh: {expected:?}\n  tclrs: {actual:?}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} cases diverge:\n{}",
        failures.len(),
        CASES.len(),
        failures.join("\n")
    );
}

/// Scripts tclsh refuses to parse must also fail here, and for the same reason.
#[test]
fn parse_errors_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };

    let cases = [
        "list {abc",
        "list [a b",
        "list \"abc",
        "set v $a(",
        "set v {a}b",
        "set v \"a\"b",
        "set v $a(x(y))",
    ];

    let mut failures = Vec::new();
    for case in cases {
        let path = std::env::temp_dir().join(format!("tclrs-err-{}.tcl", std::process::id()));
        std::fs::write(&path, case).expect("write case script");
        let out = Command::new(&tclsh).arg(&path).output().expect("run tclsh");
        let _ = std::fs::remove_file(&path);
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let reference = stderr.lines().next().unwrap_or_default().trim().to_string();

        match tclrs::parse(case) {
            Ok(_) => failures.push(format!(
                "case {case:?}: tclsh said {reference:?}, tclrs accepted it"
            )),
            Err(e) if e.msg != reference => failures.push(format!(
                "case {case:?}: tclsh said {reference:?}, tclrs said {:?}",
                e.msg
            )),
            Err(_) => {}
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
