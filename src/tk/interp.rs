//! One tclrs interpreter behind every `Tcl_Interp *` Tk holds.
//!
//! Phase 1's host answered questions about data structures; it had no
//! evaluator, and the run stopped the moment Tk asked for one
//! (`tk9.0.4/generic/tkOption.c:1592`). This module is the other half of the
//! join: a [`HostInterp`] — the 32 bytes Tk's `Tcl_InitStubs` reads, plus the
//! slot state behind it — is paired here with a real [`crate::runtime::Interp`],
//! and a script Tk hands over is compiled by this crate's own parser and
//! compiler and run on fusevm.
//!
//! # Why a registry rather than a field
//!
//! `Host` is the struct the stub bodies operate on and it is shared with the
//! rest of the phase's work. Keeping the pairing here, keyed by the address of
//! the `Host`, means the evaluator can be added without changing that struct's
//! shape — and the shape is what several files agree on.
//!
//! # The second interpreter
//!
//! Tk asks for one: `Tcl_CreateInterp` at
//! `tk9.0.4/generic/tkOption.c:1497`, used to hold the option database while it
//! is parsed, then `Tcl_DeleteInterp` at 1499. It has to be *independent* — a
//! variable set in it must not be visible in the first — which is exactly what
//! a second [`crate::runtime::Interp`] is, since an interpreter owns its
//! globals map and its chunk cache and shares neither.
//!
//! # Which interpreter a running script belongs to
//!
//! Evaluation re-enters: Tk calls `Tcl_EvalEx`, the script calls a command Tk
//! registered, that command calls `Tcl_EvalEx` again. So "the interpreter this
//! evaluation belongs to" is a stack, not a variable, and it is per thread
//! because a VM run never leaves the thread that started it. [`Scope`] pushes on
//! construction and pops on drop, so an evaluation that fails still unwinds the
//! stack correctly.

use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr;
use std::sync::Mutex;

use super::host::{Host, HostInterp};
use crate::runtime::Shared;

/// The pairing of one `Host` with one tclrs interpreter.
///
/// Keyed by the `Host`'s address rather than the `HostInterp`'s: the slots that
/// take a `Tcl_Interp *` reach the `Host` through it, and the `Host` is what
/// `Tcl_DeleteInterp` frees.
struct Entry {
    host: usize,
    shared: Shared,
}

/// Every live pairing. A `Mutex` rather than a thread-local because a `Host` is
/// process-wide state — `Tcl_GetThreadData` and the registered `Tcl_ObjType`s
/// already live on the primary one — while the *current* interpreter is not.
static INTERPS: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

/// Whether any Tk interpreter has been created in this process.
///
/// Read by the compiler ([`super::dispatch::may_exist`]) to decide whether an
/// unknown command name is worth lowering as a run-time lookup. False in every
/// process that never loaded Tk, which is what keeps an ordinary script's
/// lowering identical to what it was.
pub fn any_exists() -> bool {
    !INTERPS.lock().expect("Tk interpreter registry").is_empty()
}

/// The tclrs interpreter behind `host`, creating one on first use.
///
/// A `Shared` is an `Arc` over the interpreter's state, so this hands back a
/// handle rather than a borrow — which is what makes re-entrant evaluation
/// sound: no lock of this registry is held while a script runs.
pub(crate) fn shared_for(host: *mut Host) -> Shared {
    let key = host as usize;
    let mut interps = INTERPS.lock().expect("Tk interpreter registry");
    if let Some(e) = interps.iter().find(|e| e.host == key) {
        return e.shared.clone();
    }
    // Writes to the process's stdout, as `tclsh` does: a script Tk evaluates is
    // a script the user asked for, and its `puts` belongs on the terminal.
    let shared = crate::runtime::Interp::new().into_shared();
    interps.push(Entry {
        host: key,
        shared: shared.clone(),
    });
    shared
}

/// Make `shared` the interpreter behind `host`, replacing whatever
/// [`shared_for`] created.
///
/// What a session opened by `tclrs --tk` does with the interpreter the script
/// is about to run in: a callback Tk evaluates — a `-command` body, a `bind`
/// script, an `after` script — then runs against the same variables and
/// procedures the script that registered it has, rather than in an interpreter
/// of the host's own that shares nothing with it. See
/// [`crate::tk::session::open`].
pub(crate) fn adopt(host: *mut Host, shared: Shared) {
    let key = host as usize;
    let mut interps = INTERPS.lock().expect("Tk interpreter registry");
    match interps.iter_mut().find(|e| e.host == key) {
        Some(e) => e.shared = shared,
        None => interps.push(Entry { host: key, shared }),
    }
}

/// Forget the interpreter behind `host`. Called by `Tcl_DeleteInterp`; the
/// state itself goes when the last `Shared` handle does.
pub fn forget(host: *mut Host) {
    let key = host as usize;
    INTERPS
        .lock()
        .expect("Tk interpreter registry")
        .retain(|e| e.host != key);
}

thread_local! {
    /// The `Tcl_Interp *` chain of the evaluations running on this thread,
    /// outermost first.
    static CURRENT: RefCell<Vec<*mut HostInterp>> = const { RefCell::new(Vec::new()) };
}

/// The interpreter an evaluation is running in, for as long as it runs.
///
/// Held by value on the stack of whichever slot started the evaluation, so the
/// pop happens on every path out of it — including the one where the script
/// raised an error.
pub struct Scope;

impl Scope {
    /// # Safety
    /// `interp` must stay valid until the returned value is dropped.
    pub unsafe fn enter(interp: *mut HostInterp) -> Scope {
        CURRENT.with(|c| c.borrow_mut().push(interp));
        Scope
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        CURRENT.with(|c| {
            c.borrow_mut().pop();
        });
    }
}

/// The innermost interpreter an evaluation is running in on this thread, or the
/// primary one when nothing is running inside a `Tcl_Eval*` call.
///
/// The fallback is what makes a Tk command reachable from a script this process
/// started itself — `tclrs script.tcl` after `Tk_Init` — rather than only from a
/// script Tk asked to have evaluated. Null when no host has been built at all.
pub fn current() -> *mut HostInterp {
    let top = CURRENT.with(|c| c.borrow().last().copied());
    match top {
        Some(p) => p,
        None => super::host::primary_interp(),
    }
}

/// The `Host` behind a `Tcl_Interp *`, or null.
///
/// # Safety
/// `interp` is either null or a pointer this crate handed to Tk.
pub unsafe fn host_of(interp: *mut c_void) -> *mut Host {
    if interp.is_null() {
        return ptr::null_mut();
    }
    (*(interp as *mut HostInterp)).host
}
