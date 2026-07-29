//! Differential execution for integers wider than an `i64`.
//!
//! Tcl 9's integers are arbitrary precision, so every one of these programs has
//! a right answer that only the reference interpreter can give. Same contract as
//! the other differential suites: each program is run by both engines and the
//! outputs compared byte for byte, so nothing here is an expectation written by
//! hand.
//!
//! The cases are chosen around the places a bignum implementation goes wrong
//! rather than around addition: the sign rules of floored division, the point
//! where a value comes back down into an `i64`, ordering against a double that
//! rounds to the same bits, and the operators whose meaning is two's complement
//! over an infinite word.

use std::path::PathBuf;
use std::process::Command;

const PROGRAMS: &[&str] = &[
    // ── promotion at each boundary ───────────────────────────────────────
    "puts [expr {9223372036854775807 + 1}]",
    "puts [expr {-9223372036854775807 - 2}]",
    "puts [expr {9223372036854775807 * 2}]",
    "puts [expr {9223372036854775807 * 9223372036854775807}]",
    "puts [expr {-9223372036854775807 - 1}]",
    // The one division whose quotient does not fit, and the remainder beside it.
    "set m [expr {-9223372036854775807 - 1}]\nputs [expr {$m / -1}]",
    "set m [expr {-9223372036854775807 - 1}]\nputs [expr {$m % -1}]",
    // ── coming back down ─────────────────────────────────────────────────
    // A result that fits an `i64` again must be an ordinary integer, not a
    // value that merely prints like one.
    "puts [expr {(9223372036854775807 + 1) - 1}]",
    "puts [expr {(2 ** 100) / (2 ** 100)}]",
    "puts [expr {(2 ** 100) - (2 ** 100)}]",
    "puts [expr {(1 << 200) >> 200}]",
    "puts [expr {((2 ** 100) / (2 ** 99)) + 1}]",
    // ── floored division and remainder, all four sign pairs ──────────────
    "puts [expr {99999999999999999999 / 7}]",
    "puts [expr {99999999999999999999 % 7}]",
    "puts [expr {-99999999999999999999 / 7}]",
    "puts [expr {-99999999999999999999 % 7}]",
    "puts [expr {99999999999999999999 / -7}]",
    "puts [expr {99999999999999999999 % -7}]",
    "puts [expr {-99999999999999999999 / -7}]",
    "puts [expr {-99999999999999999999 % -7}]",
    // Divisor wider than the dividend, and an exact division.
    "puts [expr {7 / 99999999999999999999}]",
    "puts [expr {7 % 99999999999999999999}]",
    "puts [expr {-7 / 99999999999999999999}]",
    "puts [expr {100000000000000000000 / 100000000000000000000}]",
    // ── powers ───────────────────────────────────────────────────────────
    "puts [expr {2 ** 64}]",
    "puts [expr {2 ** 100}]",
    "puts [expr {(-2) ** 65}]",
    "puts [expr {(-2) ** 64}]",
    "puts [expr {10 ** 30}]",
    "puts [expr {99999999999999999999 ** 2}]",
    "puts [expr {99999999999999999999 ** 0}]",
    "puts [expr {99999999999999999999 ** 1}]",
    // A negative exponent truncates toward zero even here.
    "puts [expr {99999999999999999999 ** -1}]",
    // ── shifts ───────────────────────────────────────────────────────────
    "puts [expr {1 << 62}]",
    "puts [expr {1 << 63}]",
    "puts [expr {1 << 64}]",
    "puts [expr {1 << 200}]",
    "puts [expr {-1 << 64}]",
    "puts [expr {3 << 100}]",
    "puts [expr {1 >> 200}]",
    "puts [expr {-1 >> 200}]",
    "puts [expr {99999999999999999999 >> 10}]",
    "puts [expr {99999999999999999999 << 10}]",
    "puts [expr {(1 << 100) >> 99}]",
    // ── bitwise, which is two's complement over an infinite word ─────────
    "puts [expr {99999999999999999999 & 255}]",
    "puts [expr {99999999999999999999 | 1}]",
    "puts [expr {99999999999999999999 ^ 3}]",
    "puts [expr {~99999999999999999999}]",
    "puts [expr {~(-99999999999999999999)}]",
    "puts [expr {(1 << 100) & (1 << 100)}]",
    "puts [expr {(1 << 100) | 1}]",
    "puts [expr {-99999999999999999999 & 255}]",
    // ── ordering, which must be exact and not through a double ───────────
    // 1e20 is exactly 100000000000000000000, so these three answers only come
    // out right if neither side is converted to the other's type.
    "puts [expr {99999999999999999999 < 1e20}]",
    "puts [expr {99999999999999999999 == 1e20}]",
    "puts [expr {99999999999999999999 > 1e20}]",
    "puts [expr {1e20 == 100000000000000000000}]",
    "puts [expr {100000000000000000001 > 100000000000000000000}]",
    "puts [expr {100000000000000000001 == 100000000000000000000}]",
    "puts [expr {-100000000000000000001 < -100000000000000000000}]",
    // Against a double with a fraction, where the tie is broken by the fraction.
    "puts [expr {100000000000000000000 > 99999999999999999999.5}]",
    "puts [expr {99999999999999999999 < 99999999999999999999.5}]",
    // Against the infinities, which no integer reaches.
    "puts [expr {99999999999999999999 < 1e400}]",
    "puts [expr {99999999999999999999 > -1e400}]",
    // ── mixed with doubles, which makes the result a double ──────────────
    "puts [expr {99999999999999999999 + 0.5}]",
    "puts [expr {99999999999999999999 * 2.0}]",
    "puts [expr {99999999999999999999 / 2.0}]",
    // ── the value in the rest of the language ────────────────────────────
    "puts 99999999999999999999",
    "puts [expr {99999999999999999999}]",
    "set x 99999999999999999999\nputs $x",
    "set x 9223372036854775807\nincr x\nputs $x",
    "set x 9223372036854775807\nincr x 5\nputs $x",
    "set y 99999999999999999999\nputs [incr y -1]",
    "set y 99999999999999999999\nputs [incr y]",
    "puts [string length 99999999999999999999]",
    "puts [string is integer 99999999999999999999]",
    "puts [string is entier 99999999999999999999]",
    "puts [string is double 99999999999999999999]",
    "puts [list [expr {2 ** 100}]]",
    "puts [llength [list [expr {2 ** 100}] a]]",
    "puts <[lindex {a b c} 99999999999999999999]>",
    "puts [lsort {99999999999999999999 5}]",
    // Truthiness: a bignum is nonzero by construction, and `!` of one is 0.
    "if {99999999999999999999} {puts T} else {puts F}",
    "if {-99999999999999999999} {puts T} else {puts F}",
    "puts [expr {!99999999999999999999}]",
    "puts [expr {99999999999999999999 && 1}]",
    // ── the spelling a literal keeps ─────────────────────────────────────
    // `eq` compares what the script wrote, so a radix spelling is not its
    // decimal value even though arithmetic on it is.
    "puts [expr {0x10000000000000000}]",
    "puts [expr {0x10000000000000000 eq \"0x10000000000000000\"}]",
    "puts [expr {0x10000000000000000 eq \"18446744073709551616\"}]",
    "puts [expr {0x10000000000000000 == 18446744073709551616}]",
    "puts [expr {0b1111111111111111111111111111111111111111111111111111111111111111111}]",
    "puts [expr {0o777777777777777777777777}]",
    "puts [expr {0xffffffffffffffff + 0}]",
    "puts [expr {99999999999999999999 eq \"99999999999999999999\"}]",
    // A separator inside a wide literal is still numeric whitespace.
    "puts [expr {99_999_999_999_999_999_999 + 1}]",
];

