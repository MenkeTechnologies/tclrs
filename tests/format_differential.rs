//! Differential execution for `format`'s numeric conversions.
//!
//! `tests/string_differential.rs` already compares `format` over the shapes a
//! script writes by hand. What is compared here is the part of it that only a
//! generated cross product reaches: the integer conversions at every size
//! modifier over values on both sides of the 32-, 64- and 128-bit boundaries,
//! and the floating-point ones at every flag and precision.
//!
//! Both were divergences before this file existed, and neither was reachable
//! from a hand-written case:
//!
//! * The integer conversions **truncate modulo the conversion's width** at the
//!   arbitrary precision Tcl 9's integers have, rather than saturating an
//!   `i64` first. `format %d 18446744073709551616` is `0` in tclsh and was
//!   `-1` here; `format %lx 18446744073709551615` is `ffffffffffffffff` and was
//!   `7fffffffffffffff`. The `ll` and `L` modifiers do not truncate at all —
//!   `format %lld 18446744073709551617` is that number back — and print a sign
//!   beside the magnitude, so `format %llx -1` is `-1` and not `ffff…`.
//! * `%llu` refuses only a *negative* value, and `%#llu` prints the `0d` prefix
//!   that `%#u` does not.
//! * An integer spelling handed to a floating-point conversion is converted
//!   with a correctly rounded parser: `format %f 9223372036854775807` is
//!   `9223372036854775808.000000`, and a digit-by-digit accumulation in `f64`
//!   printed `9223372036854777856.000000`.
//!
//! Each test builds one program covering its whole cross product and runs it
//! through tclsh once, so the coverage costs one process rather than thousands.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Values on both sides of every boundary an integer conversion can truncate
/// at, in the spellings `format` accepts.
const INTEGERS: &[&str] = &[
    "0",
    "1",
    "-1",
    "-2",
    "255",
    "-255",
    "32767",
    "32768",
    "-32768",
    "65535",
    "65536",
    "2147483647",
    "2147483648",
    "-2147483648",
    "-2147483649",
    "4294967295",
    "4294967296",
    "9223372036854775807",
    "9223372036854775808",
    "-9223372036854775808",
    "-9223372036854775809",
    "18446744073709551615",
    "18446744073709551616",
    "18446744073709551617",
    "12345678901234567890",
    "-12345678901234567890",
    "340282366920938463463374607431768211457",
    "0x10",
    "-0x10",
    "0b1010",
    "0o17",
    "1_000",
];

const DOUBLES: &[&str] = &[
    "0.0",
    "-0.0",
    "1.0",
    "-1.0",
    "0.5",
    "1.5",
    "2.5",
    "-2.5",
    "0.1",
    "1e-5",
    "1e-4",
    "1e20",
    "1e21",
    "1e300",
    "1e-300",
    "123456789.123456789",
    "3.141592653589793",
    "1e16",
    "1e17",
    "9007199254740993.0",
    "1.7976931348623157e308",
    "5e-324",
    "999999.5",
    // Integer spellings, which a floating-point conversion also takes — and
    // which are what the correctly rounded parser is needed for.
    "9223372036854775807",
    "18446744073709551616",
    "0xffffffffffffffff",
];

const FLAGS: &[&str] = &["", "-", "+", " ", "0", "#", "-#", "+0", "#0", "0#"];
const SIZES: &[&str] = &["", "h", "l", "ll", "L", "j", "q", "z", "t"];
const WIDTHS: &[&str] = &["", "1", "8", "12", ".0", ".5", "8.3"];

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

