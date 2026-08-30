//! Opening the real libtk and calling `Tk_Init`.
//!
//! `dlopen` rather than a link-time dependency, deliberately: nothing about
//! tclrs should stop building on a machine with no Tk, and an `extern` block
//! would put that machine's linker in the way. The library is opened by an
//! absolute path so there is no doubt which one was measured.
//!
//! # Why the dylib is asked where it is
//!
//! A Tk install is two halves: the dylib, and the `tk.tcl` script library that
//! `tkInit`'s last statement goes looking for
//! (`tk9.0.4/generic/tkWindow.c:3513`). The second half sits beside the first —
//! `…/lib/libtcl9tk9.0.dylib` next to `…/lib/tk9.0/tk.tcl` — so the dylib's own
//! directory is the one piece of evidence that names the install actually in
//! use, and [`Libtk::library_root`] reads it back off the loaded image.
//!
//! Tcl derives the same fact the same way. `TclpFindExecutable` calls
//! `dladdr` on one of its own symbols and keeps `dli_fname` as the name of the
//! shared library (`unix/tclUnixFile.c:210-219`); `TclpInitLibraryPath` then
//! puts a directory derived from it on the library path, as the step it
//! documents as "look for the library relative to the compiled-in path"
//! (`unix/tclUnixInit.c:488-513`). tclrs has no compiled-in path — it is not
//! installed alongside a Tcl library — so the loaded image is all there is, and
//! it is strictly better evidence than a constant baked in at build time.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
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
    /// Open the Tk dylib named by `TCLRS_LIBTK`, or the first of `CANDIDATES`
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

    /// The directory the loaded image sits in, as the dynamic linker reports
    /// it — which is where the `tk9.0` and `tcl9.0` script libraries of the
    /// same install are.
    ///
    /// `dladdr` on a resolved symbol rather than [`Libtk::path`], because the
    /// two can differ: `path` is the string this process asked for, and
    /// `dli_fname` is the file the linker mapped. When a dylib was already
    /// loaded under another name — a `DYLD_LIBRARY_PATH` override, an install
    /// name that resolves elsewhere — the second is the one whose neighbours
    /// are the right scripts.
    ///
    /// `None` when the platform has no `dladdr` answer for the address, which
    /// is the case a caller falls back from; see the module documentation for
    /// the precedent in `unix/tclUnixFile.c:213-216`.
    pub fn library_root(&self) -> Option<String> {
        let addr = self.sym("Tk_Init").ok()?;
        // SAFETY: `info` is written only when `dladdr` returns non-zero, which
        // is its contract (`dladdr(3)`: "returns zero on error"), and
        // `dli_fname` is then a NUL-terminated path owned by the linker.
        let mut info = std::mem::MaybeUninit::<libc::Dl_info>::uninit();
        let name = unsafe {
            if libc::dladdr(addr, info.as_mut_ptr()) == 0 {
                return None;
            }
            let info = info.assume_init();
            if info.dli_fname.is_null() {
                return None;
            }
            CStr::from_ptr(info.dli_fname)
                .to_string_lossy()
                .into_owned()
        };
        Path::new(&name)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .filter(|p| !p.is_empty())
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

/// Put the script libraries that sit beside the loaded dylib on `interp`'s
/// library path, and return the directories that were added.
///
/// Two variables, and both are read by the port of `tcl_findLibrary` in
/// [`crate::cmd_source`] rather than by anything here:
///
/// * `auto_path` gains the dylib's directory, which is where `tcl_findLibrary`
///   looks for `$basename$version` (`library/auto.tcl:148-155`) — so
///   `tcl_findLibrary tk 9.0 …` reaches `…/lib/tk9.0/tk.tcl` with no
///   `TK_LIBRARY` in the environment. Appended the way `init.tcl` appends,
///   skipping an entry that is already there and keeping the order
///   (`library/init.tcl:63-72`);
/// * `tcl_library` is set to `…/lib/tcl9.0` when that directory holds an
///   `init.tcl`, because `tcl_findLibrary` treats an already-set `varName` as a
///   path the host hardwired and searches nothing else (`library/auto.tcl:64-65`)
///   — which is exactly the claim being made. `TCL_LIBRARY` outranks it, as it
///   outranks the compiled-in path in `TclpInitLibraryPath`
///   (`unix/tclUnixInit.c:440-486`).
///
/// Nothing is overwritten: a host that set either variable itself keeps what it
/// set, and a dylib whose neighbours are not a Tcl install adds nothing, which
/// leaves the executable-relative candidates `tcl_findLibrary` builds for
/// itself as the fallback.
///
/// # Safety
/// `interp_ptr` is a `Tcl_Interp *` this crate handed to Tk.
pub unsafe fn seed_library_path(interp_ptr: *mut c_void, root: &str) -> Vec<String> {
    let host = super::interp::host_of(interp_ptr);
    if host.is_null() {
        return Vec::new();
    }
    let shared = super::interp::shared_for(host);

    let beside = Path::new(root).join("tcl9.0");
    let tcl_library = match std::env::var("TCL_LIBRARY") {
        Ok(dir) if !dir.is_empty() => Some(dir),
        _ => beside
            .join("init.tcl")
            .exists()
            .then(|| beside.to_string_lossy().into_owned()),
    };

    let mut state = shared.lock().expect("interpreter lock");
    let mut candidates: Vec<String> = Vec::new();
    if let Some(dir) = tcl_library {
        let held = state
            .globals
            .get("tcl_library")
            .map(crate::runtime::to_tcl_string)
            .unwrap_or_default();
        if held.is_empty() {
            state.globals.insert(
                "tcl_library".to_string(),
                fusevm::Value::Str(std::sync::Arc::new(dir.clone())),
            );
        }
        candidates.push(dir);
    }
    candidates.push(root.to_string());

    let held = state
        .globals
        .get("auto_path")
        .map(crate::runtime::to_tcl_string)
        .unwrap_or_default();
    let mut entries = crate::list::split(&held).unwrap_or_default();
    let mut added: Vec<String> = Vec::new();
    for dir in candidates {
        if !entries.contains(&dir) {
            entries.push(dir.clone());
            added.push(dir);
        }
    }
    state.globals.insert(
        "auto_path".to_string(),
        fusevm::Value::Str(std::sync::Arc::new(crate::list::join(&entries))),
    );
    added
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
