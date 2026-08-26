//! Differential execution of `expr`'s math functions: every program here is
//! run by tclsh and by tclrs and the two outputs are compared byte for byte.
//!
//! Nothing is written by hand. That matters most for the functions whose
//! result *type* is not obvious — `int()` does not truncate to the machine
//! word in 9.0.4, `floor()` of an integer rounds in a direction rather than to
//! nearest, `round(1e300)` is a 301-digit integer — and for the wording of
//! every refusal, which differs between `expected number`, `expected
//! floating-point number` and the empty message `srand` leaves behind.
//!
//! One class of program is deliberately absent: `sin`, `cos` and `tan` of a
//! large or awkward argument. Those disagree in the last unit in the last
//! place between the x86-64 tclsh this is measured against and the aarch64
//! libm the Rust build links, which is a difference between two C libraries
//! rather than between two implementations of Tcl. The arguments used below
//! are ones where the two agree, and the semantics the module actually owns —
//! the domain errors, the result types, the argument checking — are covered by
//! the rest of the table.

use std::path::PathBuf;
use std::process::Command;

const PROGRAMS: &[&str] = &[
    // The integer-preserving functions, including the widths where an `i64`
    // stops being enough.
    "puts [expr {abs(-1)}]",
    "puts [expr {abs(-2.5)}]",
    "puts [expr {abs(-0.0)}]",
    "puts [expr {abs(-9223372036854775808)}]",
    "puts [expr {abs(-12345678901234567890123)}]",
    "puts [expr {int(2.99)}]",
    "puts [expr {int(-2.99)}]",
    "puts [expr {int(1e19)}]",
    "puts [expr {int(1e300)}]",
    "puts [expr {entier(1e300)}]",
    "puts [expr {wide(1e19)}]",
    "puts [expr {wide(12345678901234567890123)}]",
    "puts [expr {round(2.5)}]",
    "puts [expr {round(-2.5)}]",
    "puts [expr {round(-0.5)}]",
    "puts [expr {round(1e19)}]",
    "puts [expr {round(1e300)}]",
    "puts [expr {isqrt(2)}]",
    "puts [expr {isqrt(0)}]",
    "puts [expr {isqrt(9007199254740992)}]",
    "puts [expr {isqrt(9007199254740993)}]",
    "puts [expr {isqrt(1e17)}]",
    "puts [expr {isqrt(1e30)}]",
    "puts [expr {isqrt(12345678901234567890123)}]",
    // `floor` and `ceil` of an *integer* are directed roundings, and of a
    // double are the C library's.
    "puts [expr {floor(9223372036854775807)}]",
    "puts [expr {ceil(9223372036854775807)}]",
    "puts [expr {floor(-9223372036854775808)}]",
    "puts [expr {floor(1000000000000000000000000)}]",
    "puts [expr {ceil(1000000000000000000000000)}]",
    "puts [expr {ceil(-0.5)}]",
    "puts [expr {floor(-0.5)}]",
    "puts [expr {ceil(2.5)}]",
    "puts [expr {floor(2.5)}]",
    "puts [expr {double(1)}]",
    "puts [expr {double(9223372036854775807)}]",
    "puts [expr {double(12345678901234567890123)}]",
    // The transcendental functions at arguments where the reference build's
    // libm and the host's agree; see the note at the top of this file.
    "puts [expr {sqrt(2)}]",
    "puts [expr {sqrt(1e300)}]",
    "puts [expr {sqrt(12345678901234567890123)}]",
    "puts [expr {exp(1)}]",
    "puts [expr {exp(1e300)}]",
    "puts [expr {log(1)}]",
    "puts [expr {log(0)}]",
    "puts [expr {log10(100)}]",
    "puts [expr {sin(1)}]",
    "puts [expr {cos(1)}]",
    "puts [expr {tan(1)}]",
    "puts [expr {sin(0)}]",
    "puts [expr {asin(0.5)}]",
    "puts [expr {acos(-1)}]",
    "puts [expr {atan(1)}]",
    "puts [expr {sinh(1)}]",
    "puts [expr {cosh(1)}]",
    "puts [expr {tanh(1)}]",
    "puts [expr {atan2(1,1)}]",
    "puts [expr {atan2(-1,-1)}]",
    "puts [expr {hypot(3,4)}]",
    "puts [expr {fmod(5,3)}]",
    "puts [expr {fmod(-5,3)}]",
    "puts [expr {pow(2,10)}]",
    "puts [expr {pow(2,-1)}]",
    "puts [expr {pow(0,-1)}]",
    "puts [expr {pow(-0.0,-1)}]",
    "puts [expr {pow(10,400)}]",
    // `max` and `min` keep the earlier argument on a tie, and order a bignum
    // exactly rather than through a double.
    "puts [expr {max(1,1.0)}]",
    "puts [expr {min(1,1.0)}]",
    "puts [expr {max(1,2.5)}]",
    "puts [expr {min(-1,-2.0)}]",
    "puts [expr {max(2,12345678901234567890123)}]",
    "puts [expr {min(-12345678901234567890123,2)}]",
    "puts [expr {max(1,2,3,4,5)}]",
    // The classifications, which accept a NaN where everything else refuses
    // one.
    "puts [expr {isnan(nan)}]",
    "puts [expr {isnan(1)}]",
    "puts [expr {isinf(inf)}]",
    "puts [expr {isfinite(1e300)}]",
    "puts [expr {isnormal(1e-320)}]",
    "puts [expr {issubnormal(1e-320)}]",
    "puts [expr {isunordered(1,nan)}]",
    "puts [expr {isunordered(1,2)}]",
    "puts [expr {bool(\"yes\")}]",
    "puts [expr {bool(0)}]",
    // The generator is Park & Miller's, so a seeded sequence is reproducible
    // and can be compared exactly.
    "expr srand(1)\nputs [list [expr rand()] [expr rand()] [expr rand()]]",
    "expr srand(0)\nputs [expr rand()]",
    "expr srand(-5)\nputs [expr rand()]",
    "expr srand(2147483647)\nputs [expr rand()]",
    "expr srand(3000000000)\nputs [expr rand()]",
    // Every refusal, caught so the program's own exit status stays 0 and the
    // message itself is what is compared.
    "puts [catch {expr {abs(\"abc\")}} m]\nputs $m",
    "puts [catch {expr {sin(\"abc\")}} m]\nputs $m",
    "puts [catch {expr {ceil(\"abc\")}} m]\nputs $m",
    "puts [catch {expr {abs(\"1 2\")}} m]\nputs $m",
    "puts [catch {expr {sin(\"1 2\")}} m]\nputs $m",
    "puts [catch {expr {sqrt(-1)}} m]\nputs $m",
    "puts [catch {expr {log(-1)}} m]\nputs $m",
    "puts [catch {expr {asin(2)}} m]\nputs $m",
    "puts [catch {expr {fmod(5,0)}} m]\nputs $m",
    "puts [catch {expr {pow(-2,0.5)}} m]\nputs $m",
    "puts [catch {expr {isqrt(-1)}} m]\nputs $m",
    "puts [catch {expr {int(inf)}} m]\nputs $m",
    "puts [catch {expr {round(inf)}} m]\nputs $m",
    "puts [catch {expr {double(nan)}} m]\nputs $m",
    "puts [catch {expr {abs(nan)}} m]\nputs $m",
    "puts [catch {expr {bool(\"\")}} m]\nputs $m",
    "puts [catch {expr {abs()}} m]\nputs $m",
    "puts [catch {expr {abs(1,2)}} m]\nputs $m",
    "puts [catch {expr {max()}} m]\nputs $m",
    "puts [catch {expr {atan2(1)}} m]\nputs $m",
    "puts [catch {expr {atan2(1,2,3)}} m]\nputs $m",
    "puts [catch {expr {rand(1)}} m]\nputs $m",
    "puts [catch {expr {nosuchfunction(1)}} m]\nputs $m",
    // `srand` of a non-integer reports through a null interpreter, so the
    // message it leaves is the empty string.
    "puts [catch {expr {srand(1.5)}} m]\nputs \"<$m>\"",
    "puts [catch {expr {srand(\"a\")}} m]\nputs \"<$m>\"",
    // An infinity a function produced can still reach an operator that makes a
    // NaN of it, and tclsh reports that rather than answering.
    "puts [catch {expr {pow(10,400)-pow(10,400)}} m]\nputs $m",
    "puts [catch {expr {pow(10,400)*0}} m]\nputs $m",
    // A call in a branch that is never taken costs nothing, because the name
    // and the argument count are both resolved when the call runs.
    "if {0} {expr {nosuchfunction(1)}}\nputs ok",
    "if {0} {expr {abs(1,2)}}\nputs ok",
    // Functions compose, and their results feed ordinary arithmetic.
    "puts [expr {abs(-3) + int(2.9) * 2}]",
    "puts [expr {max(sqrt(4), 1.5)}]",
    "set x -7\nputs [expr {abs($x)}]",
    "set x 007\nputs [expr {int($x)} ]",
    "set x 0x10\nputs [expr {abs($x)}]",
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

fn reference_output(tclsh: &PathBuf, program: &str) -> String {
    let path = std::env::temp_dir().join(format!("tclrs-math-{}.tcl", std::process::id()));
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
fn math_functions_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
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
