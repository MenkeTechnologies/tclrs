//! A Tk session in the product binary: what `tclrs --tk` opens, what `package
//! require Tk` does inside it, and the loop the application sits in.
//!
//! Everything here already existed as the `tk-host` probe binary
//! (`src/bin/tk_host.rs`), which builds the hosting table, `dlopen`s libtk,
//! calls `Tk_Init` and then evaluates whatever scripts it was given. This
//! module is that sequence taken apart so the *script* drives it: the binary
//! opens a session, the script says `package require Tk`, and the toolkit is
//! loaded at that point and not before.
//!
//! # Why the session is opened before the script is compiled
//!
//! tclrs resolves a command name while compiling
//! ([`crate::tk::dispatch`]), and it lowers an unknown name as a run-time
//! lookup only when a Tk interpreter already exists in the process
//! ([`crate::tk::dispatch::may_exist`]). A script is compiled whole and then
//! run, so a `package require Tk` on its first line happens long after
//! `button .b` on its second was lowered. If the host were built by `package
//! require`, that `button` would already be a deferred `invalid command name`
//! and no amount of loading afterwards could reach it.
//!
//! So [`open`] builds the host — the stub tables, the object types, the
//! interpreter pairing — and loads nothing. That is what makes `may_exist`
//! true for the compilation that follows. `dlopen` and `Tk_Init` wait for
//! [`load_tk`]. A `--tk` run of a script that never mentions Tk therefore
//! never opens the Tk dylib, and never reaches the window server.
//!
//! # Why the session's interpreter is the script's own
//!
//! [`crate::tk::interp::shared_for`] would otherwise make the host a fresh
//! interpreter of its own, and every callback Tk evaluated — `-command`,
//! `bind`, `after` — would run against variables and procedures the script
//! could not see. [`open`] hands it the interpreter the script is running in
//! instead, so a `-command` body is evaluated in the same interpreter that
//! registered it.
//!
//! # Why the main thread is a precondition
//!
//! `Tk_MacOSXSetupTkNotifier` installs the Aqua event source only when the
//! current run loop is the process's main run loop, and panics outright if
//! that holds on a thread AppKit does not consider the main one
//! (`tk9.0.4/macosx/tkMacOSXNotify.c:258-272`). `tclrs --tk` runs the
//! interpreter on the main thread for exactly that reason — see
//! `src/main_thread.rs` — and [`load_tk`] refuses rather than letting Tk
//! panic when something else has got here.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Mutex;

use super::{host, interp, load, notifier};

/// Whether [`open`] has run.
///
/// `package require Tk` outside a session is `can't find package Tk`, which is
/// what `tclsh` says for a package it cannot locate. Loading anyway would put
/// Tk on whichever thread the interpreter happened to be spawned on.
static OPEN: AtomicBool = AtomicBool::new(false);

/// Whether [`load_tk`] has already run to completion, so a second `package
/// require Tk` does not `dlopen` and initialise twice.
static LOADED: AtomicBool = AtomicBool::new(false);

/// What `Tk_Init` returned, and what it left as the interpreter result.
///
/// Kept because the two are a measurement rather than a diagnostic: the host
/// carries `Tk_Init` past every stub slot it asks for, and whether the last
/// statement of `tkInit` also succeeds is a property of how much of the Tcl
/// language this frontend has, not of the ABI. Nothing prints them; the tests
/// read them.
static INIT_CODE: AtomicI32 = AtomicI32::new(i32::MIN);
static INIT_RESULT: Mutex<String> = Mutex::new(String::new());

/// Open a Tk session on this thread, against `interp`.
///
/// Builds the host and pairs it with the interpreter the script will run in.
/// Loads nothing: see the module documentation for why those are two steps.
///
/// `startup` is the script file this process was started with, if it was
/// started with one. `Tcl_MainEx` records the same thing from `argv`
/// (`generic/tclMain.c:336-338`), and Tk reads it back to decide whether it is
/// running under an interactive shell that wants a console window
/// (`tk9.0.4/macosx/tkMacOSXInit.c:585`).
pub fn open(interp_handle: &crate::runtime::Interp, startup: Option<&str>) {
    if OPEN.swap(true, Ordering::SeqCst) {
        return;
    }
    // A session is an application, not a probe: `Tk_Init` alone serves 2726
    // stub calls, and a line each on stderr is 2726 lines the script did not
    // ask for. `TCLRS_TK_TRACE` puts the log back for anyone measuring through
    // this binary rather than through `tk-host`.
    super::trace::set_logging(std::env::var_os("TCLRS_TK_TRACE").is_some());
    let tk_interp = host::build_hosting();
    if let Some(path) = startup {
        host::set_startup_file(path);
    }
    // `build_hosting` has already registered a pairing for this host, made
    // with an interpreter of its own; replace it with the script's before
    // anything can evaluate through it.
    let host_ptr = unsafe { interp::host_of(tk_interp as *mut c_void) };
    interp::adopt(host_ptr, interp_handle.shared_handle());
}

