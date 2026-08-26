//! Differential execution for the `binary` ensemble.
//!
//! The same contract every other `*_differential.rs` here follows: each program
//! is run by tclsh and by this frontend and the two outputs are compared byte
//! for byte, so no expectation below is written by hand. That matters more for
//! `binary` than for most commands, because almost nothing about it is a rule
//! a reading of the manual settles:
//!
//! * The `bad field specifier` diagnostic names the character the format
//!   pointer was on *before* the leading blanks were skipped, so `binary format
//!   {c 3} 1 2` reports a blank rather than the `3`.
//! * `binary format` runs a pass that resolves counts and locates arguments
//!   before it looks at a single value, so a missing argument is reported ahead
//!   of an unusable one.
//! * `x` writes null bytes rather than skipping over what is already there,
//!   which only shows once `X` or `@` has moved the cursor back.
//! * A field with no count takes its argument whole and a field with a count of
//!   one takes the argument's first element, which is the difference between
//!   `binary format c {2 5}` (a refusal) and `binary format c1 {2 5}`.
//!
//! Every one of those was found by running these programs against tclsh 9.0.4.
//!
//! Two areas are deliberately not compared here, both of them reference
//! behaviour this frontend does not reproduce; see BUGS.md.
//!
//! * A `binary format` field of `X0` followed by any further field **crashes**
//!   tclsh 9.0.4 with a segmentation fault, so there is no reference outcome to
//!   compare against.
//! * `binary decode uuencode` of a line that declares more bytes than its
//!   characters carry reads past the end of that line in tclsh, and what it
//!   answers is whatever was in memory.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Programs whose whole output must agree, byte for byte.
///
/// Each prints the result as hexadecimal rather than as bytes, so that a
/// difference is legible in a failure message and so the comparison does not
/// depend on how either interpreter encodes its output.
const PROGRAMS: &[&str] = &[
    // The helper every program below shares.
    "puts [hex [binary format a5A5 abc abc]]",
    "puts [hex [binary format a3a*a abc de f]]",
    "puts [hex [binary format A6A*A alpha bravo charlie]]",
    "puts [hex [binary format a0 abc]]",
    "puts [hex [binary format a* {}]]",
    "puts [hex [binary format a2 [binary format c 200]]]",
    // Bit and hex digit strings, in both directions within a byte.
    "puts [hex [binary format b5b* 11100 111000011010]]",
    "puts [hex [binary format B5B* 11100 111000011010]]",
    "puts [hex [binary format H3H*H2 ab DEF 987]]",
    "puts [hex [binary format h3h*h2 AB def 987]]",
    "puts [hex [binary format b0 111]]",
    "puts [hex [binary format h3 a]]",
    "puts [hex [binary format B3 1]]",
    // Integers: every width, both byte orders and the native one.
    "puts [hex [binary format c3cc* {3 -3 128 1} 260 {2 5}]]",
    "puts [hex [binary format s3 {3 -3 258 1}]]",
    "puts [hex [binary format S3 {3 -3 258 1}]]",
    "puts [hex [binary format t 258]]",
    "puts [hex [binary format i3 {3 -3 65536 1}]]",
    "puts [hex [binary format I3 {3 -3 65536 1}]]",
    "puts [hex [binary format n 65536]]",
    "puts [hex [binary format w 7810179016327718216]]",
    "puts [hex [binary format Wc 4785469626960341345 110]]",
    "puts [hex [binary format m 1]]",
    // The modular truncation, at the precision Tcl 9's integers have.
    "puts [hex [binary format c 99999999999999999999]]",
    "puts [hex [binary format w 99999999999999999999]]",
    "puts [hex [binary format s 65536]]",
    "puts [hex [binary format i 4294967296]]",
    "puts [hex [binary format c 0x100]]",
    "puts [hex [binary format c -1]]",
    "puts [hex [binary format c \" 3 \"]]",
    "puts [hex [binary format c 1_0]]",
    // Floats and doubles, including what overflow becomes.
    "puts [hex [binary format f2 {1.6 3.4}]]",
    "puts [hex [binary format rR 1.6 1.6]]",
    "puts [hex [binary format d1 {1.6}]]",
    "puts [hex [binary format qQ 1.6 1.6]]",
    "puts [hex [binary format f 1e40]]",
    "puts [hex [binary format f -1e40]]",
    "puts [hex [binary format f 1e-60]]",
    "puts [hex [binary format d Inf]]",
    // The cursor moves, and the high-water mark that is the result's length.
    "puts [hex [binary format a3xa3x2a3 abc def ghi]]",
    "puts [hex [binary format a3X*a3X2a3 abc def ghi]]",
    "puts [hex [binary format a5@2a1@*a3@10a1 abcde f ghi j]]",
    "puts [hex [binary format @5]]",
    "puts [hex [binary format a2@0a1 ab z]]",
    "puts [hex [binary format {@3X*a1} z]]",
    "puts [hex [binary format X2a1 z]]",
    "puts [hex [binary format {su1X8s0x3} 1 255]]",
    "puts [hex [binary format {Wu*x0@3 x3} 2147483647]]",
    "puts [hex [binary format {a Xu5xu2} \"hello world\"]]",
    // A count of one reads the list; no count at all reads the argument whole.
    "puts [hex [binary format c1 {2 5}]]",
    "puts [hex [binary format c {2}]]",
    "puts [hex [binary format c2 {1 2 3}]]",
    "puts [hex [binary format c* {}]]",
    "puts [hex [binary format i* {1 2}]]",
    "puts [hex [binary format {  c  c  } 1 2]]",
    // binary scan, including what stops it.
    "binary scan \\x07\\x86\\x05 c2c* a b\nputs \"$a|$b\"",
    "binary scan \\x07\\x86\\x05 cu2cu* a b\nputs \"$a|$b\"",
    "puts [binary scan abcdefg s3s a b]",
    "binary scan \\x07\\xC6\\x05\\x1F\\x34 H3H* a b\nputs \"$a|$b\"",
    "binary scan \\x07\\x86\\x05\\x12\\x34 h3h* a b\nputs \"$a|$b\"",
    "binary scan \\x07\\x87\\x05 b5b* a b\nputs \"$a|$b\"",
    "binary scan \\x70\\x87\\x05 B5B* a b\nputs \"$a|$b\"",
    "binary scan \"abc\\x00efghi\" C* a\nputs <$a>",
    "binary scan \"abc efghi  \\x00\" A* a\nputs <$a>",
    "binary scan \\x05\\x00\\x00\\x00\\x07\\x00\\x00\\x00\\xF0\\xFF\\xFF\\xFF wi* a b\nputs \"$a|$b\"",
    "binary scan \\xCD\\xCC\\xCC\\x3F\\xCD\\xCC\\xCC\\x3F rf a b\nputs \"$a|$b\"",
    "binary scan \\x9A\\x99\\x99\\x99\\x99\\x99\\xF9\\x3F\\x9A\\x99\\x99\\x99\\x99\\x99\\xF9\\x3F qd a b\nputs \"$a|$b\"",
    "binary scan \\x00\\x00\\xc0\\x7f f a\nputs $a",
    "binary scan \\x00\\x00\\x80\\x7f f a\nputs $a",
    "binary scan \\x01\\x02\\x03\\x04 x2H* a\nputs $a",
    "binary scan abcd X2H* a\nputs $a",
    "binary scan abcd @2H* a\nputs $a",
    "puts [binary scan abc a5 a]",
    "puts [binary scan abc b100 a]",
    "binary scan abc c0 a\nputs <$a>",
    "binary scan \\xff\\xff\\xff\\xff\\xff\\xff\\xff\\xff\\xff\\xff\\xff\\xff\\xff\\xff\\xff\\xff wuw a b\nputs \"$a|$b\"",
    "binary scan \\xff\\xff\\xff\\xff\\xff\\xff\\xff\\xff\\xff\\xff\\xff\\xff\\xff\\xff\\xff\\xff iuiSusut a b c d e\nputs \"$a|$b|$c|$d|$e\"",
    "puts [binary scan abc a* a b]",
    "binary scan abcdef {a2 a2} a b\nputs \"$a|$b\"",
    // encode and decode.
    "puts [binary encode hex abc]",
    "puts [binary encode base64 abcdefgh]",
    "puts [binary encode base64 -maxlen 4 abcdefgh]",
    "puts [binary encode base64 -maxlen 3 -wrapchar | abcdefgh]",
    "puts [binary encode uuencode abcdefgh]",
    "puts [binary encode uuencode -maxlen 5 abcdefgh]",
    "puts [hex [binary decode hex 616263]]",
    "puts [hex [binary decode hex \"61 62 63\"]]",
    "puts [hex [binary decode base64 YWJjZA==]]",
    "puts [hex [binary decode base64 -strict YWJjZA==]]",
    "puts [hex [binary decode base64 \"  YWJj\"]]",
    "puts [hex [binary decode uuencode [binary encode uuencode {The quick brown fox}]]]",
    "puts [hex [binary decode base64 [binary encode base64 [binary format c* {0 1 2 254 255}]]]]",
];

