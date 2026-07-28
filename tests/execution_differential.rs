//! Differential execution: a script compiled and run on fusevm must print
//! exactly what tclsh prints for the same source.
//!
//! Expectations are never written by hand — each program is executed by both
//! implementations and the output compared byte for byte, so a misreading of
//! Tcl's arithmetic (floored integer division, numeric-preferring comparison,
//! double formatting) fails here rather than becoming a baked-in bug.

use std::path::PathBuf;
use std::process::Command;

const PROGRAMS: &[&str] = &[
    // Assignment and substitution.
    "set x 5\nputs $x",
    "set x 5\nset y $x\nputs \"$x$y\"",
    "set x 5\nputs [set x]",
    "set greeting hello\nputs \"$greeting, world\"",
    "puts [set a 3]",
    // Values that must stay strings even though they look numeric.
    "set x 05\nputs $x",
    "set x 1.10\nputs $x",
    "set x { spaced }\nputs \"<$x>\"",
    // Integer arithmetic, including Tcl's floored division and remainder.
    "puts [expr {1+2*3}]",
    "puts [expr {(1+2)*3}]",
    "puts [expr {-57 / 10}]",
    "puts [expr {-57 % 10}]",
    "puts [expr {57 / -10}]",
    "puts [expr {57 % -10}]",
    "puts [expr {7/2}]",
    "puts [expr {2**10}]",
    "puts [expr {2**3**2}]",
    // Doubles and their formatting.
    "puts [expr {3.0/2}]",
    "puts [expr {1.0/3}]",
    "puts [expr {1.0+1}]",
    "puts [expr {2.0*3}]",
    "puts [expr {0.1+0.2}]",
    "puts [expr {1e300*10}]",
    "puts [expr {1.0e-7/10}]",
    "puts [expr {2**0.5}]",
    // Comparison: numeric when both operands are numeric, string otherwise.
    "puts [expr {10 < 9}]",
    "puts [expr {\"10\" < \"9\"}]",
    "puts [expr {\"abc\" < \"abd\"}]",
    "puts [expr {1 == 1.0}]",
    "puts [expr {\"a\" eq \"a\"}]",
    "puts [expr {\"a\" ne \"b\"}]",
    "puts [expr {\"abc\" lt \"abd\"}]",
    "puts [expr {\"a\" eq \"a\" == 1}]",
    // Logical and bitwise operators.
    "puts [expr {1 && 0}]",
    "puts [expr {0 || 3}]",
    "puts [expr {!5}]",
    "puts [expr {~5}]",
    "puts [expr {-8 >> 1}]",
    "puts [expr {1 << 3}]",
    "puts [expr {6 & 3}]",
    "puts [expr {6 | 3}]",
    "puts [expr {6 ^ 3}]",
    "puts [expr {1 ? 2 : 3}]",
    "puts [expr {0 ? 2 : 3}]",
    // Operands drawn from variables and nested commands.
    "set a 4\nset b 6\nputs [expr {$a*$b}]",
    "set a 4\nputs [expr {[expr {$a+1}] * 2}]",
    "set s abc\nputs [expr {$s eq \"abc\"}]",
    "set x 10\nputs [expr {$x > 3 && $x < 20}]",
    // Radix prefixes.
    "puts [expr {0xff + 1}]",
    "puts [expr {0b1010}]",
    "puts [expr {0o17}]",
    // Control flow.
    "if {1} {puts yes}",
    "if {0} {puts yes} else {puts no}",
    "if {0} {puts a} elseif {1} {puts b} else {puts c}",
    "set x 3\nif {$x > 2} {puts big} else {puts small}",
    "puts [if {1} {expr 41+1}]",
    "set i 0\nwhile {$i < 3} {puts $i; incr i}",
    "set i 0\nwhile {$i < 5} {incr i; if {$i == 3} {break}}\nputs $i",
    "set i 0\nset n 0\nwhile {$i < 5} {incr i; if {$i == 3} {continue}; incr n}\nputs $n",
    "set i 10\nwhile {0} {puts never}\nputs $i",
    "set total 0\nset i 1\nwhile {$i <= 100} {set total [expr {$total + $i}]; incr i}\nputs $total",
    // incr and its return value.
    "set i 5\nputs [incr i]",
    "set i 5\nputs [incr i 3]",
    "set i 5\nincr i -2\nputs $i",
    // puts variants.
    "puts -nonewline a\nputs b",
    "puts {}",
    "puts \"\"",
    // Comments and separators do not disturb execution.
    "# leading comment\nputs a ;# trailing\nputs b",
    "puts a; puts b",
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

fn reference_output(tclsh: &PathBuf, program: &str) -> String {
    let path = std::env::temp_dir().join(format!("tclrs-exec-{}.tcl", std::process::id()));
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

#[test]
fn execution_matches_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };

    let mut failures = Vec::new();
    for program in PROGRAMS {
        let expected = reference_output(&tclsh, program);
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
        "{} of {} programs diverge:\n\n{}",
        failures.len(),
        PROGRAMS.len(),
        failures.join("\n\n")
    );
}

/// A script's value is the value of its last command, which is what command
/// substitution reads.
#[test]
fn script_value_is_the_last_command() {
    assert_eq!(tclrs::eval("set x 7").unwrap().result, "7");
    assert_eq!(tclrs::eval("set x 7\nexpr {$x*2}").unwrap().result, "14");
    assert_eq!(tclrs::eval("").unwrap().result, "");
    assert_eq!(tclrs::eval("while {0} {puts x}").unwrap().result, "");
}

/// Constructs that are not built yet must be rejected at compile time rather
/// than silently doing something else.
#[test]
fn unsupported_constructs_are_refused() {
    for (src, expected) in [
        // `proc` is built now; `foreach` still stands in for a command that
        // this frontend has no implementation of at all.
        (
            "foreach x {1 2} {puts $x}",
            "invalid command name \"foreach\"",
        ),
        ("set a(1) x", "array variables are not supported yet"),
        (
            "puts [expr {sin(1)}]",
            "math function \"sin\" is not supported yet",
        ),
        ("puts [expr {1 in {1 2}}]", "need list support"),
        ("break", "invoked \"break\" outside of a loop"),
    ] {
        let err = tclrs::eval(src).expect_err(&format!("{src:?} should fail"));
        assert!(
            err.contains(expected),
            "{src:?}: expected an error mentioning {expected:?}, got {err:?}"
        );
    }
}

/// Integer overflow has no bignum fallback yet, so it must fail loudly instead
/// of wrapping.
#[test]
fn integer_overflow_is_an_error_not_a_wrap() {
    let err = tclrs::eval("puts [expr {9223372036854775807 + 1}]").expect_err("should overflow");
    assert!(err.contains("too large"), "got {err:?}");
}
