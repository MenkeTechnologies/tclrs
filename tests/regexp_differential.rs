//! Differential execution for `regexp` and `regsub`.
//!
//! Same contract as the other differential suites: every program is run by both
//! tclsh and tclrs and the two outputs are compared byte for byte, so no
//! expectation about matching, indices or substitution is written by hand here.
//!
//! That matters more for regular expressions than for anything else in this
//! crate, because the engine underneath is *not* the one Tcl uses. tclsh
//! matches with Henry Spencer's ARE; this frontend translates onto the `regex`
//! crate. Two of the defaults differ silently — a pattern that compiles under
//! both and means something else — and only the reference interpreter settles
//! which is right:
//!
//! * `.` matches a newline in ARE and does not in Rust.
//! * `-line` is `-lineanchor` *and* `-linestop`, so it moves both `^`/`$` and
//!   what `.` will cross.
//!
//! `empty_match_iteration_matches_tclsh` pins the third one, which is not a
//! default but a loop: where an empty match leaves the cursor, and whether the
//! position at the very end of the subject is one that matches.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Matching, the switches, and the two commands' return values.
const PROGRAMS: &[&str] = &[
    // The return value is 1/0, a count under -all, and the text under -inline.
    "puts [regexp {b} abc]",
    "puts [regexp {z} abc]",
    "puts [regexp {} abc]",
    "puts [regexp -all {a} aaa]",
    "puts [regexp -inline {(a)(b)} xab]",
    "puts <[regexp -inline {z} abc]>",
    "puts [regexp -all -inline {(a)(b)} abab]",
    "puts [regexp -all -inline {\\d+} \"a12b345\"]",
    // Match variables, including the ones that do not participate.
    "set m {}\nregexp {b} abc m\nputs $m",
    "set a {}\nset b {}\nregexp {(a)(b)} ab a b\nputs \"$a|$b\"",
    "set a {}\nset b {}\nset c {}\nregexp {(a)} a a b c\nputs \"$a|$b|$c\"",
    "set a {}\nset b {}\nset c {}\nregexp {(a)(z)?} a a b c\nputs \"$a|$b|$c\"",
    "set m {}\nregexp -all {a} aaa m\nputs $m",
    "set m {}\nputs [regexp {z} abc m]\nputs <$m>",
    // Indices are character offsets, not byte offsets, and an unmatched group
    // is -1 -1.
    "set m {}\nregexp -indices {b} abc m\nputs $m",
    "set m {}\nregexp -indices {b} \u{e9}b m\nputs $m",
    "set a {}\nset b {}\nset c {}\nregexp -indices {(a)(z)?} a a b c\nputs [list $a $b $c]",
    "puts [regexp -inline -indices {(b)} abc]",
    "set m {}\nregexp -indices {b*} ac m\nputs $m",
    // The newline defaults, which is where the two engines disagree by default.
    "puts [regexp {a.b} \"a\\nb\"]",
    "puts [regexp -line {a.b} \"a\\nb\"]",
    "puts [regexp -linestop {a.b} \"a\\nb\"]",
    "puts [regexp -lineanchor {a.b} \"a\\nb\"]",
    "puts [regexp {^b} \"a\\nb\"]",
    "puts [regexp -line {^b} \"a\\nb\"]",
    "puts [regexp -lineanchor {^b} \"a\\nb\"]",
    "puts [regexp {a$} \"a\\nb\"]",
    "puts [regexp -line {a$} \"a\\nb\"]",
    "puts [regexp -all -line {^a} \"a\\na\"]",
    // -start, whose offset does not move where an anchor thinks the string
    // begins.
    "puts [regexp -start 1 {^b} ab]",
    "puts [regexp -start 1 {b} ab]",
    "puts [regexp -start -3 {a} ab]",
    "puts [regexp -start 5 {a} ab]",
    "set m {}\nregexp -start 1 -indices {b} ab m\nputs $m",
    "puts [regsub -start 1 -all {a} aaa X]",
    // The rest of the switches.
    "puts [regexp -nocase {ABC} abc]",
    "set m {}\nregexp -nocase -indices {B} ab m\nputs $m",
    "puts [regexp -expanded { a \\# b } ab]",
    "puts [regexp -- {-x} -x]",
    "puts [regexp {a{2,3}} aaa]",
    // ARE spellings that translate rather than pass through.
    "puts [regexp {\\yfoo\\y} \"a foo b\"]",
    "puts [regexp {[[:digit:]]+} ab123]",
    "puts [regexp {(?i)ABC} abc]",
    "puts [regexp {\\Aab\\Z} ab]",
    "puts [regexp {\\d+} ab123]",
    "puts [regexp {\\w+} !!abc]",
    "puts [regexp {\\s} \"a b\"]",
    // The directors, which are ARE-only.
    "puts [regexp {***=a.b} {a.b}]",
    "puts [regexp {***=a.b} {axb}]",
    "puts [regexp {***:a+} aaa]",
    // regsub: the return value, the variable form, and the replacement spec.
    "puts [regsub {b+} abbc {[&]}]",
    "puts [regsub {(b+)} abbc {<\\1>}]",
    "puts [regsub -all {b} abbc X]",
    "set v {}\nputs [regsub -all {a} aaa X v]\nputs $v",
    "set v {}\nputs [regsub {z} abc X v]\nputs <$v>",
    "puts [regsub {b} abc {\\&}]",
    "puts [regsub {b+} abbc {[\\0]}]",
    "puts [regsub {(a)(z)?} a {<\\1|\\2>}]",
    "puts [regsub -all {,} a,b,c {;}]",
    "puts [regsub -nocase {B} abc X]",
    "puts [regsub {b} abc {\\\\}]",
    // A subject and a pattern that are values rather than literals.
    "set p {b+}\nset s abbc\nputs [regexp $p $s]",
    "set p {b+}\nset s abbc\nputs [regsub $p $s X]",
    // The commands that take a regular expression without being one.
    "puts [switch -regexp abc {^a {list one} ^b {list two} default {list none}}]",
    "puts [switch -regexp bcd {^a {list one} ^b {list two} default {list none}}]",
    "puts [switch -regexp zzz {^a {list one} ^b {list two} default {list none}}]",
    "puts [switch -regexp -- abc {b+ {list hit} default {list miss}}]",
    "puts [switch -regexp -nocase ABC {^a {list ci} default {list no}}]",
    "puts [switch -regexp abc {{^[abc]+$} {list class} default {list no}}]",
    "puts [lsearch -regexp {abc bcd} {^b}]",
    "puts [lsearch -all -regexp {abc bcd cde} {c}]",
    "puts [lsearch -regexp {abc bcd} {^z}]",
];

