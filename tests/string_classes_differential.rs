//! Differential execution for `string is`'s character classes, its
//! `-failindex` option, and the `-stride` option of `lsort` and `lsearch`.
//!
//! Same contract as the other differential suites: every program is run by both
//! tclsh and tclrs and the two outputs are compared byte for byte, so no
//! expectation here is written by hand.
//!
//! The character classes need that more than most. Tcl defines them over
//! Unicode *general categories* — the `ALPHA_BITS` / `PUNCT_BITS` / `GRAPH_BITS`
//! unions in `tclUtf.c` — and not over the derived properties Rust's standard
//! library exposes, so `char::is_alphabetic` answers a different question from
//! `string is alpha`. The sweep below drives thousands of code points through
//! both engines rather than sampling the ones a person would think of.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

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

/// A subject as a Tcl word that survives whatever is in it. Braces cannot be
/// used — half the subjects here are deliberately unbalanced — so it goes in
/// double quotes with the four characters that mean something there escaped.
fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        if matches!(c, '"' | '\\' | '$' | '[') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// What tclsh prints for a program. The scratch file is named by process and a
/// counter of this suite's own, so two tests never read each other's file.
fn reference(tclsh: &PathBuf, program: &str) -> String {
    let index = NEXT.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("tclrs-classes-{}-{index}.tcl", std::process::id()));
    std::fs::write(&path, program).expect("write program");
    let out = Command::new(tclsh).arg(&path).output().expect("run tclsh");
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "tclsh rejected the program:\n{program}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn subject(program: &str) -> String {
    tclrs::eval(program)
        .unwrap_or_else(|e| panic!("tclrs failed:\n{program}\n{e}"))
        .output
}

/// The eleven classes that consult the category tables, swept over a range of
/// code points wide enough to cross every category boundary that matters:
/// ASCII, Latin-1, the punctuation and symbol blocks, CJK, and an astral plane.
///
/// `string is` takes its class as a literal here, so the program spells each
/// class out rather than looping a variable over them.
#[test]
fn character_classes_match_tclsh_across_code_points() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };

    const CLASSES: &[&str] = &[
        "alnum", "alpha", "control", "digit", "graph", "lower", "print", "punct", "space", "upper",
        "wordchar",
    ];
    // Ranges chosen for their boundaries, not their size: each one straddles a
    // change of general category.
    const RANGES: &[(u32, u32)] = &[
        (0x00, 0x100), // ASCII and the C1 controls
        // General punctuation through the currency symbols, split around
        // U+20C1: that one is in the enumerated set this build refuses, so a
        // sweep that crossed it would be testing the refusal, which
        // `a_code_point_beyond_our_tables_is_refused` does on its own.
        (0x2000, 0x20C1),
        (0x20C2, 0x2100),
        (0x2150, 0x2190),   // number forms into arrows
        (0x3000, 0x3040),   // CJK punctuation into kana
        (0xFF00, 0xFF70),   // fullwidth forms
        (0x1F600, 0x1F610), // an astral plane
    ];

    let mut program = String::new();
    for class in CLASSES {
        for &(lo, hi) in RANGES {
            program.push_str(&format!(
                "for {{set i {lo}}} {{$i < {hi}}} {{incr i}} {{\n\
                 \x20   puts \"{class} $i [string is {class} -strict [format %c $i]]\"\n\
                 }}\n"
            ));
        }
    }

    assert_eq!(
        subject(&program),
        reference(&tclsh, &program),
        "a character class disagrees with tclsh"
    );
}

/// The classes whose answer is a *value* question rather than a character one,
/// and the strictness rule that decides an empty string.
#[test]
fn value_classes_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };

    const SUBJECTS: &[&str] = &[
        "", " ", "0", "1", "12", "  12  ", "0x10", "0b101", "0o17", "1_0", "007", "-5", "+5",
        "1.5", ".5", "1e10", "1.2e+3", "inf", "nan", "abc", "12x", "1.2.3", "true", "TRUE", "tru",
        "yes", "no", "on", "off", "maybe", "{a b}", "a {b", "a b c", "{}",
    ];
    const CLASSES: &[&str] = &[
        "boolean",
        "true",
        "false",
        "integer",
        "wideinteger",
        "entier",
        "double",
        "list",
        "dict",
        "ascii",
        "xdigit",
    ];

    let mut program = String::new();
    for class in CLASSES {
        for subject in SUBJECTS {
            let w = quoted(subject);
            program.push_str(&format!(
                "puts [list {class} {w} [string is {class} {w}] \
                 [string is {class} -strict {w}]]\n"
            ));
        }
    }

    assert_eq!(
        subject(&program),
        reference(&tclsh, &program),
        "a value class disagrees with tclsh"
    );
}

