//! Differential coverage for the two places this frontend answered a question
//! it should have answered with something else: `string wordend` / `wordstart`
//! and `string is dict`, which were refused, and `switch`'s options, which were
//! parsed by a rule that was not the interpreter's.
//!
//! Same contract as the other differential suites: every program is run by both
//! tclsh and tclrs and the two outputs compared byte for byte, so nothing here
//! is an expectation written by hand. That matters most for `switch`'s option
//! boundary — the rule is positional, not lexical, and reading `switch(n)`
//! would not tell you that `switch -exact -7 {…}` matches on `-7`.

use std::path::PathBuf;
use std::process::Command;

/// The reference interpreter, or `None` when none is installed.
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

/// What tclsh prints for a program, stdout and stderr together so a refusal is
/// compared as carefully as an answer.
fn reference(tclsh: &PathBuf, program: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "tclrs-string-switch-{}-{:x}.tcl",
        std::process::id(),
        program.len() as u64 * 31 + program.as_bytes().first().copied().unwrap_or(0) as u64
    ));
    std::fs::write(&path, program).expect("write program");
    let out = Command::new(tclsh).arg(&path).output().expect("run tclsh");
    let _ = std::fs::remove_file(&path);
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The same, from this crate. A compile-time refusal carries a `(file … line N)`
/// suffix tclsh has no equivalent for, so only programs both engines *run* are
/// compared here; the refusals have their own test below.
fn subject(program: &str) -> String {
    match tclrs::eval(program) {
        Ok(outcome) => outcome.output,
        Err(e) => format!("{e}\n"),
    }
}

fn agree(tclsh: &PathBuf, programs: &[&str], what: &str) {
    let mut failures = Vec::new();
    for program in programs {
        let expected = reference(tclsh, program);
        let actual = subject(program);
        if expected != actual {
            failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs: {actual:?}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} {what} programs diverge:\n\n{}",
        failures.len(),
        programs.len(),
        failures.join("\n\n")
    );
}

/// `string wordend` and `string wordstart` over the shapes that decide them: a
/// word's interior and both edges, the separator between two words, an index
/// before and past the string, the `end` grammar, an empty subject, and the
/// three character kinds a word is made of — letters, digits and the
/// underscore, with a non-word character standing as a word of its own.
#[test]
fn word_boundaries_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    let mut programs = Vec::new();
    for subject in [
        "hello world",
        "a_b-c",
        "one",
        "",
        " ",
        "  two  ",
        "a1_b2",
        "-",
        "x--y",
    ] {
        for index in [
            "0", "1", "2", "3", "4", "5", "6", "-1", "-5", "99", "end", "end-1", "end+1",
        ] {
            programs.push(format!(
                "puts [string wordend {{{subject}}} {index}]:[string wordstart {{{subject}}} {index}]"
            ));
        }
    }
    let refs: Vec<&str> = programs.iter().map(String::as_str).collect();
    agree(&tclsh, &refs, "word-boundary");
}

/// `string is dict`, which is a structural test rather than a character class:
/// a list of an even number of elements. The malformed-list case matters most —
/// it is 0, not an error.
#[test]
fn string_is_dict_matches_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    agree(
        &tclsh,
        &[
            "puts [string is dict {}]",
            "puts [string is dict {a 1}]",
            "puts [string is dict {a 1 b}]",
            "puts [string is dict {a 1 b 2}]",
            "puts [string is dict {a {b c}}]",
            "puts [string is dict { a  1 }]",
            "puts [string is dict {a 1 a 2}]",
            "puts [string is dict \"\\{\"]",
            "puts [string is dict {a \"b}]",
            "puts [string is dict -strict {}]",
            "set d [dict create x 1 y 2]\nputs [string is dict $d]",
            "puts [string is dict [list a 1 b 2]]",
        ],
        "string-is-dict",
    );
}

