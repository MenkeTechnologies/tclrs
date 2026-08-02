//! `Tcl_PkgProvideEx`, the stub slot, over the same registry the `package`
//! command uses.
//!
//! `Tk_Init`'s last act is to provide itself twice — as `Tk` and as `tk`, both
//! at `TK_PATCH_LEVEL`, both carrying `&tkStubs` as client data
//! (`tk9.0.4/generic/tkWindow.c:3461-3469`) — and it returns whatever the
//! second one returns. So this is the slot between a host and a `Tk_Init` that
//! completes, and the only interesting thing it can do is refuse: a second
//! provide of the same name at a different version is the one error path
//! (`generic/tclPkg.c:194-199`).
//!
//! Deciding "different" is not string inequality. Tcl normalises a version to a
//! space-separated list of signed integers first — `1.2a3` becomes
//! `1 0 2 -2 3`, so an alpha release sorts below the release it precedes
//! (`generic/tclPkg.c:1655-1747`) — and then compares those numbers segment by
//! segment as strings of equal length, which is what lifts the old 32-bit
//! ceiling on a version component (`generic/tclPkg.c:1778-1929`).
//!
//! Both of those, and the table they operate on, live in
//! [`crate::cmd_package`]: it is not behind the `tk` feature, and `package
//! vcompare` in an ordinary build needs the same arithmetic. That is also what
//! makes `package require Tk` in a script and Tk's own provide two doors onto
//! one registry rather than two registries that have to be kept in step.

use std::ffi::{c_char, c_int, c_void, CStr};

use super::abi::{RawStub, TclStubs, TCL_ERROR, TCL_OK};
use super::generated::TCL_NAMES;
use super::host;
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

/// Every provided package as `(name, version)`, in the order it was provided.
///
/// The measurement this module exists to make: `Tk_Init` returning `TCL_OK`
/// and this list holding `Tk` and `tk` are the same fact seen from two sides.
pub fn provided() -> Vec<(String, String)> {
    crate::cmd_package::provided()
}

/// The client data a package was provided with — `&tkStubs` for `tk`
/// (`tk9.0.4/generic/tkWindow.c:3466`).
pub fn client_data(name: &str) -> *mut c_void {
    crate::cmd_package::client_data(name) as *mut c_void
}

/// The version ordering `Tcl_PkgProvideEx` decides "same version" with:
/// normalise both, then compare. `None` means one of them is not a version
/// number at all, which is the C's `TCL_ERROR` from `CheckVersionAndConvert`.
pub fn compare(a: &str, b: &str) -> Option<c_int> {
    crate::cmd_package::compare(a, b)
}

/// Slot 0: `int Tcl_PkgProvideEx(Tcl_Interp *, const char *, const char *,
/// const void *)` (`generic/tclPkg.c:154-200`).
///
/// # Safety
/// `name` and `version` must be NUL-terminated strings; `interp` must be a
/// `Tcl_Interp *` this crate handed out, or NULL.
pub unsafe extern "C" fn pkg_provide_ex(
    interp: *mut c_void,
    name: *const c_char,
    version: *const c_char,
    client_data: *const c_void,
) -> c_int {
    entered!("tcl_PkgProvideEx");
    let name = CStr::from_ptr(name).to_string_lossy().into_owned();
    let version = CStr::from_ptr(version).to_string_lossy().into_owned();

    match crate::cmd_package::provide(&name, &version, client_data as usize) {
        Ok(()) => TCL_OK,
        Err(msg) => {
            // Every failure of the C sets the interpreter result and returns
            // TCL_ERROR (`generic/tclPkg.c:174-196`); the wording is the same
            // whether the version was malformed or the two conflicted.
            if !interp.is_null() {
                host::set_result_bytes(interp, msg.as_bytes());
            }
            TCL_ERROR
        }
    }
}

/// Patch this module's slot into `t`, returning its index.
///
/// # Safety
/// The erased signature is the one `tclDecls.h` gives the slot, quoted on the
/// line above it.
pub unsafe fn install_impls(t: &mut TclStubs) -> Vec<usize> {
    vec![
        // int (*tcl_PkgProvideEx)(Tcl_Interp *, const char *, const char *,
        //     const void *) /* 0 */
        install(t, "tcl_PkgProvideEx", pkg_provide_ex as *const ()),
    ]
}

/// As [`host`]'s own installer: by name, never by literal index.
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
