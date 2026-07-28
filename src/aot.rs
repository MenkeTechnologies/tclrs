//! `tclrs --aot`: a Tcl script compiled ahead of time to a native executable.
//!
//! The lowering is the same one the interpreter uses — `parser` → `compiler` →
//! `fusevm::Chunk` — and `fusevm::aot::compile_object` takes it from there,
//! emitting a relocatable object that carries a native driver for the chunk
//! plus the serialized chunk itself. Linking that object against this crate's
//! staticlib (which supplies the `fusevm_aot_register_builtins` hook in
//! [`crate::aot_runtime`] and, through it, fusevm's AOT runtime) produces a
//! standalone binary that runs the script with no parse, no lowering, and no
//! bytecode dispatch loop of its own.
//!
//! What the native code covers is fusevm's decision, not this crate's: scalar
//! arithmetic, comparisons, branches and globals lower to registers, and an op
//! the closed-world compiler has no lowering for — every `Op::Extended` this
//! frontend emits among them — becomes a deopt point that hands the rest of the
//! run to the interpreter. See the AOT section of the README for what that
//! means for a Tcl script in practice.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use fusevm::{VMResult, VM};

use crate::runtime::{to_tcl_string, Errors, Outcome};

/// Compile a Tcl script to a relocatable object at `out`.
///
/// The object is not runnable on its own: it imports the fusevm AOT runtime
/// shims and this crate's `fusevm_aot_register_builtins`. Use
/// [`compile_executable`] for a binary, or link this object yourself against
/// `libtclrs.a`.
pub fn compile_object(src: &str, out: &Path) -> Result<(), String> {
    let chunk = crate::runtime::compile(src)?;
    fusevm::aot::compile_object(&chunk, out)
}

/// Compile a script through the AOT compiler in-process and run it, capturing
/// its output — the same codegen `compile_object` writes to disk, driven by
/// Cranelift's in-memory module instead.
///
/// This is how the AOT path is tested against the interpreter without needing
/// a C toolchain or a built staticlib, and how the in-process benchmark
/// measures native execution.
pub fn run_native(src: &str) -> Result<Outcome, String> {
    let chunk = crate::runtime::compile(src)?;
    let output = Arc::new(Mutex::new(String::new()));
    let errors: Arc<Mutex<Option<Errors>>> = Arc::new(Mutex::new(None));

    let sink = Arc::clone(&output);
    let cell = Arc::clone(&errors);
    let outcome = fusevm::aot::run_chunk_native(&chunk, move |vm: &mut VM| {
        *cell.lock().expect("error lock") = Some(crate::runtime::install_hooks(vm));
        vm.set_output_sink(Box::new(move |s: &str| {
            sink.lock().expect("output lock").push_str(s);
        }));
    })?;

    if let Some(msg) = errors
        .lock()
        .expect("error lock")
        .take()
        .and_then(|e| e.take())
    {
        return Err(msg);
    }
    let output = output.lock().expect("output lock").clone();
    match outcome {
        VMResult::Ok(v) => Ok(Outcome {
            result: to_tcl_string(&v),
            output,
        }),
        VMResult::Halted => Ok(Outcome {
            result: String::new(),
            output,
        }),
        VMResult::Error(e) => Err(e),
    }
}

/// Compile a Tcl script all the way to a standalone native executable.
///
/// Emits the AOT object, writes a C `main` that calls fusevm's
/// `fusevm_aot_run_embedded`, and links the two against `libtclrs.a`.
pub fn compile_executable(src: &str, out: &Path) -> Result<(), String> {
    let stem = format!("tclrs_aot_{}", std::process::id());
    let tmp = std::env::temp_dir();
    let obj = tmp.join(format!("{stem}.o"));
    let main_c = tmp.join(format!("{stem}.c"));
    compile_object(src, &obj)?;

    std::fs::write(
        &main_c,
        // `fusevm_aot_run_embedded` rebuilds the VM from the embedded chunk,
        // calls this crate's register hook, runs the native driver and returns
        // the script's exit status; `tclrs_aot_report_error` then drains an
        // error an extension op parked, which the VM result cannot carry.
        "extern long fusevm_aot_run_embedded(void);\n\
         extern int tclrs_aot_report_error(void);\n\
         int main(void) {\n\
         \tlong status = fusevm_aot_run_embedded();\n\
         \tif (tclrs_aot_report_error()) return 1;\n\
         \treturn (int)status;\n\
         }\n",
    )
    .map_err(|e| format!("aot: write {}: {e}", main_c.display()))?;

    let lib = staticlib_path()?;
    let mut cmd = Command::new("cc");
    cmd.arg(&main_c).arg(&obj).arg(&lib).arg("-o").arg(out);
    // Platform libraries the Rust staticlib needs.
    if cfg!(target_os = "macos") {
        cmd.args(["-framework", "CoreFoundation", "-liconv"]);
    } else {
        cmd.args(["-lpthread", "-ldl", "-lm"]);
    }
    let status = cmd.status().map_err(|e| format!("aot: cc: {e}"))?;
    let _ = std::fs::remove_file(&main_c);
    let _ = std::fs::remove_file(&obj);
    if !status.success() {
        return Err(format!("aot: link failed (cc exit {:?})", status.code()));
    }
    Ok(())
}

/// Locate `libtclrs.a`: `$TCLRS_STATICLIB`, else a sibling of the running
/// binary (where `cargo build` puts it), else the `deps` directory beside it.
fn staticlib_path() -> Result<PathBuf, String> {
    if let Ok(p) = std::env::var("TCLRS_STATICLIB") {
        return Ok(PathBuf::from(p));
    }
    let exe = std::env::current_exe().map_err(|e| format!("aot: current exe: {e}"))?;
    let dir = exe
        .parent()
        .ok_or("aot: no directory for the running binary")?;
    // Beside the binary is where `cargo build` leaves it; a test binary lives
    // one level deeper, in `deps/`.
    for candidate in [
        dir.join("libtclrs.a"),
        dir.join("deps").join("libtclrs.a"),
        dir.join("..").join("libtclrs.a"),
    ] {
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(format!(
        "aot: libtclrs.a not found beside {}; build the staticlib or set TCLRS_STATICLIB",
        exe.display()
    ))
}
