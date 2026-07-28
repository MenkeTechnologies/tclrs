//! Inline `rust { ... }` blocks, through the binary that compiles them.
//!
//! The unit tests in `src/rust_ffi.rs` cover the rewrite. These cover the rest
//! of it: rustc is invoked, the library is loaded, and the exported function is
//! callable as a Tcl command — which is the only way to know the dispatch, the
//! marshalling and the cache agree.
//!
//! Each test writes its own script, and each block is distinct: `fusevm::ffi`
//! caches a compiled block by the hash of its body, so a shared body would let
//! one test's compilation stand in for another's.

use std::path::PathBuf;
use std::process::Command;

const TCLRS: &str = env!("CARGO_BIN_EXE_tclrs");

struct Run {
    stdout: String,
    stderr: String,
    status: Option<i32>,
}

fn run(script: &str, name: &str) -> Run {
    let path = std::env::temp_dir().join(format!("tclrs-ffi-{name}.tcl"));
    std::fs::write(&path, script).expect("write script");
    let out = Command::new(PathBuf::from(TCLRS))
        .arg(&path)
        .output()
        .expect("run tclrs");
    Run {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        status: out.status.code(),
    }
}

#[test]
fn an_exported_function_is_callable_as_a_command() {
    let run = run(
        r#"rust {
    pub extern "C" fn ffi_add(a: i64, b: i64) -> i64 { a + b }
    pub extern "C" fn ffi_triple(x: i64) -> i64 { x * 3 }
}
puts [ffi_add 21 21]
puts [ffi_triple 14]
"#,
        "callable",
    );
    assert_eq!(run.stderr, "");
    assert_eq!(run.stdout, "42\n42\n");
    assert_eq!(run.status, Some(0));
}

/// The value comes back as a Tcl value, so it composes with the rest of the
/// language rather than only being printable.
#[test]
fn a_result_is_an_ordinary_tcl_value() {
    let run = run(
        r#"rust {
    pub extern "C" fn ffi_square(x: i64) -> i64 { x * x }
}
set total 0
foreach n {1 2 3 4} {
    set total [expr {$total + [ffi_square $n]}]
}
puts $total
"#,
        "value",
    );
    assert_eq!(run.stderr, "");
    assert_eq!(run.stdout, "30\n");
}

/// Strings cross too, in the one spelling fusevm's marshalling accepts:
/// `*const c_char`, with `c_char`, `CStr` and `CString` already in scope.
#[test]
fn a_string_argument_and_a_string_result_cross_the_boundary() {
    let run = run(
        r#"rust {
    pub extern "C" fn ffi_shout(s: *const c_char) -> *const c_char {
        let text = unsafe { CStr::from_ptr(s) }.to_string_lossy().to_uppercase();
        CString::new(text).unwrap().into_raw()
    }
}
puts [ffi_shout hello]
"#,
        "string",
    );
    assert_eq!(run.stderr, "");
    assert_eq!(run.stdout, "HELLO\n");
}

/// A script's own procedure wins over an exported function of the same name:
/// dispatch asks the procedures first, so loading a library cannot quietly
/// replace something the script defined.
#[test]
fn a_procedure_of_the_same_name_shadows_the_exported_one() {
    let run = run(
        r#"rust {
    pub extern "C" fn ffi_shadowed(x: i64) -> i64 { x + 1 }
}
proc ffi_shadowed {x} {return "tcl $x"}
puts [ffi_shadowed 1]
"#,
        "shadow",
    );
    assert_eq!(run.stderr, "");
    assert_eq!(run.stdout, "tcl 1\n");
}

/// Rust that does not compile fails the script, with rustc's own message and
/// the line the block was written on — the rewrite pads itself so the position
/// survives it.
#[test]
fn a_block_that_does_not_compile_fails_the_script() {
    let run = run(
        r#"set x 1
rust {
    pub extern "C" fn ffi_broken(a: i64) -> i64 { a + }
}
puts unreached
"#,
        "broken",
    );
    assert_eq!(run.stdout, "");
    assert!(run.stderr.contains("rustc failed"), "{}", run.stderr);
    assert!(run.stderr.contains("line 2"), "{}", run.stderr);
    assert_eq!(run.status, Some(1));
}

/// Calling one with the wrong number of arguments is refused by the
/// marshalling rather than mis-read off the stack.
#[test]
fn the_wrong_argument_count_is_refused() {
    let run = run(
        r#"rust {
    pub extern "C" fn ffi_needs_two(a: i64, b: i64) -> i64 { a + b }
}
puts [ffi_needs_two 1]
"#,
        "arity",
    );
    assert_eq!(run.stdout, "");
    assert!(run.stderr.contains("expects 2 args"), "{}", run.stderr);
    assert_eq!(run.status, Some(1));
}

/// A script with no block is untouched: the rewrite is a substring scan that
/// finds nothing, and nothing about the run changes.
#[test]
fn a_script_without_a_block_is_unaffected() {
    let run = run("puts rust\nset rust 1\nputs $rust\n", "none");
    assert_eq!(run.stderr, "");
    assert_eq!(run.stdout, "rust\n1\n");
}
