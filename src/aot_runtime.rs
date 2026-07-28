//! The link symbol an AOT-compiled Tcl binary needs.
//!
//! fusevm's AOT model embeds the serialized chunk in the object and, at
//! startup, rebuilds a VM from it and calls back into the frontend through the
//! C symbol `fusevm_aot_register_builtins` before running the native driver.
//! That callback is this crate's only contribution to the linked binary: it
//! installs the same numeric hook and extension dispatch the interpreter uses
//! ([`crate::runtime::install_hooks`]), so a script means the same thing
//! whichever tier runs it, and buffers the script's output.
//!
//! An error raised by an extension op is written to stderr here rather than
//! returned: fusevm's AOT entry owns the VM and maps its result to an exit
//! code, so there is no caller left to hand a message to.

use std::sync::Mutex;

use fusevm::VM;

/// Install the Tcl runtime on the VM an AOT binary is about to run.
///
/// # Safety
/// `vm` must point to a live, exclusively-borrowable `VM` — fusevm's AOT entry
/// (`fusevm_aot_run_embedded`) passes one it owns for the length of the call.
#[no_mangle]
pub unsafe extern "C" fn fusevm_aot_register_builtins(vm: *mut VM) {
    // SAFETY: the contract above; the AOT entry hands over a VM it owns and
    // does not touch until this returns.
    let vm = unsafe { &mut *vm };
    let errors = crate::runtime::install_hooks(vm);
    crate::runtime::install_stdout_sink(vm);
    // The run happens after this returns and fusevm's AOT entry owns it, so
    // the error cell has to outlive the call: park it here for the binary's
    // `main` to drain through `tclrs_aot_report_error`.
    *PARKED.lock().expect("parked lock") = Some(errors);
}

/// The error cell of the run in progress, readable after fusevm's AOT entry
/// has dropped the VM.
static PARKED: Mutex<Option<crate::runtime::Errors>> = Mutex::new(None);

/// Drain and report an extension-op error left by an AOT run.
///
/// Called from the AOT binary's exit path — fusevm's `fusevm_aot_run_embedded`
/// maps the VM result to an exit code but cannot see an error an extension op
/// parked, so the C `main` calls this before exiting.
///
/// Returns 1 when an error was reported, 0 otherwise.
#[no_mangle]
pub extern "C" fn tclrs_aot_report_error() -> i32 {
    let parked = PARKED.lock().expect("parked lock").take();
    match parked.and_then(|e| e.take()) {
        Some(msg) => {
            eprintln!("tclrs: {msg}");
            1
        }
        None => 0,
    }
}
