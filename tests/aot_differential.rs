//! Ahead-of-time compilation must not change what a script means.
//!
//! `fusevm::aot` lowers a chunk to native code with its own rules — scalars in
//! registers, heap ops through a shim, a deopt to the interpreter where it has
//! no lowering — so the AOT path can diverge from the interpreter in ways the
//! tclsh differential suites would never see. Every program here is run both
//! ways and the output compared byte for byte.
//!
//! `run_native` drives the same `fusevm::aot` codegen `--aot` writes into an
//! object, through Cranelift's in-memory module, so this needs no C toolchain
//! and no built staticlib. The linked-binary path is covered by
//! `linked_executable_matches_the_interpreter`, which skips when either is
//! missing.

use std::path::PathBuf;
use std::process::Command;

/// Programs chosen for the shapes the AOT compiler treats differently: scalar
/// arithmetic it lowers to registers, control flow it lowers to native
/// branches, string and list work it runs through the boxed shim, and the
/// extension ops it has no lowering for at all.
const PROGRAMS: &[&str] = &[
    // An `i64` that overflows promotes rather than failing, and the two tiers
    // have to promote alike. This is the case native codegen is most likely to
    // drift on: the register path wraps unless the overflow reaches the numeric
    // hook, and a wrapped answer would differ from the interpreter's here
    // rather than merely being wrong somewhere unobserved.
    "puts [expr {9223372036854775807 + 1}]",
    "puts [expr {-9223372036854775808 - 1}]",
    "puts [expr {9223372036854775807 * 3}]",
    "puts [expr {2 ** 100}]",
    // Straight-line scalar arithmetic — the register path.
    "puts [expr {1 + 2 * 3}]",
    "puts [expr {(7 - 2) * (3 + 1)}]",
    "puts [expr {1 << 10 | 3}]",
    "puts [expr {-57 / 10}]",
    "puts [expr {3.0 / 2}]",
    "puts [expr {2 ** 10}]",
    // Globals, which AOT holds in registers under a definite-assignment
    // analysis and must spill correctly at every deopt.
    "set x 5\nset y [expr {$x * $x}]\nputs $y",
    "set x 1\nset x [expr {$x + 1}]\nset x [expr {$x + 1}]\nputs $x",
    // Native control flow.
    "if {1} {puts yes} else {puts no}",
    "if {[expr {3 > 4}]} {puts a} else {puts b}",
    "set i 0\nwhile {$i < 5} {puts $i; incr i}",
    "set i 0\nset s 0\nwhile {$i < 20} {set s [expr {$s + $i * $i}]; incr i}\nputs $s",
    "set i 0\nwhile {1} {incr i; if {$i > 3} {break}}\nputs $i",
    // Strings — boxed values through the shim.
    "set s \"\"\nset i 0\nwhile {$i < 5} {set s \"$s$i\"; incr i}\nputs $s",
    "puts \"a[expr {1+1}]b\"",
    // Lists and associative data — all extension ops, so all deopt.
    "puts [lsort [list c a b]]",
    "set out {}\nforeach x [list 1 2 3] {lappend out [expr {$x * 2}]}\nputs $out",
    "array set a {x 1 y 2}\nputs [lsort [array names a]]",
    "set d [dict create k v]\nputs [dict get $d k]",
    // Output shapes.
    "puts -nonewline a\nputs b",
    "puts {}",
];

/// Programs whose *failure* must survive AOT: an error raised by the frontend
/// mid-run has to reach the caller the same way it does interpreted.
const FAILING: &[&str] = &["puts [expr {1 / 0}]", "puts [expr {\"abc\" + 1}]"];

#[test]
fn aot_codegen_matches_the_interpreter() {
    let mut failures = Vec::new();
    for program in PROGRAMS {
        let interpreted = tclrs::eval(program);
        let native = tclrs::aot::run_native(program);
        match (interpreted, native) {
            (Ok(a), Ok(b)) if a == b => {}
            (a, b) => failures.push(format!(
                "program:\n{program}\n  interp: {a:?}\n  aot:    {b:?}"
            )),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} programs diverge under AOT:\n\n{}",
        failures.len(),
        PROGRAMS.len(),
        failures.join("\n\n")
    );
}

/// A run that fails must fail identically. This is where native codegen is
/// most likely to drift: an i64 overflow the interpreter reports through the
/// numeric hook would, lowered naively, wrap silently in a register.
#[test]
fn aot_errors_match_the_interpreter() {
    for program in FAILING {
        let interpreted = tclrs::eval(program).expect_err("must fail interpreted");
        let native = tclrs::aot::run_native(program);
        match native {
            Err(e) => assert_eq!(e, interpreted, "program:\n{program}"),
            Ok(outcome) => panic!(
                "program:\n{program}\n  interp failed: {interpreted}\n  aot succeeded: {outcome:?}"
            ),
        }
    }
}

/// The full `--aot` path: object emission, the link against `libtclrs.a`, and
/// the resulting binary's output. Skips when the staticlib has not been built
/// (`cargo test` alone does not produce it) or there is no C compiler.
#[test]
fn linked_executable_matches_the_interpreter() {
    if staticlib().is_none() {
        eprintln!("skipping: no libtclrs.a — run `cargo build` first");
        return;
    }
    if Command::new("cc").arg("--version").output().is_err() {
        eprintln!("skipping: no cc on PATH");
        return;
    }

    let program = "set i 0\nset s 0\nwhile {$i < 10} {set s [expr {$s + $i}]; incr i}\nputs \"s=$s\"\nputs [lsort [list c a b]]";
    let out = std::env::temp_dir().join(format!("tclrs-aot-test-{}", std::process::id()));
    tclrs::aot::compile_executable(program, &out).expect("link an AOT executable");

    let produced = Command::new(&out).output().expect("run the AOT binary");
    let _ = std::fs::remove_file(&out);
    assert!(
        produced.status.success(),
        "AOT binary failed: {}",
        String::from_utf8_lossy(&produced.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&produced.stdout),
        tclrs::eval(program).expect("interpreted run").output
    );
}

/// `libtclrs.a` as `cargo build` leaves it, relative to this test binary
/// (`target/<profile>/deps/<test>`) — the same place `aot::staticlib_path`
/// looks, so finding it here means the link will find it too.
fn staticlib() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let lib = exe.parent()?.parent()?.join("libtclrs.a");
    lib.exists().then_some(lib)
}