/// Programs whose *error* must agree with the interpreter's, message included.
const ERRORS: &[&str] = &[
    "puts [catch {regexp -bogus {a} b} e]\nputs $e",
    "puts [catch {regsub -inline {a} abc X} e]\nputs $e",
    "puts [catch {regsub -bogus {a} b X} e]\nputs $e",
    // `-about` is a `regexp` option and not a `regsub` one, so `regsub -about`
    // really is a bad option and its wording is the reference implementation's.
    // The `regexp` half cannot be here — tclsh answers it — and is pinned as a
    // named refusal by `unsupported_are_constructs_are_refused`.
    "puts [catch {regsub -about {a} b X} e]\nputs $e",
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

/// What tclsh prints for a program, run from a file so the shell never sees it.
fn reference(tclsh: &PathBuf, program: &str) -> String {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("tclrs-regexp-{}-{n}.tcl", std::process::id()));
    std::fs::write(&path, program).expect("write program");
    let out = Command::new(tclsh).arg(&path).output().expect("run tclsh");
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "tclsh rejected program:\n{program}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn compare_all(programs: &[&str], what: &str) {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    let mut failures = Vec::new();
    for program in programs {
        let expected = reference(&tclsh, program);
        match tclrs::eval(program) {
            Ok(outcome) if outcome.output == expected => {}
            Ok(outcome) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs: {:?}",
                outcome.output
            )),
            Err(e) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs failed: {e}"
            )),
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