fn reference(tclsh: &PathBuf, program: &str) -> (String, Option<String>) {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "tclrs-format-{}-{}.tcl",
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

/// Compare one generated program, naming the first line that differs.
fn compare(tclsh: &PathBuf, program: &str, what: &str) {
    let (expected, error) = reference(tclsh, program);
    assert!(
        error.is_none(),
        "tclsh rejected the {what} program: {}",
        error.unwrap_or_default()
    );
    let got = tclrs::eval(program)
        .unwrap_or_else(|e| panic!("the {what} program runs: {e}"))
        .output;
    if got == expected {
        return;
    }
    let mismatch = expected
        .lines()
        .zip(got.lines())
        .filter(|(a, b)| a != b)
        .take(10)
        .map(|(a, b)| format!("  tclsh: {a}\n  tclrs: {b}"))
        .collect::<Vec<_>>()
        .join("\n");
    let count = expected
        .lines()
        .zip(got.lines())
        .filter(|(a, b)| a != b)
        .count();
    panic!("{count} {what} conversions diverge:\n{mismatch}");
}

/// A refusal is printed rather than raised, so that one program covers the
/// whole cross product including the specifiers tclsh rejects.
///
/// The cross products are walked by `foreach` in the generated script rather
/// than unrolled into one `puts` per case: a hundred thousand distinct string
/// literals overflows the chunk's constant pool, whose operand is a `u16`, and
/// the loop costs five constants whatever the product's size.
const CATCHER: &str = "proc f {spec value} {\n\
    if {[catch {format $spec $value} out]} { return ERR:$out }\n\
    return $out\n\
}\n";

/// One Tcl list literal holding `items`, each braced so that an empty element
/// and one that is a blank both survive.
fn tcl_list(items: &[&str]) -> String {
    let mut out = String::from("{");
    for item in items {
        out.push('{');
        out.push_str(item);
        out.push_str("} ");
    }
    out.push('}');
    out
}

/// The generated loop nest: every combination of the lists, printed as the
/// specifier, the value and what `format` made of them.
fn sweep(values: &[&str], flags: &[&str], widths: &[&str], sizes: &[&str], convs: &[&str]) -> String {
    format!(
        "{CATCHER}\
         foreach value {} {{\n\
         foreach flags {} {{\n\
         foreach width {} {{\n\
         foreach size {} {{\n\
         foreach conv {} {{\n\
         set spec \"%$flags$width$size$conv\"\n\
         puts \"$spec $value [f $spec $value]\"\n\
         }}}}}}}}}}\n",
        tcl_list(values),
        tcl_list(flags),
        tcl_list(widths),
        tcl_list(sizes),
        tcl_list(convs),
    )
}

#[test]
fn integer_conversions_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    let program = sweep(
        INTEGERS,
        FLAGS,
        WIDTHS,
        SIZES,
        &["d", "i", "u", "o", "x", "X", "b"],
    );
    compare(&tclsh, &program, "integer");
}

#[test]
fn floating_point_conversions_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    let program = sweep(
        DOUBLES,
        FLAGS,
        &["", "1", "10", "20", ".0", ".1", ".6", ".17", "15.7"],
        &[""],
        &["e", "E", "f", "g", "G"],
    );
    compare(&tclsh, &program, "floating-point");
}

/// `%c` over the code points a conversion can be handed, and `%s` with a
/// precision that cuts a multi-byte character.
///
/// The surrogate range is left out: a `String` in this frontend cannot hold a
/// lone surrogate, so `format %c 55296` is the replacement character here and
/// U+D800 in tclsh. That is the same representation gap `encoding` documents.
#[test]
fn character_conversions_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    let mut program = String::from(CATCHER);
    for value in [
        "0", "1", "65", "127", "128", "255", "256", "8364", "65533", "65536", "128169", "1114111",
        "1114112", "-1", "2147483648",
    ] {
        for width in ["", "3", "-3"] {
            program.push_str(&format!(
                "puts \"%{width}c {value} [string length [f {{%{width}c}} {{{value}}}]]\"\n"
            ));
        }
    }
    for text in ["abcdef", "h\u{e9}llo", "\u{65e5}\u{672c}\u{8a9e}", ""] {
        for width in ["", ".0", ".3", "8.3", "-8.3"] {
            program.push_str(&format!(
                "puts \"%{width}s [f {{%{width}s}} {{{text}}}]|\"\n"
            ));
        }
    }
    compare(&tclsh, &program, "character");
}
