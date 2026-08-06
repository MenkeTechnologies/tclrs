//! `Tcl_Preserve`, `Tcl_Release` and `Tcl_EventuallyFree`, ported from
//! `generic/tclPreserve.c`.
//!
//! Tk's widget records outlive the call that frees them: a binding script can
//! destroy a widget from inside a callback the widget itself is running, and
//! the C after the callback still dereferences the record. Tcl's answer is not
//! a reference count on the object but a side table of *addresses currently
//! being used* — `Tcl_Preserve(ptr)` says "do not actually free this yet",
//! `Tcl_EventuallyFree(ptr, proc)` says "free it when nobody is using it", and
//! `Tcl_Release(ptr)` performs the deferred free when the last user goes
//! (`generic/tclPreserve.c:17-46`).
//!
//! The mechanism has to be the host's, not Tk's: Tk stores nothing itself, and
//! the free procedure it hands over is Tk's own code that must run at the right
//! moment or a widget leaks — or worse, is freed while a `Tk_Window` still
//! points at it.
//!
//! Two of the C's panics are reproduced, because both catch a caller bug that
//! is otherwise silent: releasing an address that was never preserved
//! (`generic/tclPreserve.c:186-191`), and asking for the same block to be freed
//! twice (`generic/tclPreserve.c:218-220`).

use std::ffi::{c_int, c_void};
use std::sync::Mutex;

use super::abi::{RawStub, TclStubs};
use super::generated::TCL_NAMES;
use super::trace::{record, Table};

macro_rules! entered {
    ($name:literal) => {
        record(
            Table::Tcl,
            TCL_NAMES
                .iter()
                .position(|n| *n == $name)
                .expect("no such slot"),
        )
    };
}

/// `TCL_DYNAMIC` (`generic/tcl.h:998`): the sentinel that means "the block came
/// from `Tcl_Alloc`, free it with `Tcl_Free`" rather than a callable address.
const TCL_DYNAMIC: usize = 3;

/// `Reference` (`generic/tclPreserve.c:23-31`).
#[derive(Clone, Copy)]
struct Reference {
    client_data: usize,
    ref_count: usize,
    must_free: bool,
    free_proc: usize,
}

/// `refArray` (`generic/tclPreserve.c:38-43`), behind the C's `preserveMutex`.
static REFS: Mutex<Vec<Reference>> = Mutex::new(Vec::new());

/// How many blocks are preserved right now. The measurement that says the
/// deferred-free path was exercised at all.
pub fn preserved() -> usize {
    REFS.lock().expect("preserve table poisoned").len()
}

/// Run a `Tcl_FreeProc *`, or `Tcl_Free`, per the C's `TCL_DYNAMIC` test
/// (`generic/tclPreserve.c:169-173`, `:236-240`).
///
/// # Safety
/// `free_proc` is `TCL_DYNAMIC` or a `void (*)(void *)`, and `client_data` is
/// the block it was registered for.
unsafe fn dispose(client_data: usize, free_proc: usize) {
    if free_proc == TCL_DYNAMIC {
        libc::free(client_data as *mut c_void);
    } else {
        let f: unsafe extern "C" fn(*mut c_void) = std::mem::transmute(free_proc);
        f(client_data as *mut c_void);
    }
}

/// Slot 201. `void Tcl_Preserve(void *clientData)`
/// (`generic/tclPreserve.c:110-152`).
///
/// # Safety
/// `client_data` is an address the caller intends to keep alive.
pub unsafe extern "C" fn preserve(client_data: *mut c_void) {
    entered!("tcl_Preserve");
    let key = client_data as usize;
    let mut refs = REFS.lock().expect("preserve table poisoned");
    if let Some(r) = refs.iter_mut().find(|r| r.client_data == key) {
        r.ref_count += 1;
        return;
    }
    refs.push(Reference {
        client_data: key,
        ref_count: 1,
        must_free: false,
        free_proc: 0,
    });
}