#[test]
fn regexp_matches_tclsh() {
    compare_all(PROGRAMS, "regexp");
}

#[test]
fn regexp_errors_match_tclsh() {
    compare_all(ERRORS, "error");
}

/// Where an empty match leaves the cursor, and whether the end of the subject
/// is a position that matches.
///
/// The two commands disagree with each other in tclsh — `regexp -all {x*} ab`
/// is 2 while `regsub -all {x*} ab -` substitutes three times — and the
/// literally empty pattern disagrees with every other pattern that can match
/// empty. `(?:)`, `()` and `a{0}` all behave like `x*`; only `{}` does not.
/// None of that is derivable, so it is measured.
#[test]
fn empty_match_iteration_matches_tclsh() {
    let mut programs: Vec<String> = Vec::new();
    for pattern in ["", "x*", "z*", "(?:)", "()", "a{0}", "b*", "a*"] {
        for subject in ["", "a", "ab", "abc", "aab"] {
            programs.push(format!(
                "puts [regexp -all {{{pattern}}} \"{subject}\"]\n\
                 puts [regsub -all {{{pattern}}} \"{subject}\" -]\n\
                 puts <[regexp -all -inline {{{pattern}}} \"{subject}\"]>\n"
            ));
        }
    }
    let refs: Vec<&str> = programs.iter().map(String::as_str).collect();
    compare_all(&refs, "empty-match");
}

/// Multi-byte subjects, where a byte offset and a character offset differ and
/// an empty-match step of one byte would land inside a character.
#[test]
fn character_offsets_match_tclsh() {
    let mut programs: Vec<String> = Vec::new();
    for subject in ["\u{e9}b", "a\u{e9}b", "\u{1f600}b", "\u{65e5}\u{672c}b"] {
        programs.push(format!(
            "set m {{}}\nregexp -indices {{b}} \"{subject}\" m\nputs $m\n\
             puts [regexp -all {{x*}} \"{subject}\"]\n\
             puts [regsub -all {{x*}} \"{subject}\" -]\n\
             puts [string length [regsub -all {{}} \"{subject}\" -]]\n"
        ));
    }
    let refs: Vec<&str> = programs.iter().map(String::as_str).collect();
    compare_all(&refs, "character-offset");
}

/// What this frontend will not approximate must say so, and must say it at the
/// point of use rather than matching something wrong.
///
/// These are the constructs a finite-automaton engine cannot express. tclsh
/// accepts all of them, so there is no reference wording to copy — what is
/// pinned here is that the refusal happens and names the construct.
#[test]
fn unsupported_are_constructs_are_refused() {
    for (program, expected) in [
        ("puts [regexp {(a+)\\1} aaaa]", "back-reference"),
        ("puts [regexp {a(?=b)} ab]", "look-ahead"),
        ("puts [regexp {a(?!b)} ac]", "look-ahead"),
        ("puts [regexp {\\mfoo} \"a foo\"]", "word-start"),
        ("puts [regexp {foo\\M} \"foo b\"]", "word-end"),
        ("puts [regexp {[[.hyphen-minus.]]} -]", "collating element"),
        ("puts [regexp {[[=a=]]} a]", "equivalence class"),
        // Not a construct but an option, and refused for a related reason: its
        // second element is the reference engine's report on its own compile.
        // Named rather than reported as a bad option, which is what it was —
        // and `bad option "-about": must be … -about …` contradicts itself.
        ("puts [regexp -about {(a)}]", "regexp -about is not supported yet"),
    ] {
        let err = tclrs::eval(program)
            .map(|o| format!("no error, printed {:?}", o.output))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(expected),
            "expected a refusal naming {expected:?} for {program}, got: {err}"
        );
    }
}
