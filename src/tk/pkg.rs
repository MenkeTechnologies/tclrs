//! `Tcl_PkgProvideEx` and the version arithmetic behind it, ported from
//! `generic/tclPkg.c`.
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
//! (`generic/tclPkg.c:1655-1747`) — and then compares those numbers
//! segment by segment as strings of equal length, which is what lifts the old
//! 32-bit ceiling on a version component (`generic/tclPkg.c:1778-1929`). Both
//! are ported here rather than approximated, because "9.0" and "9.0.0" and
//! "09.0" all have to answer the same way and none of those is a string
//! comparison.

use std::ffi::{c_char, c_int, c_void, CStr};
use std::sync::Mutex;

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

/// One `Package` (`generic/tclPkg.c:47-58`), minus the `availPtr` chain, which
/// only `package ifneeded` fills and nothing here runs.
struct Package {
    name: String,
    version: String,
    client_data: *mut c_void,
}

/// The package table.
///
/// Tcl's is `iPtr->packageTable`, one per interpreter
/// (`generic/tclPkg.c:113-116`). This host has one live interpreter at a time
/// plus the short-lived second one Tk makes for its option database, and Tk
/// provides packages only into the first, so a single table is the same table.
/// The pointer is not `Send`, hence the newtype.
struct Packages(Vec<Package>);
unsafe impl Send for Packages {}

static PACKAGES: Mutex<Packages> = Mutex::new(Packages(Vec::new()));

/// Every provided package as `(name, version)`, in the order it was provided.
///
/// The measurement this module exists to make: `Tk_Init` returning `TCL_OK`
/// and this list holding `Tk` and `tk` are the same fact seen from two sides.
pub fn provided() -> Vec<(String, String)> {
    PACKAGES
        .lock()
        .expect("package table poisoned")
        .0
        .iter()
        .map(|p| (p.name.clone(), p.version.clone()))
        .collect()
}

/// The client data a package was provided with — `&tkStubs` for `tk`
/// (`tk9.0.4/generic/tkWindow.c:3466`).
pub fn client_data(name: &str) -> *mut c_void {
    PACKAGES
        .lock()
        .expect("package table poisoned")
        .0
        .iter()
        .find(|p| p.name == name)
        .map(|p| p.client_data)
        .unwrap_or(std::ptr::null_mut())
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

    // `FindPackage` (`generic/tclPkg.c:1571-1594`): create on first mention.
    let existing = {
        let table = PACKAGES.lock().expect("package table poisoned");
        table.0.iter().find(|p| p.name == name).map(|p| p.version.clone())
    };

    let Some(previous) = existing else {
        // `pkgPtr->version == NULL` (`generic/tclPkg.c:165-170`).
        PACKAGES
            .lock()
            .expect("package table poisoned")
            .0
            .push(Package {
                name,
                version,
                client_data: client_data as *mut c_void,
            });
        return TCL_OK;
    };

    // `generic/tclPkg.c:174-180`.
    let Some(pvi) = check_version_and_convert(&previous) else {
        set_bad_version(interp, &previous);
        return TCL_ERROR;
    };
    let Some(vi) = check_version_and_convert(&version) else {
        set_bad_version(interp, &version);
        return TCL_ERROR;
    };

    if compare_versions(&pvi, &vi).0 == 0 {
        // `generic/tclPkg.c:186-191`: same version, adopt the new client data
        // if there is one, and say nothing.
        if !client_data.is_null() {
            let mut table = PACKAGES.lock().expect("package table poisoned");
            if let Some(p) = table.0.iter_mut().find(|p| p.name == name) {
                p.client_data = client_data as *mut c_void;
            }
        }
        return TCL_OK;
    }

    // `generic/tclPkg.c:192-198`.
    host::set_result_bytes(
        interp,
        format!("conflicting versions provided for package \"{name}\": {previous}, then {version}")
            .as_bytes(),
    );
    TCL_ERROR
}

/// The `error:` label of `CheckVersionAndConvert`
/// (`generic/tclPkg.c:1743-1747`).
///
/// # Safety
/// `interp` must be a `Tcl_Interp *` this crate handed out, or NULL.
unsafe fn set_bad_version(interp: *mut c_void, text: &str) {
    if !interp.is_null() {
        host::set_result_bytes(
            interp,
            format!("expected version number but got \"{text}\"").as_bytes(),
        );
    }
}