/// Whether a session is open on this process.
pub fn is_open() -> bool {
    OPEN.load(Ordering::SeqCst)
}

/// `package require Tk`, from [`crate::cmd_package`].
///
/// `dlopen` the toolkit, hand it the session's interpreter and call `Tk_Init`.
/// Tk registers its commands into that interpreter as it goes
/// (`tk9.0.4/generic/tkWindow.c:1004-1096`) and provides itself as `Tk` and
/// `tk` (`:3461-3469`) — through this crate's `Tcl_PkgProvideEx`, into the
/// registry `package require` is asking about, which is what makes the answer
/// come out of the ordinary lookup rather than out of a special case here.
///
/// # What a `TCL_ERROR` from `Tk_Init` means, and why it is not the answer
///
/// `Tk_Init` returns the completion code of its *last statement*, which
/// evaluates `tkInit` (`tk9.0.4/generic/tkWindow.c:3508-3518`) — long after the
/// two provides. So the toolkit can be initialised, the main window created and
/// the commands registered, and the return code still be `TCL_ERROR` because
/// the trailing script used a piece of the Tcl language this frontend does not
/// have yet. What decides whether the package is there is whether it was
/// provided, and that is what this reports: an error only when `Tk_Init`
/// failed *and* left no `Tk` behind.
pub fn load_tk() -> Result<(), String> {
    if LOADED.load(Ordering::SeqCst) {
        return Ok(());
    }
    if !is_open() {
        return Err("Tk can only be initialised in a session started with tclrs --tk".to_string());
    }
    if !notifier::on_main_run_loop() {
        // Tk would call `Tcl_Panic` from inside `Tk_MacOSXSetupTkNotifier`
        // (`tk9.0.4/macosx/tkMacOSXNotify.c:259-266`), which does not return.
        return Err("Tk must be initialised on the process main thread".to_string());
    }

    let lib = load::Libtk::open()?;
    let tk_interp = host::primary_interp();
    assert!(!tk_interp.is_null(), "a session has a primary interpreter");

    let code = unsafe { load::call_tk_init(&lib, tk_interp) }?;
    let result = String::from_utf8_lossy(&unsafe { host::result_bytes(tk_interp as *mut c_void) })
        .into_owned();
    INIT_CODE.store(code, Ordering::SeqCst);
    *INIT_RESULT.lock().expect("init result poisoned") = result.clone();
    LOADED.store(true, Ordering::SeqCst);
    // Under the same switch as the call log, and for the same reason: this is
    // a measurement, and an application's stderr is not the place for one.
    if std::env::var_os("TCLRS_TK_TRACE").is_some() {
        eprintln!(
            "tkinit code {code} after {} served calls, result {result:?}",
            super::trace::served()
        );
    }

    match crate::cmd_package::provided_version("Tk") {
        Some(_) => Ok(()),
        None => Err(match result.is_empty() {
            true => format!("Tk_Init failed with completion code {code}"),
            false => result,
        }),
    }
}

/// What `Tk_Init` returned and left behind, or `None` if it has not been
/// called in this process.
pub fn init_report() -> Option<(i32, String)> {
    match INIT_CODE.load(Ordering::SeqCst) {
        i32::MIN => None,
        code => Some((
            code,
            INIT_RESULT.lock().expect("init result poisoned").clone(),
        )),
    }
}

/// `Tcl_MainLoopProc *`, as `Tcl_SetMainLoop` takes one
/// (`generic/tcl.h:643`).
type MainLoopProc = unsafe extern "C" fn();

/// Sit in Tk's own main loop until the application's last window is gone.
///
/// This is not a loop written here. Tk registers `Tk_MainLoop` with
/// `Tcl_SetMainLoop` as it finishes initialising
/// (`tk9.0.4/generic/tkWindow.c:3477`), the host records the pointer
/// ([`notifier::main_loop_proc`]), and this calls it. `Tk_MainLoop` is
/// `while (Tk_GetNumMainWindows() > 0) Tcl_DoOneEvent(0);`
/// (`tk9.0.4/generic/tkEvent.c`), and the `Tcl_DoOneEvent` it calls is this
/// crate's ported notifier through the stub table — so the events it services
/// are the ones [`notifier`] queued.
///
/// Returns immediately when Tk was never loaded, which is what a `--tk` run of
/// a script that does not mention Tk does.
pub fn main_loop() {
    let proc_ = notifier::main_loop_proc();
    if proc_.is_null() {
        return;
    }
    // SAFETY: the pointer is what Tk passed to `Tcl_SetMainLoop`, which takes a
    // `void (*)(void)`; it stays valid for the life of the process because the
    // dylib is never closed.
    let main_loop: MainLoopProc = unsafe { std::mem::transmute(proc_) };
    unsafe { main_loop() };
}