/// `switch`'s options, and the boundary that decides where they stop.
///
/// The rule is positional: an argument is an option only while at least two
/// arguments follow it, so the same `-7` is an option in one arity and the
/// subject in another. Every combination of `-exact` / `-glob` / `-nocase` /
/// `--` is driven against a subject that distinguishes them.
#[test]
fn switch_options_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    let mut programs: Vec<String> = Vec::new();

    // The option boundary, at each arity that moves it.
    for form in [
        "switch -7 {a {puts a} default {puts d}}",
        "switch -exact -7 {a {puts a} default {puts d}}",
        "switch -exact -7 {-7 {puts hit} default {puts d}}",
        "switch -glob -1 {-* {puts hit} default {puts d}}",
        "switch -exact -nan {default {puts d}}",
        "switch -- -exact {-exact {puts hit} default {puts d}}",
        "switch -exact -- -glob {-glob {puts hit} default {puts d}}",
    ] {
        programs.push(form.to_string());
    }

    // The matching modes, with and without case folding, against subjects that
    // separate them: same letters different case, and a glob metacharacter.
    for opts in [
        "",
        "-exact",
        "-glob",
        "-nocase",
        "-nocase -exact",
        "-nocase -glob",
    ] {
        for subject in ["abc", "ABC", "aXc", "a*c"] {
            programs.push(format!(
                "switch {opts} -- {subject} {{abc {{puts exact}} a*c {{puts glob}} ABC {{puts upper}} default {{puts none}}}}"
            ));
        }
    }

    // The grouped and unspecified forms still behave, with options in front.
    for form in [
        "switch -nocase ABC abc {puts hit} default {puts d}",
        "switch -glob -- abc {a* - b* {puts shared} default {puts d}}",
        "switch -nocase -- {} {{} {puts empty} default {puts d}}",
    ] {
        programs.push(form.to_string());
    }

    let refs: Vec<&str> = programs.iter().map(String::as_str).collect();
    agree(&tclsh, &refs, "switch-option");
}

/// A bad option is refused with the interpreter's own wording, listing every
/// option `switch` has rather than the three this frontend used to name.
///
/// The *message* is compared here; that both engines now raise it at the same
/// moment — when the command runs, so `catch` sees it and an unreached `switch`
/// never does — is pinned by `switch_refusals_wait_for_the_command_to_run`.
#[test]
fn bad_switch_option_is_worded_as_tclsh_words_it() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    for program in [
        "switch -bogus x {default {puts d}}",
        "switch -nocase -bogus x y {default {puts d}}",
        "switch -exact -inf x {x {puts hit} default {puts d}}",
    ] {
        let expected = reference(&tclsh, program);
        let line = expected
            .lines()
            .next()
            .expect("tclsh reports something")
            .to_string();
        assert!(
            line.starts_with("bad option "),
            "{program}: tclsh no longer reports a bad option: {expected:?}"
        );
        let actual = tclrs::eval(program)
            .expect_err("a bad option is refused")
            .to_string();
        assert!(
            actual.starts_with(&line),
            "{program}\n  tclsh: {line:?}\n  tclrs: {actual:?}"
        );
    }
}

/// Every option `switch` names is one it runs. `-regexp`, `-matchvar` and
/// `-indexvar` were each refused by name here until they landed; each is pinned
/// as *working* now, so the day one starts refusing again this test says so.
///
/// What the two variables are *filled with* is compared against tclsh in
/// `tests/regexp_differential.rs`, which is where the capture information they
/// carry belongs. This test only pins that they are accepted at all.
#[test]
fn every_named_switch_option_runs() {
    for (src, expected) in [
        ("switch -regexp abc {a.c {puts hit}}", "hit\n"),
        (
            "switch -matchvar m -regexp abc {{a(.)c} {puts $m}}",
            "abc b\n",
        ),
        (
            "switch -indexvar i -regexp abc {{a(.)c} {puts $i}}",
            "{0 2} {1 1}\n",
        ),
        (
            "switch -matchvar m -indexvar i -regexp abc {b {puts \"$m $i\"}}",
            "b {1 1}\n",
        ),
    ] {
        assert_eq!(
            tclrs::eval(src)
                .unwrap_or_else(|e| panic!("{src} should run: {e}"))
                .output,
            expected,
            "{src}"
        );
    }
}

/// `switch`'s refusals are the command's, not the script's: `Tcl_SwitchObjCmd`
/// reaches every one of them while running, so a `switch` that is never
/// executed costs a script nothing and one that is can be caught.
///
/// This is the half of the compile-time-refusal class BUGS.md records that
/// `switch` used to sit outside: a bad option, an odd number of pattern words
/// and a `-` body with nothing after it all took the whole script down while it
/// was being read, where tclsh ran everything before them first.
#[test]
fn switch_refusals_wait_for_the_command_to_run() {
    for src in [
        "switch -bogus x {a b}",
        "switch -- x {a}",
        "switch -- x {a - }",
        "switch -matchvar m -glob abc {a* {}}",
        "switch -indexvar i -exact abc {abc {}}",
    ] {
        // Never reached: the script runs to its end and prints.
        let skipped = format!("if {{0}} {{{src}}}\nputs reached");
        assert_eq!(
            tclrs::eval(&skipped)
                .unwrap_or_else(|e| panic!("{src} should not be reached: {e}"))
                .output,
            "reached\n",
            "{src}"
        );
        // Reached inside `catch`: the answer is 1 and the script survives.
        let caught = format!("puts [catch {{{src}}}]");
        assert_eq!(
            tclrs::eval(&caught)
                .unwrap_or_else(|e| panic!("{src} should be catchable: {e}"))
                .output,
            "1\n",
            "{src}"
        );
    }
}