/// `CheckVersionAndConvert` (`generic/tclPkg.c:1654-1747`): validate, and
/// return the normalised internal representation.
///
/// The rules are TIP 268's, quoted at `generic/tclPkg.c:1676-1687`: the first
/// character is a digit; every other is a digit, `.`, `a` or `b`; at most one
/// of `a`/`b`; and neither may sit next to a `.`. The conversion turns `.`
/// into ` 0 `, `a` into ` -2 ` and `b` into ` -1 `, so ordering falls out of a
/// plain numeric comparison of the pieces. Everything from a `+` onward is
/// ignored (`generic/tclPkg.c:1692`).
///
/// `None` is the C's `TCL_ERROR`. The `stable` out-parameter is dropped: only
/// `package require`'s selection logic reads it, and nothing here calls that.
fn check_version_and_convert(version: &str) -> Option<String> {
    let b = version.as_bytes();
    // Rule 1 (`generic/tclPkg.c:1689-1691`).
    if b.first().is_none_or(|c| !c.is_ascii_digit()) {
        return None;
    }
    let mut out = String::new();
    out.push(b[0] as char);
    let mut has_unstable = false;
    let mut prev = b[0];
    for &c in &b[1..] {
        if c == b'+' {
            break;
        }
        // Rules 2, 4 and 5 (`generic/tclPkg.c:1696-1703`), in the C's own
        // order so the reading is checkable against it.
        let structural = c == b'.' || c == b'a' || c == b'b';
        if !c.is_ascii_digit()
            && (!structural
                || (has_unstable && (c == b'a' || c == b'b'))
                || ((prev == b'a' || prev == b'b' || prev == b'.') && c == b'.')
                || (structural && prev == b'.'))
        {
            return None;
        }
        if c == b'a' || c == b'b' {
            has_unstable = true;
        }
        match c {
            b'.' => out.push_str(" 0 "),
            b'a' => out.push_str(" -2 "),
            b'b' => out.push_str(" -1 "),
            _ => out.push(c as char),
        }
        prev = c;
    }
    // A version may not end on a separator (`generic/tclPkg.c:1729`).
    if prev == b'.' || prev == b'a' || prev == b'b' {
        return None;
    }
    Some(out)
}

/// `CompareVersions` (`generic/tclPkg.c:1777-1929`).
///
/// Returns `(ordering, is_major)`: -1/0/1 as the C does, and whether the
/// difference was in the first segment, which is the C's `isMajorPtr`.
///
/// The comparison is deliberately not numeric. Leading zeros are skipped and
/// then a *shorter* run of digits is the smaller number, with `strcmp` only
/// between runs of equal length (`generic/tclPkg.c:1871-1887`) — which is what
/// makes a version component wider than 64 bits compare correctly.
fn compare_versions(v1: &str, v2: &str) -> (c_int, bool) {
    let a = v1.as_bytes();
    let b = v2.as_bytes();
    let (mut s1, mut s2) = (0usize, 0usize);
    let mut this_is_major = true;
    let res;
    loop {
        while s1 < a.len() && a[s1] == b'0' {
            s1 += 1;
        }
        while s2 < b.len() && b[s2] == b'0' {
            s2 += 1;
        }
        let neg1 = a.get(s1) == Some(&b'-');
        let neg2 = b.get(s2) == Some(&b'-');
        // Signs first, as a shortcut (`generic/tclPkg.c:1830-1839`).
        if neg1 && !neg2 {
            res = -1;
            break;
        }
        if !neg1 && neg2 {
            res = 1;
            break;
        }
        let flip = neg1 && neg2;
        if flip {
            s1 += 1;
            s2 += 1;
        }

        let e1 = s1 + a[s1..].iter().position(|c| *c == b' ').unwrap_or(a.len() - s1);
        let e2 = s2 + b[s2..].iter().position(|c| *c == b' ').unwrap_or(b.len() - s2);

        let mut r: c_int = if e1 - s1 < e2 - s2 {
            -1
        } else if e2 - s2 < e1 - s1 {
            1
        } else {
            match a[s1..e1].cmp(&b[s2..e2]) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            }
        };
        if r != 0 {
            if flip {
                r = -r;
            }
            res = r;
            break;
        }

        // `generic/tclPkg.c:1906-1921`: advance past the separator, and stop
        // when both sides are exhausted at the same time.
        s1 = e1;
        s2 = e2;
        if s1 < a.len() {
            s1 += 1;
        } else if s2 >= b.len() {
            res = 0;
            break;
        }
        if s2 < b.len() {
            s2 += 1;
        }
        this_is_major = false;
    }
    (res, this_is_major)
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
