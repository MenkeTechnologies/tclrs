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
//!
//! A few cases are narrower than an `i64` on purpose. Exact ordering is not a
//! bignum rule but an integer rule: `3**34` fits a machine integer and still
//! has no double, so the same rounding that makes a bignum compare wrong makes
//! it compare wrong. Those cases sit here rather than in a suite of their own
//! because they are testing the same `runtime::big_cmp` the wide ones are.

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
    // ── exact ordering below `i64`, where width is not what decides it ────
    // 3^34 fits a machine integer and still has no double: past 2^53 the
    // conversion lands on a neighbour, so `3**34` and `double(3**34)` are one
    // apart. `min`/`max` are the ordering these can reach today — the six
    // comparison operators lower to native fusevm ops, which answer an
    // integer-vs-double pair themselves and round doing it (BUGS.md).
    "set l [expr {3**34}]\nputs [expr {min($l, double($l))}]",
    "set l [expr {3**34}]\nputs [expr {max($l, double($l))}]",
    "set l [expr {3**34}]\nputs [expr {min(double($l), $l)}]",
    "set l [expr {3**34}]\nputs [expr {max(double($l), $l)}]",
    // 2^53 + 1, the first integer a double cannot hold, and a negative one:
    // the sign must not decide which way the exact ordering goes.
    "puts [expr {min(9007199254740993, 9007199254740992.0)}]",
    "puts [expr {max(9007199254740993, 9007199254740992.0)}]",
    "puts [expr {min(-16677181699666569, -16677181699666568.0)}]",
    "puts [expr {max(-16677181699666569, -16677181699666568.0)}]",
    // ── mixed with doubles, which makes the result a double ──────────────
    "puts [expr {99999999999999999999 + 0.5}]",
    "puts [expr {99999999999999999999 * 2.0}]",
    "puts [expr {99999999999999999999 / 2.0}]",
    // Arithmetic promotes where ordering does not, and the same pair shows
    // both rules: tclsh answers `0.0` here, not the exact `1` that ordering
    // the two operands apart implies. A fix to the ordering rule that leaked
    // into arithmetic would change these.
    "set l [expr {3**34}]\nputs [expr {$l - double($l)}]",
    "set l [expr {3**34}]\nputs [expr {$l + double($l)}]",
    "set l [expr {3**34}]\nputs [expr {$l * 1.0}]",
    "set l [expr {3**34}]\nputs [expr {$l / double($l)}]",
    "set l [expr {3**34}]\nputs [expr {$l ** 1.0}]",
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
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
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
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
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

/// `**` with a base of 0, 1 or -1 answers at ANY exponent, and only a base of
/// magnitude 2 or more can be refused for the exponent's width.
///
/// `expr {0 ** 4611686018427387903}` is 0 in tclsh and was `exponent too large`
/// here: both `**` arms measured the exponent before looking at the base, so
/// every exponent past `u32` was refused whatever it was raising. The three
/// bases that cannot overflow are exactly the three the negative-exponent arm
/// already answered directly, and `(-1) ** n` is the parity of `n` — read off
/// the low bit, since a bignum exponent has no `abs()` that fits.
///
/// Both widths of exponent are covered: one that fits an `i64` and one that does
/// not, since they reach different code (`Num::Int` against `big_arith`), and
/// the bignum path is reached with a SMALL base whenever the exponent is wide.
/// The refusals are pinned beside them so widening the rule cannot swallow them.
#[test]
fn a_base_that_cannot_overflow_answers_at_any_exponent() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };

    let programs: &[&str] = &[
        // i64 exponent, the three bases that always answer.
        "puts [expr {0 ** 4611686018427387903}]",
        "puts [expr {1 ** 9223372036854775807}]",
        "puts [expr {(-1) ** 4611686018427387903}]",
        "puts [expr {(-1) ** 4611686018427387902}]",
        // Bignum exponent, same three bases, both signs.
        "puts [expr {0 ** 99999999999999999999999}]",
        "puts [expr {1 ** 99999999999999999999999}]",
        "puts [expr {(-1) ** 99999999999999999999999}]",
        "puts [expr {(-1) ** 99999999999999999999998}]",
        "puts [expr {1 ** -99999999999999999999999}]",
        "puts [expr {(-1) ** -99999999999999999999999}]",
        "puts [expr {(-1) ** -99999999999999999999998}]",
        // A base of magnitude >= 2 truncates to zero at a negative exponent
        // rather than being refused, at either width.
        "puts [expr {2 ** -99999999999999999999999}]",
        "puts [expr {99999999999999999999999 ** -1}]",
        "puts [expr {(-99999999999999999999999) ** -1}]",
        // The zero exponent, where each base is 1.
        "puts [expr {0 ** 0}]",
        "puts [expr {1 ** 0}]",
        "puts [expr {(-1) ** 0}]",
        // Still refused, and still with tclsh's wording: only |base| >= 2 can
        // outgrow the exponent, and zero to a negative power has no value.
        "puts [expr {2 ** 4611686018427387903}]",
        "puts [expr {(-2) ** 4611686018427387903}]",
        "puts [expr {2 ** 99999999999999999999999}]",
        "puts [expr {0 ** -1}]",
        "puts [expr {0 ** -99999999999999999999999}]",
        // Ordinary exponentiation is untouched, on both sides of the promotion.
        "puts [expr {2 ** 10}]",
        "puts [expr {(-2) ** 3}]",
        "puts [expr {2 ** 63}]",
        "puts [expr {3 ** 40}]",
        "puts [expr {(-2) ** 101}]",
        "puts [expr {99999999999999999999999 ** 2}]",
    ];

    let mut failures = Vec::new();
    for program in programs {
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
        "{} of {} differ:\n\n{}",
        failures.len(),
        programs.len(),
        failures.join("\n\n")
    );
}