/// Programs tclsh refuses. Only the first line of its message is compared, as
/// every other differential test here does: the stack trace below it is the
/// interpreter's own and not the diagnostic.
const REFUSALS: &[&str] = &[
    "binary",
    "binary zzz",
    "binary format",
    "binary format q",
    "binary format c",
    "binary format a",
    "binary format b3c x",
    "binary format a*c abc",
    "binary format c {2 5}",
    "binary format c {a b}",
    "binary format c \"a b\"",
    "binary format c {\"a b\"}",
    "binary format s {1 2}",
    "binary format f {1 2}",
    "binary format d {1.5 2}",
    "binary format c2 {1}",
    "binary format c2 x",
    "binary format c \"\"",
    "binary format c x",
    "binary format c true",
    "binary format c 1e10",
    "binary format d x",
    "binary format b x",
    "binary format H z",
    "binary format x*",
    "binary format @",
    "binary format @x",
    "binary format Z 1",
    "binary format {c 3} 1 2",
    "binary format {c z} 1 2",
    "binary format { 3} 1",
    "binary format {3} 1",
    "binary format c-1 1",
    "binary format cZ 1",
    "binary scan",
    "binary scan abc",
    "binary scan abc c",
    "binary scan abc a",
    "binary scan abc {a a} v",
    "binary scan abc zz v",
    "binary scan abc cu2u v",
    "binary scan abc @",
    "binary scan \"\\u0100\" c v",
    "binary scan \"aa\\u00e9\\u0100\" c v",
    "binary encode",
    "binary encode zz x",
    "binary encode hex",
    "binary encode hex a b",
    "binary encode hex -strict abc",
    "binary encode base64",
    "binary encode base64 a b",
    "binary encode base64 -zz 1 abc",
    "binary encode base64 -maxlen abc",
    "binary encode base64 -maxlen x abc",
    "binary encode base64 -maxlen -1 abc",
    "binary encode uuencode -maxlen 4 abc",
    "binary encode uuencode -maxlen 86 abc",
    "binary encode uuencode -wrapchar | abc",
    "binary decode",
    "binary decode hex",
    "binary decode hex zz",
    "binary decode hex a b",
    "binary decode hex -maxlen 4 YWJj",
    "binary decode hex -zz YWJj",
    "binary decode hex -strict \"61 62\"",
    "binary decode base64 -strict \"====\"",
    "binary decode base64 -strict \"YQ===\"",
    "binary decode base64 -strict \"YWJjZ\"",
    "binary decode base64 -strict \"Y Q==\"",
    "binary decode uuencode -strict YWJj",
    "binary decode uuencode -strict [binary encode uuencode a]",
];