/// `-failindex` writes the index of the first character that failed — and only
/// when the answer is 0, leaving the variable alone otherwise.
///
/// The index is the length of the longest prefix that still belongs to the
/// class, which is one rule for every class: `"  12x"` is 4 as an integer
/// because `"  12"` still is one, and `"1.2e+"` is 3 as a double but 1 as an
/// integer.
#[test]
fn failindex_matches_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };

    const CASES: &[(&str, &str)] = &[
        ("alpha", "abc"),
        ("alpha", "abX9"),
        ("alpha", "9ab"),
        ("alpha", ""),
        ("alpha", "abé9"),
        ("alnum", "ab-cd"),
        ("lower", "abC"),
        ("upper", "ABc"),
        ("control", "5"),
        ("digit", "12a34"),
        ("integer", "12x4"),
        ("integer", "  12x"),
        ("integer", "0x1g"),
        ("integer", "  "),
        ("double", "1.2.3"),
        ("double", "1.2e+"),
        ("boolean", "maybe"),
        ("list", "a {b"),
        ("list", "{a} {b"),
        ("xdigit", "12g"),
    ];

    let mut program = String::new();
    for (class, text) in CASES {
        // The variable starts with a value, so a run that leaves it untouched
        // is visible as such rather than as an empty string.
        let w = quoted(text);
        program.push_str(&format!(
            "set v PRE\n\
             puts [list {class} {w} [string is {class} -failindex v {w}] $v]\n\
             set v PRE\n\
             puts [list {class} strict {w} \
             [string is {class} -strict -failindex v {w}] $v]\n"
        ));
    }

    assert_eq!(
        subject(&program),
        reference(&tclsh, &program),
        "-failindex disagrees with tclsh"
    );
}

/// `-stride` on both commands, including the two ways they disagree with each
/// other: the smallest stride each accepts, and the wording each refuses with.
#[test]
fn stride_matches_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };

    let program = concat!(
        "puts [lsort -stride 2 {c 3 a 1 b 2}]\n",
        "puts [lsort -stride 2 -decreasing {c 3 a 1 b 2}]\n",
        "puts [lsort -stride 3 {b 2 x a 1 y}]\n",
        "puts [lsort -stride 2 -integer {10 x 9 y 100 z}]\n",
        "puts [lsort -stride 2 -real {1.5 a 0.5 b}]\n",
        // `-indices` answers every index of each group, not the group's first.
        "puts [lsort -stride 2 -indices {c 3 a 1 b 2}]\n",
        // `-unique` keeps the *last* of a run of equal groups.
        "puts [lsort -stride 2 -unique {a 1 a 2 b 3}]\n",
        "puts [lsort -stride 2 -unique -indices {a 1 a 2 b 3}]\n",
        "puts [lsearch -stride 2 {a 1 b 2 c 3} b]\n",
        "puts [lsearch -stride 2 -inline {a 1 b 2} b]\n",
        "puts [lsearch -stride 2 -all {a 1 a 2} a]\n",
        "puts [lsearch -stride 2 -all -inline {a 1 a 2} a]\n",
        "puts [lsearch -stride 3 {a 1 x b 2 y} b]\n",
        "puts [lsearch -stride 2 -exact {a 1 b 2} b]\n",
        "puts [lsearch -stride 2 -not {a 1 b 2} a]\n",
        "puts [lsearch -stride 2 {a 1 b 2} zz]\n",
        // `lsearch` takes a stride of 1 and `lsort` does not, and they word the
        // refusal differently.
        "puts [catch {lsearch -stride 1 {a b} b} e]; puts <$e>\n",
        "puts [catch {lsearch -stride 0 {a b} b} e]; puts <$e>\n",
        "puts [catch {lsort -stride 1 {a b}} e]; puts <$e>\n",
        "puts [catch {lsort -stride 2 {a 1 b}} e]; puts <$e>\n",
        "puts [catch {lsearch -stride 2 {a 1 b} b} e]; puts <$e>\n",
    );

    assert_eq!(
        subject(program),
        reference(&tclsh, program),
        "-stride disagrees with tclsh"
    );
}

/// The refusal itself, and its boundary.
///
/// tclsh 9.0.4's tables are ahead of Unicode 16.0 — the version this build
/// carries and the one Python's `unicodedata` reports for the same data. 4804
/// code points differ, and rather than answer those from a table that does not
/// know them, `string is` refuses and names the character. Its neighbours on
/// either side are answered, which is what keeps the refusal a boundary rather
/// than a hole.
#[test]
fn a_code_point_beyond_our_tables_is_refused() {
    let refused = tclrs::eval("puts [string is alpha [format %c 0x20C1]]");
    let message = match refused {
        Err(e) => e.to_string(),
        Ok(out) => panic!("U+20C1 should be refused, got {out:?}"),
    };
    assert!(
        message.contains("U+20C1") && message.contains("Unicode 16.0"),
        "the refusal should name the character and the table: {message}"
    );

    // U+0295 is the one the two tables both know and disagree about: Unicode
    // 16.0 calls it a lowercase letter, tclsh answers 0 for `string is lower`.
    assert!(tclrs::eval("puts [string is lower [format %c 0x295]]").is_err());

    // Either side of it answers.
    for cp in ["0x20C0", "0x20C2", "0x294", "0x296"] {
        let program = format!("puts [string is alpha [format %c {cp}]]");
        assert!(
            tclrs::eval(&program).is_ok(),
            "{cp} is inside our tables and should be answered"
        );
    }
}