/// Slot 202. `void Tcl_Release(void *clientData)`
/// (`generic/tclPreserve.c:166-195`).
///
/// The lock is dropped before the free procedure runs — the C says why in so
/// many words (`generic/tclPreserve.c:154-163`): a `freeProc` is allowed to
/// call `Tcl_Preserve` on the same address, and holding the mutex across it
/// would deadlock.
///
/// # Safety
/// `client_data` must have been passed to [`preserve`] and not yet released.
pub unsafe extern "C" fn release(client_data: *mut c_void) {
    entered!("tcl_Release");
    let key = client_data as usize;
    let disposal = {
        let mut refs = REFS.lock().expect("preserve table poisoned");
        let Some(i) = refs.iter().position(|r| r.client_data == key) else {
            // `generic/tclPreserve.c:186-191`, which is a `Tcl_Panic`.
            panic!("Tcl_Release couldn't find reference for {client_data:?}");
        };
        if refs[i].ref_count > 1 {
            refs[i].ref_count -= 1;
            return;
        }
        let r = refs.swap_remove(i);
        r.must_free.then_some((r.client_data, r.free_proc))
    };
    if let Some((data, proc_)) = disposal {
        dispose(data, proc_);
    }
}

/// Slot 128. `void Tcl_EventuallyFree(void *clientData, Tcl_FreeProc *)`
/// (`generic/tclPreserve.c:211-241`): defer when the block is preserved, free
/// on the spot when it is not.
///
/// # Safety
/// `free_proc` is `TCL_DYNAMIC` or a `void (*)(void *)` that can free
/// `client_data`.
pub unsafe extern "C" fn eventually_free(client_data: *mut c_void, free_proc: *mut c_void) {
    entered!("tcl_EventuallyFree");
    let key = client_data as usize;
    {
        let mut refs = REFS.lock().expect("preserve table poisoned");
        if let Some(r) = refs.iter_mut().find(|r| r.client_data == key) {
            // `generic/tclPreserve.c:218-220`, which is a `Tcl_Panic`.
            assert!(
                !r.must_free,
                "Tcl_EventuallyFree called twice for {client_data:?}"
            );
            r.must_free = true;
            r.free_proc = free_proc as usize;
            return;
        }
    }
    dispose(key, free_proc as usize);
}

/// Slot 219. `void Tcl_SetErrno(int errorCode)` and slot 143
/// `int Tcl_GetErrno(void)` (`generic/tclPosixStr.c` / `unix/tclUnixNotfy.c`
/// use the thread's `errno` directly; `generic/tclDecls.h` gives both a slot).
///
/// # Safety
/// None beyond the ABI.
pub unsafe extern "C" fn set_errno(code: c_int) {
    entered!("tcl_SetErrno");
    *libc::__error() = code;
}

/// `int Tcl_GetErrno(void)`.
///
/// # Safety
/// None beyond the ABI.
pub unsafe extern "C" fn get_errno() -> c_int {
    entered!("tcl_GetErrno");
    *libc::__error()
}

/// Patch this module's slots into `t`, returning their indices.
///
/// # Safety
/// Each erased signature is the one `tclDecls.h` gives the slot, quoted on the
/// line above it.
pub unsafe fn install_impls(t: &mut TclStubs) -> Vec<usize> {
    vec![
        // void (*tcl_Preserve)(void *data) /* 201 */
        install(t, "tcl_Preserve", preserve as *const ()),
        // void (*tcl_Release)(void *clientData) /* 202 */
        install(t, "tcl_Release", release as *const ()),
        // void (*tcl_EventuallyFree)(void *clientData, Tcl_FreeProc *freeProc) /* 128 */
        install(t, "tcl_EventuallyFree", eventually_free as *const ()),
        // void (*tcl_SetErrno)(int err) /* 219 */
        install(t, "tcl_SetErrno", set_errno as *const ()),
        // int (*tcl_GetErrno)(void) /* 143 */
        install(t, "tcl_GetErrno", get_errno as *const ()),
    ]
}

/// As [`super::host`]'s own installer: by name, never by literal index.
///
/// # Safety
/// `f` must have the signature `tclDecls.h` gives the named slot.
unsafe fn install(t: &mut TclStubs, name: &str, f: *const ()) -> usize {
    let i = TCL_NAMES
        .iter()
        .position(|n| *n == name)
        .unwrap_or_else(|| panic!("no slot named {name} in TclStubs"));
    t.slots[i] = std::mem::transmute::<*const (), RawStub>(f);
    i
}