/// Prepended to every program: the hexadecimal rendering the comparisons use,
/// written without `binary encode hex` so that a bug in one command cannot hide
/// a bug in another.
const HELPER: &str = "proc hex {s} {\n\
    set out {}\n\
    foreach c [split $s {}] { append out [format %02x [scan $c %c]] }\n\
    return $out\n\
}\n";

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

/// Run a program through tclsh, returning its stdout and the first line of any
/// error it reported.
fn reference(tclsh: &PathBuf, program: &str) -> (String, Option<String>) {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "tclrs-binary-{}-{}.tcl",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path, program).expect("write program");
    let out = Command::new(tclsh).arg(&path).output().expect("run tclsh");
    let _ = std::fs::remove_file(&path);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let error = (!out.status.success()).then(|| {
        String::from_utf8_lossy(&out.stderr)
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_string()
    });
    (stdout, error)
}

#[test]
fn binary_execution_matches_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };

    let mut failures = Vec::new();
    for program in PROGRAMS {
        let whole = format!("{HELPER}{program}");
        let (expected, error) = reference(&tclsh, &whole);
        assert!(
            error.is_none(),
            "tclsh rejected program:\n{program}\n{}",
            error.unwrap_or_default()
        );
        match tclrs::eval(&whole) {
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
        "{} of {} programs diverge:\n\n{}",
        failures.len(),
        PROGRAMS.len(),
        failures.join("\n\n")
    );
}

