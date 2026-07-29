//! Opening the real libtk and calling `Tk_Init`.
//!
//! `dlopen` rather than a link-time dependency, deliberately: nothing about
//! tclrs should stop building on a machine with no Tk, and an `extern` block
//! would put that machine's linker in the way. The library is opened by an
//! absolute path so there is no doubt which one was measured.

use std::ffi::{c_char, c_int, c_void, CString};
use std::path::Path;

use super::host::HostInterp;

/// Where the Tk dylib is looked for, in order, when `TCLRS_LIBTK` is unset.
///
/// Both are Homebrew's, and on an Apple Silicon machine only the first is
/// loadable: Homebrew's `/usr/local` prefix is the Intel one, and its
/// `libtcl9tk9.0.dylib` is a `Mach-O 64-bit dynamically linked shared library
/// x86_64`, which an arm64 process cannot `dlopen` at all. The two builds carry
/// byte-identical headers, so the ABI measured against either is the same, but
/// only a matching architecture can be run.
const CANDIDATES: &[&str] = &[
    "/opt/homebrew/opt/tcl-tk/lib/libtcl9tk9.0.dylib",
    "/usr/local/opt/tcl-tk/lib/libtcl9tk9.0.dylib",
];

/// A `dlopen`ed handle. Never closed: `Tk_Init` installs exit handlers and
/// Cocoa state that outlive any scope this could be dropped in.
pub struct Libtk {
    handle: *mut c_void,
    pub path: String,
}

impl Libtk {
    /// Open the Tk dylib named by `TCLRS_LIBTK`, or the first of [`CANDIDATES`]
    /// that exists.
    pub fn open() -> Result<Libtk, String> {
        let path = match std::env::var("TCLRS_LIBTK") {
            Ok(p) => p,
            Err(_) => CANDIDATES
                .iter()
                .find(|p| Path::new(p).exists())
                .map(|p| (*p).to_string())
                .ok_or_else(|| format!("no Tk dylib at any of {CANDIDATES:?}"))?,
        };
        let c = CString::new(path.clone()).map_err(|e| e.to_string())?;
        let handle = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if handle.is_null() {
            let err = unsafe { libc::dlerror() };
            let msg = if err.is_null() {
                "unknown error".to_string()
            } else {
                unsafe { std::ffi::CStr::from_ptr(err) }
                    .to_string_lossy()
                    .into_owned()
            };
            return Err(format!("dlopen({path}): {msg}"));
        }
        Ok(Libtk { handle, path })
    }

    /// Resolve a symbol, or say which one was missing.
    pub fn sym(&self, name: &str) -> Result<*mut c_void, String> {
        let c = CString::new(name).map_err(|e| e.to_string())?;
        let p = unsafe { libc::dlsym(self.handle, c.as_ptr()) };
        if p.is_null() {
            Err(format!("dlsym({name}) in {}: not found", self.path))
        } else {
            Ok(p)
        }
    }
}

/// `int Tk_Init(Tcl_Interp *interp)` — `tk9.0.4/generic/tkWindow.c:3055-3070`,
/// which forwards straight to the file-static `Initialize`.
pub type TkInit = unsafe extern "C" fn(*mut c_void) -> c_int;

/// `const char *Tk_PkgInitStubsCheck(Tcl_Interp *, const char *, int)` —
/// `tk9.0.4/generic/tkWindow.c:3558`. Not used by the probe; resolved only to
/// confirm the dylib is the one the headers describe.
pub type TkPkgInitStubsCheck =
    unsafe extern "C" fn(*mut c_void, *const c_char, c_int) -> *const c_char;

/// Hand `interp` to Tk and return what `Tk_Init` returns.
///
/// It usually does not return: the point of the exercise is that Tk stops on a
/// slot with no implementation, and that slot aborts the process after naming
/// itself.
///
/// # Safety
/// `interp` must be a table built by [`super::host::build`] and must outlive
/// the call.
pub unsafe fn call_tk_init(lib: &Libtk, interp: *mut HostInterp) -> Result<c_int, String> {
    let f: TkInit = std::mem::transmute(lib.sym("Tk_Init")?);
    Ok(f(interp as *mut c_void))
}