/// Programs both engines must refuse, and with the same message.
const REFUSED: &[&str] = &[
    // tclsh wants a machine integer for `-integer`, and says so.
    "puts [lsort -integer {99999999999999999999 5}]",
    "puts [lsearch -integer {99999999999999999999 5} 5]",
    // An exponent past what any memory holds is its own diagnostic, not the
    // overflow the product would report.
    "puts [expr {2 ** 9999999999}]",
    // Division by zero is still division by zero at any width.
    "puts [expr {99999999999999999999 / 0}]",
    "puts [expr {99999999999999999999 % 0}]",
    // A negative shift distance is illegal whatever the value's width.
    "puts [expr {99999999999999999999 << -1}]",
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

/// What tclsh prints for a program, stdout and stderr both — a refusal is as
/// much of an answer as a value.
fn reference(tclsh: &PathBuf, program: &str) -> String {
    // Named by the program's own hash as well as the pid: the two tests in this
    // file run concurrently, and a path shared between them has each reading the
    // other's script.
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(program, &mut hash);
    let path = std::env::temp_dir().join(format!(
        "tclrs-bignum-{}-{:x}.tcl",
        std::process::id(),
        std::hash::Hasher::finish(&hash)
    ));
    std::fs::write(&path, program).expect("write program");
    let out = Command::new(tclsh).arg(&path).output().expect("run tclsh");
    let _ = std::fs::remove_file(&path);
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        first_line(&String::from_utf8_lossy(&out.stderr))
    )
}

fn subject(program: &str) -> String {
    match tclrs::eval(program) {
        Ok(outcome) => outcome.output,
        Err(e) => first_line(&e.to_string()),
    }
}

/// The first line only: tclsh follows an error with a `while executing` trace
/// and tclrs with `(file "…" line N)`, and neither is the message.
fn first_line(text: &str) -> String {
    match text.trim_end().lines().next() {
        Some(line) => format!("{line}\n"),
        None => String::new(),
    }
}

#[test]
fn wide_integer_arithmetic_matches_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };

    let mut failures = Vec::new();
    for program in PROGRAMS {
        let expected = reference(&tclsh, program);
        let actual = subject(program);
        if expected != actual {
            failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs: {actual:?}"
            ));
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

/// A width neither engine will take must be refused by both, in the same words.
#[test]
fn what_tclsh_refuses_at_width_is_refused_here() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };

    let mut failures = Vec::new();
    for program in REFUSED {
        let expected = reference(&tclsh, program);
        let actual = subject(program);
        if expected != actual {
            failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs: {actual:?}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} refusals differ:\n\n{}",
        failures.len(),
        REFUSED.len(),
        failures.join("\n\n")
    );
}