#[test]
fn binary_errors_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };

    let mut failures = Vec::new();
    for program in REFUSALS {
        let (_, error) = reference(&tclsh, program);
        let Some(expected) = error else {
            failures.push(format!("program:\n{program}\n  tclsh accepted it"));
            continue;
        };
        match tclrs::eval(program) {
            Ok(outcome) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs produced {:?}",
                outcome.output
            )),
            Err(e) if !e.starts_with(&expected) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs: {e:?}"
            )),
            Err(_) => {}
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} programs diverge:\n\n{}",
        failures.len(),
        REFUSALS.len(),
        failures.join("\n\n")
    );
}

/// Every field type, every count form and every flag, driven through `format`
/// and then back through `scan`, as one generated program rather than one per
/// case — so a divergence anywhere in the cross product is caught without
/// paying for a tclsh process per case.
#[test]
fn every_field_type_round_trips_through_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };

    let mut program = String::from(HELPER);
    // One argument per type, chosen so that every field has something legal to
    // format: text for the string types, digits for the bit and hex ones,
    // numbers for the rest.
    // A field with no count takes its argument whole, so the numeric types need
    // a scalar there and a list everywhere else; the string types take the same
    // word either way.
    for (kind, scalar, list) in [
        ("a", "{ab cd}", "{ab cd}"),
        ("A", "{ab cd}", "{ab cd}"),
        ("b", "1011000111", "1011000111"),
        ("B", "1011000111", "1011000111"),
        ("h", "0123456789abcdef", "0123456789abcdef"),
        ("H", "0123456789abcdef", "0123456789abcdef"),
        ("c", "200", "{0 1 127 128 255 -1 -128}"),
        ("s", "32768", "{0 1 32767 32768 -1}"),
        ("S", "32768", "{0 1 32767 32768 -1}"),
        ("t", "32768", "{0 1 32767 32768 -1}"),
        ("i", "2147483648", "{0 1 2147483647 -1}"),
        ("I", "2147483648", "{0 1 2147483647 -1}"),
        ("n", "2147483648", "{0 1 2147483647 -1}"),
        ("w", "9223372036854775808", "{0 1 9223372036854775807 -1}"),
        ("W", "9223372036854775808", "{0 1 9223372036854775807 -1}"),
        ("m", "9223372036854775808", "{0 1 9223372036854775807 -1}"),
        ("f", "1.5", "{1.5 -0.5 0.0}"),
        ("r", "1.5", "{1.5 -0.5 0.0}"),
        ("R", "1.5", "{1.5 -0.5 0.0}"),
        ("d", "1.5", "{1.5 -0.5 0.0}"),
        ("q", "1.5", "{1.5 -0.5 0.0}"),
        ("Q", "1.5", "{1.5 -0.5 0.0}"),
    ] {
        for count in ["", "0", "1", "2", "3", "*"] {
            let arg = if count.is_empty() { scalar } else { list };
            program.push_str(&format!(
                "puts \"{kind}{count} [hex [binary format {kind}{count} {arg}]]\"\n"
            ));
            for flag in ["", "u"] {
                program.push_str(&format!(
                    "unset -nocomplain v\n\
                     puts \"{kind}{flag}{count} [binary scan [binary format {kind}{count} {arg}] \
                     {kind}{flag}{count} v] [expr {{[info exists v] ? $v : {{-}}}}]\"\n"
                ));
            }
        }
    }

    let (expected, error) = reference(&tclsh, &program);
    assert!(
        error.is_none(),
        "tclsh rejected the generated program: {}",
        error.unwrap_or_default()
    );
    let got = tclrs::eval(&program)
        .expect("the generated program runs")
        .output;
    if got != expected {
        let mismatch = expected
            .lines()
            .zip(got.lines())
            .find(|(a, b)| a != b)
            .map(|(a, b)| format!("tclsh: {a}\ntclrs: {b}"))
            .unwrap_or_else(|| "the outputs differ in length".to_string());
        panic!("the round trip diverges:\n{mismatch}");
    }
}
