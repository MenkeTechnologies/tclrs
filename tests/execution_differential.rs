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
    // Loop rotation: every loop is emitted entered-at-its-test and closed by a
    // conditional backward branch, so the test runs before the first iteration
    // and the next test sits below the body. What that moves is where `break`
    // and `continue` land and at what stack depth — these programs pin both.
    "for {set i 0} {$i < 3} {incr i} {puts $i}",
    "for {set i 0} {0} {incr i} {puts never}\nputs done",
    "for {set i 0} {$i < 9} {incr i} {if {$i == 4} {break}}\nputs $i",
    "for {set i 0} {$i < 5} {incr i} {if {$i == 2} {continue}; puts $i}",
    "set n 0\nfor {set i 0} {$i < 4} {incr i} {continue; incr n}\nputs \"$i $n\"",
    "puts [for {set i 0} {$i < 2} {incr i} {set x $i}]",
    "foreach x {a b c} {puts $x}",
    "foreach x {} {puts never}\nputs done",
    "foreach x {a b c d} {if {$x eq \"c\"} {break}; puts $x}",
    "foreach x {a b c d} {if {$x eq \"b\"} {continue}; puts $x}",
    "foreach {a b} {1 2 3} {puts \"$a|$b\"}",
    "foreach a {1 2} b {x y} {puts \"$a$b\"}",
    "puts [foreach x {a b} {set y $x}]",
    // Nested rotated loops: the inner loop's exit must not disturb the outer
    // loop's own test, and `break` must leave only the inner one.
    "for {set i 0} {$i < 3} {incr i} {for {set j 0} {$j < 3} {incr j} {if {$j == 1} {break}; puts \"$i$j\"}}",
    "set i 0\nwhile {$i < 3} {incr i; foreach x {a b} {if {$x eq \"b\"} {continue}; puts \"$i$x\"}}",
    "foreach x {1 2 3} {set j 0\nwhile {$j < $x} {incr j}\nputs \"$x:$j\"}",
    // A loop whose body leaves values on the stack per iteration: the exits
    // discard a statically known number of them.
    "set i 0\nwhile {$i < 4} {incr i; if {$i == 2} {continue}; if {$i == 3} {break}; puts $i}\nputs $i",
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
        // `proc`, `foreach` and `set a(1) x` all stood here until the phases
        // that built them landed. `uplevel` is a command this frontend has no
        // implementation of at all, so it stands in now.
        ("uplevel 1 {set x 1}", "invalid command name \"uplevel\""),
        (
            "array startsearch a",
            "array startsearch is not supported yet",
        ),
        // A procedure's frame slots are what `return` returns from, so the
        // command has no meaning outside one.
        (
            "return 1",
            "\"return\" outside of a procedure is not supported",
        ),
        (
            "puts [expr {sin(1)}]",
            "math function \"sin\" is not supported yet",
        ),
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

/// `i64::MIN % -1` and `i64::MIN / -1` are the two integer operations whose
/// hardware form traps. Tcl answers 0 and a bignum; this frontend must answer
/// 0 and report the overflow, and must not abort the process either way.
/// Found by the conformance run against the official suite.
#[test]
fn min_int_over_negative_one_does_not_trap() {
    let min = "set min [expr {-9223372036854775807 - 1}]\n";
    let outcome = tclrs::eval(&format!("{min}puts [expr {{$min % -1}}]")).expect("remainder");
    assert_eq!(outcome.output, "0\n", "tclsh prints 0 for this remainder");

    let err = tclrs::eval(&format!("{min}puts [expr {{$min / -1}}]"))
        .expect_err("the quotient does not fit in i64");
    assert!(err.contains("too large"), "got {err:?}");
}
