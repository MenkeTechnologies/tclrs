//! The interpreter that Tk is handed, and the slots it has so far.
//!
//! Everything here was written because a run stopped on it and the Tk source at
//! that call site said what the answer had to be; nothing was added
//! speculatively. That is still the method, and it is what makes the list of
//! implemented slots a measurement rather than a plan.
//!
//! The evaluator is not in this file. A slot that has to *run* a script — the
//! three `Tcl_Eval*` — lives in [`super::eval`], against a
//! [`crate::runtime::Interp`] that [`super::interp`] pairs with each `Host`;
//! this file keeps the data structures those slots operate on. [`Level`] is the
//! seam: [`Level::Probe`] is the table without them, whose stopping point is a
//! pinned measurement, and [`Level::Hosting`] is the table with.
//!
//! Every implementation is installed by slot *name*, looked up in the generated
//! name table, never by a literal index. Indices are the ABI, but a literal
//! index in this file would be a second, unchecked copy of it: get one wrong
//! and Tk silently calls the wrong function. Looking the name up means a typo
//! is a panic at install time.

use std::ffi::{c_char, c_int, c_void, CStr};
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};

use super::abi::*;
use super::generated::*;
use super::trace::{note, record, Table};
use super::{dstring, obj, objtype};

// ---------------------------------------------------------------------------
// Host state
// ---------------------------------------------------------------------------

/// What the interpreter pointer handed to Tk actually points at.
///
/// The first 32 bytes are Tcl's, laid out per [`InterpPrefix`]; everything
/// after is this crate's and Tk never looks at it, because to Tk a
/// `Tcl_Interp *` is opaque and the one piece of code that does look inside —
/// `Tcl_InitStubs` — stops at `stubTable` (offset 24).
#[repr(C)]
pub struct HostInterp {
    pub prefix: InterpPrefix,
    /// Back pointer to the state the slots operate on.
    pub host: *mut Host,
}

/// The single live host per process.
///
/// The slots that take no `Tcl_Interp *` — `Tcl_Alloc`, `Tcl_NewObj`,
/// `Tcl_RegisterObjType`, `Tcl_GetThreadData` — have no other way to reach
/// state. One per process is all Tk needs here and all this phase creates.
static CURRENT: AtomicPtr<Host> = AtomicPtr::new(ptr::null_mut());

/// The primary `Tcl_Interp *`, as [`build`] returned it.
///
/// [`CURRENT`] is the `Host`; this is the 32-byte wrapper Tk was handed, which
/// is what a call *into* Tk has to pass back. A command registered by Tk and
/// invoked from a script this process started itself — rather than from inside
/// a `Tcl_Eval*` — is called against this one; see
/// [`super::interp::current`].
static PRIMARY_INTERP: AtomicPtr<HostInterp> = AtomicPtr::new(ptr::null_mut());

/// The primary `Tcl_Interp *`, or null when no host has been built.
pub fn primary_interp() -> *mut HostInterp {
    PRIMARY_INTERP.load(Ordering::Relaxed)
}

/// The four stub tables, built once per process and never moved.
///
/// Separate from [`Host`] because `Tcl_CreateInterp` makes a second
/// interpreter with its own state but the same tables.
pub struct Tables {
    pub tcl: Box<TclStubs>,
    pub tcl_int: Box<TclIntStubs>,
    pub tcl_plat: Box<TclPlatStubs>,
    pub tcl_int_plat: Box<TclIntPlatStubs>,
    pub hooks: Box<TclStubHooks>,
}

/// The tables, leaked once [`build`] has run.
static TABLES: AtomicPtr<Tables> = AtomicPtr::new(ptr::null_mut());

/// Everything the served slots need to remember.
///
/// Some of it is per interpreter (the result, the commands, the variables) and
/// some is per process (the registered `Tcl_ObjType`s, the thread data). The
/// per-process parts are only ever read off the primary host, which is what
/// [`CURRENT`] points at, so a second interpreter created through
/// `Tcl_CreateInterp` shares them rather than starting a second copy.
pub struct Host {
    /// Blocks handed out by `Tcl_GetThreadData`, keyed by the address of the
    /// caller's `Tcl_ThreadDataKey`.
    pub thread_data: Vec<(usize, *mut c_void)>,
    /// The interpreter result, as `Tcl_SetObjResult` last left it.
    pub result: *mut TclObj,
    /// Commands Tk created, in creation order: name, `Tcl_ObjCmdProc *`,
    /// client data, delete proc. Nothing dispatches them in this phase; the
    /// list is itself a measurement of what a real host would have to host.
    pub commands: Vec<Box<HostCommand>>,
    /// The variable store: `(name, index, value)`, where `index` is the array
    /// element name or empty for a scalar. Flat, with no namespaces, no arrays
    /// beyond the two-part name, and no traces — see [`set_var2`].
    pub vars: Vec<(String, String, *mut TclObj)>,
    /// Interpreter association data: name, delete proc, client data
    /// (`generic/tclBasic.c`'s `assocData`).
    pub assoc_data: Vec<(String, *mut c_void, *mut c_void)>,
    /// Namespaces Tk asked for, by fully qualified name.
    pub namespaces: Vec<(String, *mut TclNamespace)>,
    /// Each namespace's export patterns, keyed by the address of its
    /// [`TclNamespace`]. Tcl keeps them in `Namespace.exportArrayPtr`
    /// (`generic/tclInt.h`), which is past the end of the public
    /// `Tcl_Namespace` this host hands out, so they live here instead.
    pub exports: Vec<(usize, Vec<String>)>,
    /// Variables Tk linked to C storage: name and `TCL_LINK_*` type.
    pub linked_vars: Vec<(String, c_int)>,
    /// Exit handlers Tk registered. Recorded, never run.
    pub exit_handlers: Vec<(*mut c_void, *mut c_void)>,
}

/// One entry of the command table. Boxed, so the address returned as the
/// `Tcl_Command` token stays valid as the table grows.
pub struct HostCommand {
    pub name: String,
    pub proc_: *mut c_void,
    pub client_data: *mut c_void,
    pub delete_proc: *mut c_void,
    /// Whether `proc_` is a `Tcl_ObjCmdProc2` rather than a `Tcl_ObjCmdProc`.
    ///
    /// The two differ in the width of their third argument — `Tcl_Size`, i.e.
    /// `ptrdiff_t`, against `int` (`generic/tcl.h:587-591`, `generic/tcl.h:332`)
    /// — so calling one through the other's type leaves half of `objc`
    /// undefined. Which slot the command arrived through is the only record of
    /// which it is, so it is kept at that moment rather than guessed at the
    /// call.
    pub proc2: bool,
    /// The subcommand dictionary, for a command created as an ensemble.
    pub ensemble_map: *mut TclObj,
}

unsafe fn host() -> &'static mut Host {
    let p = CURRENT.load(Ordering::Relaxed);
    assert!(!p.is_null(), "no host interpreter installed");
    &mut *p
}

// ---------------------------------------------------------------------------
// Table construction
// ---------------------------------------------------------------------------

/// Index of a slot by the name the header gives it. Panics on an unknown name,
/// which is the point: it turns a typo into a build-time-ish failure instead of
/// a call to whatever happens to live at the wrong index.
fn slot(names: &[&str], name: &str) -> usize {
    names
        .iter()
        .position(|n| *n == name)
        .unwrap_or_else(|| panic!("no slot named {name} in this table"))
}

/// Install `f` at the named slot of the primary table.
///
/// # Safety
/// `f` must have exactly the signature `tclDecls.h` gives that slot. The caller
/// states the header line it read it from.
unsafe fn install(t: &mut TclStubs, name: &str, f: *const ()) -> usize {
    let i = slot(&TCL_NAMES, name);
    t.slots[i] = std::mem::transmute::<*const (), RawStub>(f);
    i
}

/// Build the interpreter Tk will be handed: four tables of traps, with the
/// implemented slots patched over them.
///
/// Returns a leaked pointer. The process aborts inside Tk when it reaches an
/// unimplemented slot, so there is no orderly teardown to hook a free onto, and
/// a live table has to outlive every frame Tk might return through.
/// Whether the run is allowed to install slots that are known to be wrong.
///
/// One slot cannot be written correctly in stable Rust at all — see
/// [`append_strings_to_obj`] — so a correct run stops there. Setting
/// `TCLRS_TK_DEGRADED` installs a deliberately wrong body for it instead, which
/// buys a longer call log at the cost of the log describing a Tk that was fed
/// bad data. Off by default, and every degraded slot announces itself.
pub fn degraded() -> bool {
    std::env::var_os("TCLRS_TK_DEGRADED").is_some()
}

/// Which of the two stub tables to build.
///
/// They differ by five slots, and the difference is the whole of phase 2. The
/// distinction is kept rather than collapsed because the two answer different
/// questions and both answers are worth having:
///
/// * [`Level::Probe`] is the *measuring instrument*. Every slot with no body is
///   a trap, so the first thing Tk asks for that this crate cannot supply stops
///   the run and names itself. Its stopping point is a measurement, pinned by
///   `tests/tk_probe_session.rs`, and it must keep stopping where it stopped —
///   otherwise the number in `src/tk/mod.rs` is a claim about a table nobody
///   can rebuild.
/// * [`Level::Hosting`] is the *host*. It adds the evaluator and the C
///   trampoline, so a script Tk hands over is compiled and run rather than
///   trapped on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Probe,
    Hosting,
}

/// The measuring table, and a host with no evaluator behind it.
pub fn build() -> *mut HostInterp {
    build_at(Level::Probe)
}

/// The hosting table: [`build`] plus the evaluator and the trampoline.
///
/// Only one of the two may be built per process — they share [`TABLES`] and
/// [`CURRENT`] — which is why each caller is a separate binary or a separate
/// test process.
pub fn build_hosting() -> *mut HostInterp {
    build_at(Level::Hosting)
}

fn build_at(level: Level) -> *mut HostInterp {
    let mut tcl = Box::new(TclStubs {
        magic: TCL_STUB_MAGIC,
        hooks: ptr::null(),
        slots: TCL_TRAPS,
    });
    let tcl_int = Box::new(TclIntStubs {
        magic: TCL_STUB_MAGIC,
        hooks: ptr::null(),
        slots: TCL_INT_TRAPS,
    });
    let mut tcl_plat = Box::new(TclPlatStubs {
        magic: TCL_STUB_MAGIC,
        hooks: ptr::null(),
        slots: TCL_PLAT_TRAPS,
    });
    // The one platform slot referenced anywhere in libtk. It has to be in place
    // before `Tk_Init` runs, because Tk calls it from `Tk_MacOSXSetupTkNotifier`
    // (`tk9.0.4/macosx/tkMacOSXNotify.c:270-271`) during initialisation.
    unsafe { super::notifier::install_plat(&mut tcl_plat) };
    let tcl_int_plat = Box::new(TclIntPlatStubs {
        magic: TCL_STUB_MAGIC,
        hooks: ptr::null(),
        slots: TCL_INT_PLAT_TRAPS,
    });

    unsafe { install_impls(&mut tcl, degraded(), level) };

    let hooks = Box::new(TclStubHooks {
        tcl_plat_stubs: &*tcl_plat,
        tcl_int_stubs: &*tcl_int,
        tcl_int_plat_stubs: &*tcl_int_plat,
    });
    tcl.hooks = &*hooks;

    let tables = Box::into_raw(Box::new(Tables {
        tcl,
        tcl_int,
        tcl_plat,
        tcl_int_plat,
        hooks,
    }));
    TABLES.store(tables, Ordering::Relaxed);

    let host = Box::into_raw(Box::new(empty_host()));
    CURRENT.store(host, Ordering::Relaxed);
    // `TclInitObjSubsystem` registers Tcl's own eight types before any
    // extension runs (`generic/tclObj.c:370-386`); this side registers its five
    // at the same point, so that `Tcl_GetObjType("double")` answers from the
    // first call rather than only after something has built a double.
    objtype::register_host_types();
    let interp = unsafe { wrap_interp(host) };
    PRIMARY_INTERP.store(interp, Ordering::Relaxed);
    // Registering it here rather than on first use is what makes
    // `tk::dispatch::may_exist` true from the moment a host exists, so a script
    // compiled after `Tk_Init` can name a Tk command and one compiled before
    // any host was built is lowered exactly as it was.
    super::interp::shared_for(host);
    interp
}

/// Wrap a [`Host`] in the 32-byte `Interp` prefix Tk's `Tcl_InitStubs` reads.
///
/// # Safety
/// [`build`] must have run, so that the tables exist.
unsafe fn wrap_interp(host: *mut Host) -> *mut HostInterp {
    Box::into_raw(Box::new(HostInterp {
        prefix: InterpPrefix {
            legacy_result: ptr::null(),
            legacy_free_proc: ptr::null(),
            error_line: 0,
            _pad: 0,
            stub_table: &*(*TABLES.load(Ordering::Relaxed)).tcl,
        },
        host,
    }))
}

/// A `Host` with empty per-interpreter state.
fn empty_host() -> Host {
    Host {
        thread_data: Vec::new(),
        result: ptr::null_mut(),
        commands: Vec::new(),
        vars: Vec::new(),
        assoc_data: Vec::new(),
        namespaces: Vec::new(),
        exports: Vec::new(),
        linked_vars: Vec::new(),
        exit_handlers: Vec::new(),
    }
}

/// Slots that have been given a body, in install order. Reported by the probe
/// so "how many of the table is real" is a measured number.
pub fn implemented() -> Vec<(usize, &'static str)> {
    implemented_at(Level::Probe)
}

/// The same, for whichever table is being built. Deduplicated, because
/// `tcl_AppendStringsToObj` is installed twice at [`Level::Hosting`] under
/// `TCLRS_TK_DEGRADED` — once truncating, once by the trampoline — and a slot
/// counted twice would overstate the coverage.
pub fn implemented_at(level: Level) -> Vec<(usize, &'static str)> {
    let mut scratch = TclStubs {
        magic: TCL_STUB_MAGIC,
        hooks: ptr::null(),
        slots: TCL_TRAPS,
    };
    let mut seen = Vec::new();
    let mut out = Vec::new();
    unsafe {
        for i in install_impls(&mut scratch, degraded(), level) {
            if seen.contains(&i) {
                continue;
            }
            seen.push(i);
            out.push((i, TCL_NAMES[i]));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Object plumbing
// ---------------------------------------------------------------------------

/// A value that came in through a slot and is about to be stored somewhere
/// that outlives the call — a list element, a dictionary value, the
/// interpreter result, a variable.
///
/// Rule 2 of [`obj`]'s ownership discipline: Tk builds `Tcl_Obj`s on its own C
/// stack (`tk9.0.4/macosx/tkMacOSXEmbed.c:160-165`,
/// `tk9.0.4/generic/tkObj.c:201-206`), and retaining one of those would leave a
/// dangling pointer the moment the frame returns. Every retaining slot goes
/// through here first, so that the failure is a named assertion at the point of
/// the mistake rather than a use-after-free later.
///
/// # Safety
/// `obj` must point at a live `Tcl_Obj`.
unsafe fn retain(obj: *mut TclObj) -> *mut TclObj {
    assert!(
        obj::is_host_allocated(obj),
        "a Tcl_Obj at {obj:?} that this side never allocated is being stored \
         past the call that passed it; Tk's stack objects may not be retained"
    );
    obj::incr_ref(obj);
    obj
}

/// `strlen`-terminated or explicitly-sized C string, per Tcl's convention that
/// a negative length means "measure it" (`TCL_INDEX_NONE`, `generic/tcl.h`).
unsafe fn c_bytes(p: *const c_char, len: isize) -> &'static [u8] {
    if p.is_null() {
        return &[];
    }
    if len < 0 {
        CStr::from_ptr(p).to_bytes()
    } else {
        std::slice::from_raw_parts(p as *const u8, len as usize)
    }
}

// ---------------------------------------------------------------------------
// What the evaluator and the dispatcher need from this file
//
// Everything below is a thin, named way in to machinery the slots above
// already use. It is separate from the slots themselves because none of it is
// a call Tk makes: a call from this side must not appear in the trace log, or
// the log stops being a record of what Tk asked for.
// ---------------------------------------------------------------------------

/// The index of a slot by the name `tclDecls.h` gives it. Panics on an unknown
/// name, so a typo in a caller is a failure at the call rather than a jump
/// through the wrong slot.
pub fn slot_index(name: &str) -> usize {
    slot(&TCL_NAMES, name)
}

/// A C string's bytes, with Tcl's convention that a negative length means
/// "measure it" (`TCL_INDEX_NONE`, `generic/tcl.h:2292`).
///
/// # Safety
/// `p` is null or points at `len` readable bytes, or at a NUL-terminated
/// string when `len` is negative.
pub unsafe fn c_bytes_of(p: *const c_char, len: isize) -> &'static [u8] {
    c_bytes(p, len)
}

/// The string rep of `obj`, regenerating it through the type's
/// `updateStringProc` if a `Tcl_ObjType` has invalidated it.
///
/// # Safety
/// `obj` is a live `Tcl_Obj`.
pub unsafe fn obj_bytes_of(obj: *mut TclObj) -> &'static [u8] {
    obj::string_of(obj)
}

/// A fresh value holding `bytes`, with one reference already taken.
///
/// Taking the reference here is what makes it safe to hand to a C command: a
/// command that keeps the value calls `Tcl_IncrRefCount`, which is a direct
/// write to `refCount` (`generic/tcl.h:2517-2519`), and a count of 1 on arrival
/// is what Tcl's own callers give it.
///
/// # Safety
/// A host must exist, since the allocation is counted against it.
pub unsafe fn retained_obj(bytes: &[u8]) -> *mut TclObj {
    let obj = obj::new_string(bytes);
    obj::incr_ref(obj);
    obj
}

/// Give back a reference taken by [`retained_obj`], freeing the value if that
/// was the last one — the `Tcl_DecrRefCount` macro's own behaviour
/// (`generic/tcl.h:2524-2531`).
///
/// # Safety
/// `obj` is a live `Tcl_Obj` this side retained.
pub unsafe fn release_obj(obj: *mut TclObj) {
    obj::release(obj);
}

/// Append `bytes` to `obj`'s string rep, dropping any internal representation
/// first.
///
/// Tcl's `Tcl_AppendToObj` does the same: appending is defined on the string
/// rep, so a value that was a list stops being one (`generic/tclStringObj.c`'s
/// `Tcl_AppendToObj` calls `SetStringFromAny`). Leaving a stale list behind
/// would make the next `Tcl_ListObjGetElements` answer from bytes that are no
/// longer there.
///
/// # Safety
/// `obj` is a live `Tcl_Obj`.
pub unsafe fn append_bytes_to_obj(obj: *mut TclObj, bytes: &[u8]) {
    obj::append_bytes(obj, bytes);
}

/// Make `bytes` the interpreter result.
///
/// The body of `Tcl_SetObjResult` without its trace line, for the same reason
/// `reset_dstring` exists: this is called by the evaluator when a script
/// finishes, and Tk did not ask for it.
///
/// # Safety
/// `interp` is a `Tcl_Interp *` this crate handed to Tk.
pub unsafe fn set_result_bytes(interp: *mut c_void, bytes: &[u8]) {
    install_result(interp, obj::new_string(bytes));
}

/// The interpreter result as bytes, empty when nothing has been set.
///
/// # Safety
/// `interp` is a `Tcl_Interp *` this crate handed to Tk.
pub unsafe fn result_bytes(interp: *mut c_void) -> Vec<u8> {
    let h = &mut *(*(interp as *mut HostInterp)).host;
    if h.result.is_null() {
        return Vec::new();
    }
    obj::string_of(h.result).to_vec()
}

// ---------------------------------------------------------------------------
// The implemented slots
// ---------------------------------------------------------------------------

/// Patch every implemented slot into `t`, returning their indices.
///
/// # Safety
/// Each `as *const ()` below erases a signature that must match the header
/// exactly; the comment on each line is the `tclDecls.h` declaration it was
/// written from.
unsafe fn install_impls(t: &mut TclStubs, degraded: bool, level: Level) -> Vec<usize> {
    let mut slots = vec![
        // const char *(*tcl_PkgRequireEx)(Tcl_Interp *, const char *, const char *, int, void *) /* 1 */
        install(t, "tcl_PkgRequireEx", pkg_require_ex as *const ()),
        // void *(*tcl_Alloc)(TCL_HASH_TYPE size) /* 3 */
        install(t, "tcl_Alloc", tcl_alloc as *const ()),
        // void (*tcl_Free)(void *ptr) /* 4 */
        install(t, "tcl_Free", tcl_free as *const ()),
        // void *(*tcl_Realloc)(void *ptr, TCL_HASH_TYPE size) /* 5 */
        install(t, "tcl_Realloc", tcl_realloc as *const ()),
        // void (*tclFreeObj)(Tcl_Obj *objPtr) /* 30 */
        install(t, "tclFreeObj", tcl_free_obj as *const ()),
        // int (*tcl_ListObjAppendElement)(Tcl_Interp *, Tcl_Obj *, Tcl_Obj *) /* 44 */
        install(
            t,
            "tcl_ListObjAppendElement",
            list_obj_append_element as *const (),
        ),
        // int (*tcl_ListObjIndex)(Tcl_Interp *, Tcl_Obj *, Tcl_Size, Tcl_Obj **) /* 46 */
        install(t, "tcl_ListObjIndex", list_obj_index as *const ()),
        // Tcl_Obj *(*tcl_NewListObj)(Tcl_Size objc, Tcl_Obj *const objv[]) /* 53 */
        install(t, "tcl_NewListObj", new_list_obj as *const ()),
        // Tcl_Obj *(*tcl_NewObj)(void) /* 55 */
        install(t, "tcl_NewObj", new_empty_obj as *const ()),
        // Tcl_Obj *(*tcl_NewStringObj)(const char *bytes, Tcl_Size length) /* 56 */
        install(t, "tcl_NewStringObj", new_string_obj as *const ()),
        // void (*tcl_SetObjLength)(Tcl_Obj *objPtr, Tcl_Size length) /* 64 */
        install(t, "tcl_SetObjLength", set_obj_length as *const ()),
        // Tcl_Command (*tcl_CreateObjCommand)(Tcl_Interp *, const char *,
        //     Tcl_ObjCmdProc *, void *, Tcl_CmdDeleteProc *) /* 96 */
        install(t, "tcl_CreateObjCommand", create_obj_command as *const ()),
        // Tcl_Command (*tcl_CreateObjCommand2)(Tcl_Interp *, const char *,
        //     Tcl_ObjCmdProc2 *, void *, Tcl_CmdDeleteProc *) /* 676 */
        install(t, "tcl_CreateObjCommand2", create_obj_command2 as *const ()),
        // Tcl_Interp *(*tcl_CreateInterp)(void) /* 94 */
        install(t, "tcl_CreateInterp", create_interp as *const ()),
        // void (*tcl_DeleteInterp)(Tcl_Interp *interp) /* 110 */
        install(t, "tcl_DeleteInterp", delete_interp as *const ()),
        // void (*tcl_DeleteHashEntry)(Tcl_HashEntry *entryPtr) /* 108 */
        install(t, "tcl_DeleteHashEntry", delete_hash_entry as *const ()),
        // void (*tcl_DeleteHashTable)(Tcl_HashTable *tablePtr) /* 109 */
        install(t, "tcl_DeleteHashTable", delete_hash_table as *const ()),
        // char *(*tcl_DStringAppend)(Tcl_DString *, const char *, Tcl_Size) /* 117 */
        install(t, "tcl_DStringAppend", dstring_append as *const ()),
        // void (*tcl_DStringSetLength)(Tcl_DString *, Tcl_Size) /* 124 */
        install(t, "tcl_DStringSetLength", dstring_set_length as *const ()),
        // void (*tcl_DStringFree)(Tcl_DString *dsPtr) /* 120 */
        install(t, "tcl_DStringFree", dstring_free as *const ()),
        // void (*tcl_DStringInit)(Tcl_DString *dsPtr) /* 122 */
        install(t, "tcl_DStringInit", dstring_init as *const ()),
        // int (*tcl_GetCommandInfo)(Tcl_Interp *, const char *, Tcl_CmdInfo *) /* 159 */
        install(t, "tcl_GetCommandInfo", get_command_info as *const ()),
        // void *(*tcl_GetAssocData)(Tcl_Interp *, const char *,
        //     Tcl_InterpDeleteProc **) /* 150 */
        install(t, "tcl_GetAssocData", get_assoc_data as *const ()),
        // Tcl_Obj *(*tcl_GetObjResult)(Tcl_Interp *interp) /* 166 */
        install(t, "tcl_GetObjResult", get_obj_result as *const ()),
        // int (*tcl_LinkVar)(Tcl_Interp *, const char *, void *, int) /* 187 */
        install(t, "tcl_LinkVar", link_var as *const ()),
        // Tcl_HashEntry *(*tcl_FirstHashEntry)(Tcl_HashTable *, Tcl_HashSearch *) /* 145 */
        install(t, "tcl_FirstHashEntry", first_hash_entry as *const ()),
        // void (*tcl_InitHashTable)(Tcl_HashTable *tablePtr, int keyType) /* 181 */
        install(t, "tcl_InitHashTable", init_hash_table as *const ()),
        // int (*tcl_IsSafe)(Tcl_Interp *interp) /* 185 */
        install(t, "tcl_IsSafe", is_safe as *const ()),
        // void (*tcl_RegisterObjType)(const Tcl_ObjType *typePtr) /* 211 */
        install(t, "tcl_RegisterObjType", register_obj_type as *const ()),
        // void (*tcl_CreateThreadExitHandler)(Tcl_ExitProc *, void *) /* 288 */
        install(
            t,
            "tcl_CreateThreadExitHandler",
            create_thread_exit_handler as *const (),
        ),
        // void (*tcl_CreateExitHandler)(Tcl_ExitProc *, void *) /* 93 */
        install(t, "tcl_CreateExitHandler", create_exit_handler as *const ()),
        // Tcl_HashEntry *(*tcl_NextHashEntry)(Tcl_HashSearch *searchPtr) /* 193 */
        install(t, "tcl_NextHashEntry", next_hash_entry as *const ()),
        // void (*tcl_ResetResult)(Tcl_Interp *interp) /* 217 */
        install(t, "tcl_ResetResult", reset_result as *const ()),
        // void (*tcl_SetAssocData)(Tcl_Interp *, const char *,
        //     Tcl_InterpDeleteProc *, void *) /* 223 */
        install(t, "tcl_SetAssocData", set_assoc_data as *const ()),
        // const char *(*tcl_SetVar2)(Tcl_Interp *, const char *, const char *,
        //     const char *, int) /* 238 */
        install(t, "tcl_SetVar2", set_var2 as *const ()),
        // void (*tcl_SetErrorCode)(Tcl_Interp *interp, ...) /* 228 */
        install(t, "tcl_SetErrorCode", set_error_code as *const ()),
        // void (*tcl_SetObjResult)(Tcl_Interp *, Tcl_Obj *) /* 235 */
        install(t, "tcl_SetObjResult", set_obj_result as *const ()),
        // void *(*tcl_GetThreadData)(Tcl_ThreadDataKey *keyPtr, Tcl_Size size) /* 305 */
        install(t, "tcl_GetThreadData", get_thread_data as *const ()),
        // const char *(*tcl_GetVar2)(Tcl_Interp *, const char *, const char *, int) /* 176 */
        install(t, "tcl_GetVar2", get_var2 as *const ()),
        // void (*tcl_MutexLock)(Tcl_Mutex *mutexPtr) /* 308 */
        install(t, "tcl_MutexLock", mutex_lock as *const ()),
        // void (*tcl_MutexUnlock)(Tcl_Mutex *mutexPtr) /* 309 */
        install(t, "tcl_MutexUnlock", mutex_unlock as *const ()),
        // Tcl_Obj *(*tcl_GetVar2Ex)(Tcl_Interp *, const char *, const char *, int) /* 306 */
        install(t, "tcl_GetVar2Ex", get_var2_ex as *const ()),
        // Tcl_Size (*tcl_UtfToTitle)(char *src) /* 335 */
        install(t, "tcl_UtfToTitle", utf_to_title as *const ()),
        // void (*tcl_GetTime)(Tcl_Time *timeBuf) /* 482 */
        install(t, "tcl_GetTime", get_time as *const ()),
        // int (*tcl_DictObjPut)(Tcl_Interp *, Tcl_Obj *, Tcl_Obj *, Tcl_Obj *) /* 494 */
        install(t, "tcl_DictObjPut", dict_obj_put as *const ()),
        // Tcl_Namespace *(*tcl_CreateNamespace)(Tcl_Interp *, const char *,
        //     void *, Tcl_NamespaceDeleteProc *) /* 506 */
        install(t, "tcl_CreateNamespace", create_namespace as *const ()),
        // int (*tcl_Export)(Tcl_Interp *, Tcl_Namespace *, const char *, int) /* 509 */
        install(t, "tcl_Export", export as *const ()),
        // Tcl_Command (*tcl_FindCommand)(Tcl_Interp *, const char *,
        //     Tcl_Namespace *, int) /* 515 */
        install(t, "tcl_FindCommand", find_command as *const ()),
        // int (*tcl_Canceled)(Tcl_Interp *, int flags) /* 581 */
        install(t, "tcl_Canceled", canceled as *const ()),
        // Tcl_Obj *(*tcl_GetStartupScript)(const char **encodingPtr) /* 623 */
        install(t, "tcl_GetStartupScript", get_startup_script as *const ()),
        // void (*tcl_SetStartupScript)(Tcl_Obj *path, const char *encoding) /* 622 */
        install(t, "tcl_SetStartupScript", set_startup_script as *const ()),
        // Tcl_Command (*tcl_CreateEnsemble)(Tcl_Interp *, const char *,
        //     Tcl_Namespace *, int) /* 541 */
        install(t, "tcl_CreateEnsemble", create_ensemble as *const ()),
        // Tcl_Command (*tcl_FindEnsemble)(Tcl_Interp *, Tcl_Obj *, int) /* 542 */
        install(t, "tcl_FindEnsemble", find_ensemble as *const ()),
        // int (*tcl_SetEnsembleMappingDict)(Tcl_Interp *, Tcl_Command, Tcl_Obj *) /* 544 */
        install(
            t,
            "tcl_SetEnsembleMappingDict",
            set_ensemble_mapping_dict as *const (),
        ),
        // Tcl_Namespace *(*tcl_FindNamespace)(Tcl_Interp *, const char *,
        //     Tcl_Namespace *, int) /* 514 */
        install(t, "tcl_FindNamespace", find_namespace as *const ()),
        // void (*tcl_RegisterConfig)(Tcl_Interp *, const char *, const Tcl_Config *, const char *) /* 505 */
        install(t, "tcl_RegisterConfig", register_config as *const ()),
        // char *(*tcl_GetStringFromObj)(Tcl_Obj *objPtr, Tcl_Size *lengthPtr) /* 651 */
        install(t, "tcl_GetStringFromObj", get_string_from_obj as *const ()),
        // int (*tcl_ListObjLength)(Tcl_Interp *, Tcl_Obj *, Tcl_Size *) /* 662 */
        install(t, "tcl_ListObjLength", list_obj_length as *const ()),
        // int (*tcl_ListObjGetElements)(Tcl_Interp *, Tcl_Obj *, Tcl_Size *, Tcl_Obj ***) /* 661 */
        install(
            t,
            "tcl_ListObjGetElements",
            list_obj_get_elements as *const (),
        ),
        // --- the object layer (phase 4) ------------------------------------
        // int (*tcl_AppendAllObjTypes)(Tcl_Interp *, Tcl_Obj *) /* 14 */
        install(
            t,
            "tcl_AppendAllObjTypes",
            append_all_obj_types as *const (),
        ),
        // void (*tcl_AppendToObj)(Tcl_Obj *, const char *bytes, Tcl_Size length) /* 16 */
        install(t, "tcl_AppendToObj", append_to_obj as *const ()),
        // int (*tcl_ConvertToType)(Tcl_Interp *, Tcl_Obj *, const Tcl_ObjType *) /* 18 */
        install(t, "tcl_ConvertToType", convert_to_type as *const ()),
        // Tcl_Obj *(*tcl_DuplicateObj)(Tcl_Obj *objPtr) /* 29 */
        install(t, "tcl_DuplicateObj", duplicate_obj as *const ()),
        // int (*tcl_GetBooleanFromObj)(Tcl_Interp *, Tcl_Obj *, int *) /* 32 */
        install(
            t,
            "tcl_GetBooleanFromObj",
            get_boolean_from_obj as *const (),
        ),
        // int (*tcl_GetDoubleFromObj)(Tcl_Interp *, Tcl_Obj *, double *) /* 35 */
        install(t, "tcl_GetDoubleFromObj", get_double_from_obj as *const ()),
        // int (*tcl_GetIntFromObj)(Tcl_Interp *, Tcl_Obj *, int *) /* 38 */
        install(t, "tcl_GetIntFromObj", get_int_from_obj as *const ()),
        // int (*tcl_GetLongFromObj)(Tcl_Interp *, Tcl_Obj *, long *) /* 39 */
        install(t, "tcl_GetLongFromObj", get_long_from_obj as *const ()),
        // const Tcl_ObjType *(*tcl_GetObjType)(const char *typeName) /* 40 */
        install(t, "tcl_GetObjType", get_obj_type as *const ()),
        // void (*tcl_InvalidateStringRep)(Tcl_Obj *objPtr) /* 42 */
        install(
            t,
            "tcl_InvalidateStringRep",
            invalidate_string_rep as *const (),
        ),
        // int (*tcl_ListObjAppendList)(Tcl_Interp *, Tcl_Obj *, Tcl_Obj *) /* 43 */
        install(
            t,
            "tcl_ListObjAppendList",
            list_obj_append_list as *const (),
        ),
        // int (*tcl_ListObjReplace)(Tcl_Interp *, Tcl_Obj *, Tcl_Size, Tcl_Size,
        //     Tcl_Size, Tcl_Obj *const objv[]) /* 48 */
        install(t, "tcl_ListObjReplace", list_obj_replace as *const ()),
        // Tcl_Obj *(*tcl_NewDoubleObj)(double doubleValue) /* 51 */
        install(t, "tcl_NewDoubleObj", new_double_obj as *const ()),
        // void (*tcl_SetStringObj)(Tcl_Obj *, const char *, Tcl_Size) /* 65 */
        install(t, "tcl_SetStringObj", set_string_obj as *const ()),
        // char *(*tcl_DStringAppendElement)(Tcl_DString *, const char *) /* 118 */
        install(
            t,
            "tcl_DStringAppendElement",
            dstring_append_element as *const (),
        ),
        // void (*tcl_DStringEndSublist)(Tcl_DString *dsPtr) /* 119 */
        install(t, "tcl_DStringEndSublist", dstring_end_sublist as *const ()),
        // void (*tcl_DStringGetResult)(Tcl_Interp *, Tcl_DString *) /* 121 */
        install(t, "tcl_DStringGetResult", dstring_get_result as *const ()),
        // void (*tcl_DStringResult)(Tcl_Interp *, Tcl_DString *) /* 123 */
        install(t, "tcl_DStringResult", dstring_result as *const ()),
        // void (*tcl_DStringStartSublist)(Tcl_DString *dsPtr) /* 125 */
        install(
            t,
            "tcl_DStringStartSublist",
            dstring_start_sublist as *const (),
        ),
        // void (*tcl_PrintDouble)(Tcl_Interp *, double value, char *dst) /* 202 */
        install(t, "tcl_PrintDouble", print_double as *const ()),
        // void (*tcl_AppendObjToObj)(Tcl_Obj *, Tcl_Obj *) /* 286 */
        install(t, "tcl_AppendObjToObj", append_obj_to_obj as *const ()),
        // int (*tcl_AttemptSetObjLength)(Tcl_Obj *, Tcl_Size) /* 432 */
        install(
            t,
            "tcl_AttemptSetObjLength",
            attempt_set_obj_length as *const (),
        ),
        // int (*tcl_GetWideIntFromObj)(Tcl_Interp *, Tcl_Obj *, Tcl_WideInt *) /* 487 */
        install(
            t,
            "tcl_GetWideIntFromObj",
            get_wide_int_from_obj as *const (),
        ),
        // Tcl_Obj *(*tcl_NewWideIntObj)(Tcl_WideInt wideValue) /* 488 */
        install(t, "tcl_NewWideIntObj", new_wide_int_obj as *const ()),
        // int (*tcl_DictObjGet)(Tcl_Interp *, Tcl_Obj *, Tcl_Obj *, Tcl_Obj **) /* 495 */
        install(t, "tcl_DictObjGet", dict_obj_get as *const ()),
        // int (*tcl_DictObjRemove)(Tcl_Interp *, Tcl_Obj *, Tcl_Obj *) /* 496 */
        install(t, "tcl_DictObjRemove", dict_obj_remove as *const ()),
        // int (*tcl_DictObjFirst)(Tcl_Interp *, Tcl_Obj *, Tcl_DictSearch *,
        //     Tcl_Obj **, Tcl_Obj **, int *) /* 498 */
        install(t, "tcl_DictObjFirst", dict_obj_first as *const ()),
        // void (*tcl_DictObjNext)(Tcl_DictSearch *, Tcl_Obj **, Tcl_Obj **, int *) /* 499 */
        install(t, "tcl_DictObjNext", dict_obj_next as *const ()),
        // void (*tcl_DictObjDone)(Tcl_DictSearch *searchPtr) /* 500 */
        install(t, "tcl_DictObjDone", dict_obj_done as *const ()),
        // Tcl_Obj *(*tcl_NewDictObj)(void) /* 503 */
        install(t, "tcl_NewDictObj", new_dict_obj as *const ()),
        // void (*tcl_AppendLimitedToObj)(Tcl_Obj *, const char *, Tcl_Size,
        //     Tcl_Size, const char *) /* 575 */
        install(
            t,
            "tcl_AppendLimitedToObj",
            append_limited_to_obj as *const (),
        ),
        // void (*tcl_FreeInternalRep)(Tcl_Obj *objPtr) /* 636 */
        install(t, "tcl_FreeInternalRep", free_internal_rep as *const ()),
        // char *(*tcl_InitStringRep)(Tcl_Obj *, const char *, TCL_HASH_TYPE) /* 637 */
        install(t, "tcl_InitStringRep", init_string_rep as *const ()),
        // Tcl_ObjInternalRep *(*tcl_FetchInternalRep)(Tcl_Obj *, const Tcl_ObjType *) /* 638 */
        install(t, "tcl_FetchInternalRep", fetch_internal_rep as *const ()),
        // void (*tcl_StoreInternalRep)(Tcl_Obj *, const Tcl_ObjType *,
        //     const Tcl_ObjInternalRep *) /* 639 */
        install(t, "tcl_StoreInternalRep", store_internal_rep as *const ()),
        // int (*tcl_HasStringRep)(Tcl_Obj *objPtr) /* 640 */
        install(t, "tcl_HasStringRep", has_string_rep as *const ()),
        // int (*tcl_DictObjSize)(Tcl_Interp *, Tcl_Obj *, Tcl_Size *) /* 663 */
        install(t, "tcl_DictObjSize", dict_obj_size as *const ()),
        // int (*tcl_GetBoolFromObj)(Tcl_Interp *, Tcl_Obj *, int, char *) /* 675 */
        install(t, "tcl_GetBoolFromObj", get_bool_from_obj as *const ()),
        // Tcl_Obj *(*tcl_DStringToObj)(Tcl_DString *dsPtr) /* 685 */
        install(t, "tcl_DStringToObj", dstring_to_obj as *const ()),
    ];
    // The UTF-8 to UTF-16 conversion Cocoa needs, with Tcl's own decoder.
    slots.extend(super::utf16::install_impls(t));
    // The deferred-free table is a module: three slots that share one side
    // table, plus the two errno slots that sit beside them in `tclPreserve.c`'s
    // neighbourhood of the stub table.
    slots.extend(super::preserve::install_impls(t));
    // The package table is a module too: `Tcl_PkgProvideEx` needs Tcl's
    // version normalisation and comparison, which is 250 lines of C.
    slots.extend(super::pkg::install_impls(t));
    // The two index-lookup slots carry a `Tcl_ObjType` of their own, so like
    // the event loop they are a module rather than two more bodies here.
    slots.extend(super::index::install_impls(t));
    // The event loop is a module of its own — twenty-four slots ported from
    // `generic/tclNotify.c`, `generic/tclTimer.c` and `macosx/tclMacOSXNotify.c`
    // — so it installs itself rather than listing its bodies here.
    slots.extend(super::notifier::install_impls(t));
    if degraded {
        // void (*tcl_AppendStringsToObj)(Tcl_Obj *objPtr, ...) /* 15 */
        slots.push(install(
            t,
            "tcl_AppendStringsToObj",
            append_strings_to_obj as *const (),
        ));
    }
    if level == Level::Hosting {
        slots.extend(install_hosting(t));
    }
    slots
}

/// The slots that turn the measuring instrument into a host: the trampoline
/// for the one slot Rust cannot write, and the evaluator.
///
/// Installed last, so that `tcl_AppendStringsToObj` here overwrites the
/// truncating body [`Level::Probe`] may have put there — the two are the same
/// slot and the real one must win.
///
/// # Safety
/// As [`install_impls`]: each erased signature is the one `tclDecls.h` gives
/// the slot, quoted on the line above it.
unsafe fn install_hosting(t: &mut TclStubs) -> Vec<usize> {
    vec![
        // void (*tcl_AppendStringsToObj)(Tcl_Obj *objPtr, ...) /* 15 */
        //
        // The C trampoline, not a Rust body: stable rustc cannot define a
        // C-variadic function at all. See `src/tk/trampoline.c`.
        install(
            t,
            "tcl_AppendStringsToObj",
            super::eval::tclrs_tk_append_strings_to_obj as *const (),
        ),
        // TCL_NORETURN void (*tcl_Panic)(const char *format, ...) /* 2 */
        install(
            t,
            "tcl_Panic",
            super::eval::tclrs_tk_panic_trampoline as *const (),
        ),
        // int (*tcl_EvalEx)(Tcl_Interp *, const char *, Tcl_Size, int) /* 291 */
        install(t, "tcl_EvalEx", super::eval::eval_ex as *const ()),
        // int (*tcl_EvalObjv)(Tcl_Interp *, Tcl_Size, Tcl_Obj *const [], int) /* 292 */
        install(t, "tcl_EvalObjv", super::eval::eval_objv as *const ()),
        // int (*tcl_EvalObjEx)(Tcl_Interp *, Tcl_Obj *, int) /* 293 */
        install(t, "tcl_EvalObjEx", super::eval::eval_obj_ex as *const ()),
        // Tcl_Obj *(*tcl_ObjPrintf)(const char *format, ...) /* 578 */
        //
        // The third trampoline: the formatted text is the returned value, so
        // ignoring the variadic arguments would answer every `wm geometry`
        // with an empty string.
        install(
            t,
            "tcl_ObjPrintf",
            super::eval::tclrs_tk_obj_printf as *const (),
        ),
    ]
}

macro_rules! entered {
    ($name:literal) => {
        record(Table::Tcl, slot(&TCL_NAMES, $name))
    };
}

/// Slot 15, and the one slot in this file that is knowingly wrong.
///
/// `void Tcl_AppendStringsToObj(Tcl_Obj *objPtr, ...)` is variadic and, unlike
/// the other variadic slots Tk calls, the variadic arguments *are* the payload:
/// Tk builds a fully qualified command name out of them
/// (`tk9.0.4/generic/tkUtil.c:1222`). Reading them requires defining a
/// C-variadic function, which stable Rust rejects:
///
/// ```text
/// error[E0658]: C-variadic functions are unstable
///   = note: see issue #44930 for more information
/// ```
///
/// (rustc 1.97.1, `extern "C" fn f(_: *mut c_char, mut args: ...)`.) On AAPCS64
/// there is no way around it from Rust either, because every variadic argument
/// is passed on the stack and a non-variadic declaration cannot name any of
/// them; on SysV x86-64 the first few would land in registers, so a hack there
/// would not port. The real fix is a small C trampoline that walks `va_list`
/// and calls back in with an array, which is phase-2 work.
///
/// This body appends nothing. Everything logged after it describes a Tk that
/// was handed a truncated command name.
unsafe extern "C" fn append_strings_to_obj(o: *mut TclObj) {
    entered!("tcl_AppendStringsToObj");
    note("DEGRADED-AppendStringsToObj", &obj::text_of(o));
}

/// Slot 1. `Tcl_InitStubs` calls this immediately after the magic check and
/// treats NULL as "wrong Tcl" (`generic/tclStubLib.c:101-107`); the string it
/// returns is what `Tcl_InitStubs` hands back to Tk, which only checks it for
/// NULL (`tk9.0.4/generic/tkWindow.c:3218`).
unsafe extern "C" fn pkg_require_ex(
    _interp: *mut c_void,
    _name: *const c_char,
    _version: *const c_char,
    _exact: c_int,
    client_data: *mut *mut c_void,
) -> *const c_char {
    entered!("tcl_PkgRequireEx");
    if !client_data.is_null() {
        *client_data = ptr::null_mut();
    }
    c"9.0.4".as_ptr()
}

/// Slot 3.
unsafe extern "C" fn tcl_alloc(size: usize) -> *mut c_void {
    entered!("tcl_Alloc");
    libc::malloc(size)
}

/// Slot 4.
unsafe extern "C" fn tcl_free(p: *mut c_void) {
    entered!("tcl_Free");
    libc::free(p)
}

/// Slot 5.
unsafe extern "C" fn tcl_realloc(p: *mut c_void, size: usize) -> *mut c_void {
    entered!("tcl_Realloc");
    libc::realloc(p, size)
}

/// Slot 30. Reached from the `Tcl_DecrRefCount` macro once it has already
/// written the count back (`generic/tcl.h:2524-2531`), so by the time this runs
/// the object is unreferenced by definition.
unsafe extern "C" fn tcl_free_obj(o: *mut TclObj) {
    entered!("tclFreeObj");
    obj::free_obj(o);
}

/// Slot 44. The appended value gains a reference and the list's string rep is
/// dropped, so that the next `Tcl_GetString` rebuilds it through the list
/// type's `updateStringProc` — Tcl's own order (`generic/tclListObj.c`).
unsafe extern "C" fn list_obj_append_element(
    _interp: *mut c_void,
    list: *mut TclObj,
    o: *mut TclObj,
) -> c_int {
    entered!("tcl_ListObjAppendElement");
    let l = objtype::list_of(list);
    l.elems.push(retain(o));
    objtype::invalidate(list);
    TCL_OK
}

/// Slot 43. `Tcl_ListObjAppendList`: every element of the second list, appended
/// to the first.
unsafe extern "C" fn list_obj_append_list(
    _interp: *mut c_void,
    list: *mut TclObj,
    from: *mut TclObj,
) -> c_int {
    entered!("tcl_ListObjAppendList");
    let add: Vec<*mut TclObj> = objtype::list_of(from).elems.clone();
    let l = objtype::list_of(list);
    for e in add {
        l.elems.push(retain(e));
    }
    objtype::invalidate(list);
    TCL_OK
}

/// Slot 48. `Tcl_ListObjReplace`: drop `count` elements from `first` and put
/// `objv` there instead. `count` past the end is clamped, which is
/// `generic/tclListObj.c`'s behaviour and what makes "delete to the end" a
/// large count rather than a special case.
unsafe extern "C" fn list_obj_replace(
    _interp: *mut c_void,
    list: *mut TclObj,
    first: isize,
    count: isize,
    objc: isize,
    objv: *const *mut TclObj,
) -> c_int {
    entered!("tcl_ListObjReplace");
    let l = objtype::list_of(list);
    let len = l.elems.len();
    let start = first.clamp(0, len as isize) as usize;
    let end = (start + count.max(0) as usize).min(len);
    let mut fresh = Vec::with_capacity(objc.max(0) as usize);
    if !objv.is_null() {
        for i in 0..objc.max(0) {
            fresh.push(retain(*objv.offset(i)));
        }
    }
    let removed: Vec<*mut TclObj> = l.elems.splice(start..end, fresh).collect();
    for e in removed {
        obj::decr_ref(e);
    }
    objtype::invalidate(list);
    TCL_OK
}

/// Slot 46. An index past the end is not an error: Tcl stores NULL and returns
/// `TCL_OK` (`generic/tclListObj.c`).
unsafe extern "C" fn list_obj_index(
    _interp: *mut c_void,
    list: *mut TclObj,
    index: isize,
    out: *mut *mut TclObj,
) -> c_int {
    entered!("tcl_ListObjIndex");
    let l = objtype::list_of(list);
    *out = if index < 0 || index as usize >= l.elems.len() {
        ptr::null_mut()
    } else {
        l.elems[index as usize]
    };
    TCL_OK
}

/// Slot 662.
unsafe extern "C" fn list_obj_length(
    _interp: *mut c_void,
    list: *mut TclObj,
    out: *mut isize,
) -> c_int {
    entered!("tcl_ListObjLength");
    *out = objtype::list_of(list).elems.len() as isize;
    TCL_OK
}

/// Slot 53. `objv` may be NULL, in which case `objc` is a capacity hint and the
/// list starts empty (`tk9.0.4/generic/tkWindow.c:3280` passes `2, NULL`).
unsafe extern "C" fn new_list_obj(objc: isize, objv: *const *mut TclObj) -> *mut TclObj {
    entered!("tcl_NewListObj");
    let mut elems = Vec::new();
    if !objv.is_null() {
        for i in 0..objc.max(0) {
            let e = *objv.offset(i);
            assert!(
                obj::is_host_allocated(e),
                "Tcl_NewListObj was handed a Tcl_Obj this side never allocated"
            );
            elems.push(e);
        }
    }
    objtype::new_list(&elems)
}

/// Slot 55.
unsafe extern "C" fn new_empty_obj() -> *mut TclObj {
    entered!("tcl_NewObj");
    obj::alloc()
}

/// Slot 56.
unsafe extern "C" fn new_string_obj(bytes: *const c_char, length: isize) -> *mut TclObj {
    entered!("tcl_NewStringObj");
    let b = c_bytes(bytes, length);
    note("NewStringObj", &String::from_utf8_lossy(b));
    obj::new_string(b)
}

/// Slot 488. `Tcl_NewIntObj` and `Tcl_NewBooleanObj` are macros over this one in
/// Tcl 9 (`generic/tcl.h`), so it is the only integer constructor in the table
/// and covers all 284 of Tk's calls to the three names.
unsafe extern "C" fn new_wide_int_obj(v: i64) -> *mut TclObj {
    entered!("tcl_NewWideIntObj");
    objtype::new_wide(v)
}

/// Slot 51.
unsafe extern "C" fn new_double_obj(v: f64) -> *mut TclObj {
    entered!("tcl_NewDoubleObj");
    objtype::new_double(v)
}

/// Slot 503.
unsafe extern "C" fn new_dict_obj() -> *mut TclObj {
    entered!("tcl_NewDictObj");
    objtype::new_dict(&[])
}

/// Slot 29. `Tcl_DuplicateObj` (`generic/tclObj.c:1558-1567`).
unsafe extern "C" fn duplicate_obj(o: *mut TclObj) -> *mut TclObj {
    entered!("tcl_DuplicateObj");
    obj::duplicate(o)
}

/// Slot 64. Tk truncates the class name to the length `Tcl_UtfToTitle` reported
/// (`tk9.0.4/generic/tkWindow.c:3374`); growing is the other half of the same
/// call and is implemented too, in [`obj::set_obj_length`].
unsafe extern "C" fn set_obj_length(o: *mut TclObj, length: isize) {
    entered!("tcl_SetObjLength");
    obj::set_obj_length(o, length);
}

/// Slot 432. The attempting form differs only in reporting failure rather than
/// panicking on it; this side aborts on an allocation failure either way, so it
/// can only return success.
unsafe extern "C" fn attempt_set_obj_length(o: *mut TclObj, length: isize) -> c_int {
    entered!("tcl_AttemptSetObjLength");
    obj::set_obj_length(o, length);
    1
}

/// Slot 65. `Tcl_SetStringObj`: replace both reps.
unsafe extern "C" fn set_string_obj(o: *mut TclObj, bytes: *const c_char, length: isize) {
    entered!("tcl_SetStringObj");
    objtype::free_internal_rep(o);
    obj::set_string(o, c_bytes(bytes, length));
}

/// Slot 16. `Tcl_AppendToObj`.
unsafe extern "C" fn append_to_obj(o: *mut TclObj, bytes: *const c_char, length: isize) {
    entered!("tcl_AppendToObj");
    obj::append_bytes(o, c_bytes(bytes, length));
}

/// Slot 286. `Tcl_AppendObjToObj`.
unsafe extern "C" fn append_obj_to_obj(o: *mut TclObj, from: *mut TclObj) {
    entered!("tcl_AppendObjToObj");
    let add = obj::string_of(from).to_vec();
    obj::append_bytes(o, &add);
}

/// Slot 575. `Tcl_AppendLimitedToObj`: at most `limit` bytes, with `ellipsis`
/// (or Tcl's default `...`) marking a truncation.
unsafe extern "C" fn append_limited_to_obj(
    o: *mut TclObj,
    bytes: *const c_char,
    length: isize,
    limit: isize,
    ellipsis: *const c_char,
) {
    entered!("tcl_AppendLimitedToObj");
    let add = c_bytes(bytes, length);
    if (add.len() as isize) <= limit {
        obj::append_bytes(o, add);
        return;
    }
    let tail = if ellipsis.is_null() {
        b"...".as_slice()
    } else {
        CStr::from_ptr(ellipsis).to_bytes()
    };
    let keep = (limit as usize).saturating_sub(tail.len());
    let mut out = add[..keep.min(add.len())].to_vec();
    out.extend_from_slice(tail);
    obj::append_bytes(o, &out);
}

/// Slot 42. `Tcl_InvalidateStringRep`. Tk calls it on values whose internal rep
/// it has just changed behind this side's back.
unsafe extern "C" fn invalidate_string_rep(o: *mut TclObj) {
    entered!("tcl_InvalidateStringRep");
    obj::invalidate_string_rep(o);
}

/// Slot 640. `Tcl_HasStringRep` (`generic/tclObj.c:1881-1886`).
unsafe extern "C" fn has_string_rep(o: *mut TclObj) -> c_int {
    entered!("tcl_HasStringRep");
    c_int::from(obj::has_string_rep(o))
}

/// Slot 637. `Tcl_InitStringRep` (`generic/tclObj.c:1790-1841`).
unsafe extern "C" fn init_string_rep(
    o: *mut TclObj,
    bytes: *const c_char,
    num: usize,
) -> *mut c_char {
    entered!("tcl_InitStringRep");
    obj::init_string_rep(o, bytes, num)
}

/// Slot 636. `Tcl_FreeInternalRep` (`generic/tclObj.c:1973-1978`).
unsafe extern "C" fn free_internal_rep(o: *mut TclObj) {
    entered!("tcl_FreeInternalRep");
    objtype::free_internal_rep(o);
}

/// Slot 639. `Tcl_StoreInternalRep` (`generic/tclObj.c:1910-1927`).
unsafe extern "C" fn store_internal_rep(
    o: *mut TclObj,
    ty: *const TclObjType,
    ir: *const TclObjInternalRep,
) {
    entered!("tcl_StoreInternalRep");
    objtype::store_internal_rep(o, ty, ir);
}

/// Slot 638. `Tcl_FetchInternalRep` (`generic/tclObj.c:1948-1954`).
unsafe extern "C" fn fetch_internal_rep(
    o: *mut TclObj,
    ty: *const TclObjType,
) -> *mut TclObjInternalRep {
    entered!("tcl_FetchInternalRep");
    objtype::fetch_internal_rep(o, ty)
}

/// Slot 18. `Tcl_ConvertToType` (`generic/tclObj.c:931-957`). Every one of Tk's
/// twelve types has a NULL `setFromAnyProc`, so this returns `TCL_ERROR` for
/// all of them — which is why Tk converts by hand instead.
unsafe extern "C" fn convert_to_type(
    interp: *mut c_void,
    o: *mut TclObj,
    ty: *const TclObjType,
) -> c_int {
    entered!("tcl_ConvertToType");
    objtype::convert_to_type(interp, o, ty)
}

/// Slot 40. `Tcl_GetObjType` (`generic/tclObj.c:895-909`).
unsafe extern "C" fn get_obj_type(name: *const c_char) -> *const TclObjType {
    entered!("tcl_GetObjType");
    if name.is_null() {
        return ptr::null();
    }
    let text = String::from_utf8_lossy(CStr::from_ptr(name).to_bytes()).into_owned();
    note("GetObjType", &text);
    objtype::lookup(&text)
}

/// Slot 14. `Tcl_AppendAllObjTypes` (`generic/tclObj.c:844-876`).
unsafe extern "C" fn append_all_obj_types(_interp: *mut c_void, o: *mut TclObj) -> c_int {
    entered!("tcl_AppendAllObjTypes");
    let list = objtype::list_of(o);
    for name in objtype::registered_names() {
        let e = obj::new_string(name.as_bytes());
        obj::incr_ref(e);
        list.elems.push(e);
    }
    objtype::invalidate(o);
    TCL_OK
}

/// Slot 38. `Tcl_GetIntFromObj`.
unsafe extern "C" fn get_int_from_obj(
    _interp: *mut c_void,
    o: *mut TclObj,
    out: *mut c_int,
) -> c_int {
    entered!("tcl_GetIntFromObj");
    match objtype::wide_of(o) {
        Some(v) if v >= c_int::MIN as i64 && v <= c_int::MAX as i64 => {
            *out = v as c_int;
            TCL_OK
        }
        _ => TCL_ERROR,
    }
}

/// Slot 39. `Tcl_GetLongFromObj`, which is how `TkpScanWindowId` reads a window
/// id — out of a `Tcl_Obj` it built on its own C stack
/// (`tk9.0.4/macosx/tkMacOSXEmbed.c:160-167`). Nothing on this path may touch
/// `refCount` or free `bytes`, and nothing does.
unsafe extern "C" fn get_long_from_obj(
    _interp: *mut c_void,
    o: *mut TclObj,
    out: *mut std::ffi::c_long,
) -> c_int {
    entered!("tcl_GetLongFromObj");
    match objtype::wide_of(o) {
        Some(v) => {
            *out = v as std::ffi::c_long;
            TCL_OK
        }
        None => TCL_ERROR,
    }
}

/// Slot 487. `Tcl_GetWideIntFromObj`.
unsafe extern "C" fn get_wide_int_from_obj(
    _interp: *mut c_void,
    o: *mut TclObj,
    out: *mut i64,
) -> c_int {
    entered!("tcl_GetWideIntFromObj");
    match objtype::wide_of(o) {
        Some(v) => {
            *out = v;
            TCL_OK
        }
        None => TCL_ERROR,
    }
}

/// Slot 35. `Tcl_GetDoubleFromObj` (`generic/tclObj.c:2438-2471`).
///
/// The one slot Tk uses to learn a type pointer rather than a value:
/// `GetTypeCache` calls it on a stack `Tcl_Obj` reading `"0.0"` and keeps
/// whatever `typePtr` it finds afterwards (`tk9.0.4/generic/tkObj.c:198-207`).
/// That object's `refCount` is never initialised, so reading it would be reading
/// uninitialised memory; and `doublePtr` there is `&obj.internalRep.doubleValue`,
/// which aliases the very field the conversion writes. Writing the internal rep
/// first and `*out` from it afterwards is what makes the alias harmless, and is
/// also exactly the order Tcl uses.
unsafe extern "C" fn get_double_from_obj(
    _interp: *mut c_void,
    o: *mut TclObj,
    out: *mut f64,
) -> c_int {
    entered!("tcl_GetDoubleFromObj");
    match objtype::double_of(o) {
        Some(v) => {
            *out = v;
            TCL_OK
        }
        None => TCL_ERROR,
    }
}

/// Slot 32. `Tcl_GetBooleanFromObj`.
unsafe extern "C" fn get_boolean_from_obj(
    _interp: *mut c_void,
    o: *mut TclObj,
    out: *mut c_int,
) -> c_int {
    entered!("tcl_GetBooleanFromObj");
    match objtype::bool_of(o) {
        Some(v) => {
            *out = c_int::from(v);
            TCL_OK
        }
        None => TCL_ERROR,
    }
}

/// Slot 675. `Tcl_GetBoolFromObj`, the Tcl 9 form that writes a `char` and takes
/// flags. The flags select which spellings are accepted; this side accepts them
/// all, which is the `0` case and a superset of the others.
unsafe extern "C" fn get_bool_from_obj(
    _interp: *mut c_void,
    o: *mut TclObj,
    _flags: c_int,
    out: *mut c_char,
) -> c_int {
    entered!("tcl_GetBoolFromObj");
    match objtype::bool_of(o) {
        Some(v) => {
            if !out.is_null() {
                *out = c_char::from(v);
            }
            TCL_OK
        }
        None => TCL_ERROR,
    }
}

/// Slots 96 and 676. `Tcl_CreateObjCommand2` differs only in the signature of
/// the command procedure it takes — `Tcl_ObjCmdProc2` counts arguments with a
/// `Tcl_Size` rather than an `int` — and this phase never invokes either, so
/// one body serves both slots and records which names were claimed.
///
/// The returned `Tcl_Command` is an opaque token; Tk keeps it to pass
/// to `Tcl_DeleteCommandFromToken` and `Tcl_GetCommandName`, so it has to stay
/// valid and distinct. The address of the boxed record is both.
unsafe extern "C" fn create_obj_command(
    interp: *mut c_void,
    name: *const c_char,
    proc_: *mut c_void,
    client_data: *mut c_void,
    delete_proc: *mut c_void,
) -> *mut c_void {
    entered!("tcl_CreateObjCommand");
    record_command(interp, name, proc_, client_data, delete_proc, false)
}

/// Slot 676. Same body, its own trace line: two slots sharing one function
/// would make the call log attribute every `Tcl_CreateObjCommand2` call to slot
/// 96, and the log is the deliverable.
unsafe extern "C" fn create_obj_command2(
    interp: *mut c_void,
    name: *const c_char,
    proc_: *mut c_void,
    client_data: *mut c_void,
    delete_proc: *mut c_void,
) -> *mut c_void {
    entered!("tcl_CreateObjCommand2");
    record_command(interp, name, proc_, client_data, delete_proc, true)
}

/// Add a command to the table and return its token.
///
/// A name that is already taken is replaced rather than appended, which is
/// Tcl's contract: `Tcl_CreateObjCommand` deletes the old command of that name
/// first (`generic/tclBasic.c`'s `Tcl_CreateObjCommand2` calls the old entry's
/// `deleteProc` before installing the new one). Appending instead would leave
/// the *first* registration shadowing every later one, which is the wrong way
/// round.
unsafe fn record_command(
    interp: *mut c_void,
    name: *const c_char,
    proc_: *mut c_void,
    client_data: *mut c_void,
    delete_proc: *mut c_void,
    proc2: bool,
) -> *mut c_void {
    let text = String::from_utf8_lossy(CStr::from_ptr(name).to_bytes()).into_owned();
    note("CreateObjCommand", &text);
    let h = &mut *(*(interp as *mut HostInterp)).host;
    let fresh = HostCommand {
        name: text.clone(),
        proc_,
        client_data,
        delete_proc,
        proc2,
        ensemble_map: ptr::null_mut(),
    };
    match h.commands.iter().position(|c| c.name == text) {
        Some(i) => {
            let old = std::mem::replace(&mut *h.commands[i], fresh);
            run_delete_proc(&old);
            &mut *h.commands[i] as *mut HostCommand as *mut c_void
        }
        None => {
            h.commands.push(Box::new(fresh));
            &mut **h.commands.last_mut().unwrap() as *mut HostCommand as *mut c_void
        }
    }
}

/// Run a command's `Tcl_CmdDeleteProc` — `void (*)(void *clientData)`
/// (`generic/tcl.h:559`) — if it has one. Called where Tcl would call it: when
/// the command is replaced, and when the interpreter holding it is deleted
/// (`generic/tclBasic.c`).
unsafe fn run_delete_proc(cmd: &HostCommand) {
    if cmd.delete_proc.is_null() {
        return;
    }
    let f: unsafe extern "C" fn(*mut c_void) = std::mem::transmute(cmd.delete_proc);
    f(cmd.client_data);
}

/// The command registered under `name` in `host`, or `None`.
///
/// The lifetime is a claim about the table, not about the borrow: a
/// `HostCommand` is boxed, so its address survives the vector growing, and the
/// entry lives until the interpreter is deleted.
///
/// # Safety
/// `host` is null or a `Host` this crate created and has not deleted.
pub unsafe fn command_named(host: *mut Host, name: &str) -> Option<&'static HostCommand> {
    if host.is_null() {
        return None;
    }
    (*host)
        .commands
        .iter()
        .find(|c| c.name == name)
        .map(|c| &*(&**c as *const HostCommand))
}

/// Slot 94.
///
/// Tk builds a throwaway interpreter to hold the option database while it
/// parses it, then deletes it (`tk9.0.4/generic/tkOption.c:1496-1499`). There is
/// no placeholder for this: the return value has to be an interpreter Tk can
/// drive through the same table. What it gets is a second host with its own
/// commands, variables and result, sharing the process-wide tables and thread
/// data.
/// The interpreter this returns is independent in the sense Tk needs: its own
/// commands, its own result, and — since [`super::interp::shared_for`] pairs
/// each `Host` with a [`crate::runtime::Interp`] of its own — its own variables
/// and its own compiled-chunk cache. A `set` in it is invisible to the primary
/// interpreter, which is what a throwaway interpreter for parsing an option
/// file has to mean.
unsafe extern "C" fn create_interp() -> *mut c_void {
    entered!("tcl_CreateInterp");
    let host = Box::into_raw(Box::new(empty_host()));
    super::interp::shared_for(host);
    wrap_interp(host) as *mut c_void
}

/// Slot 110. Frees the child host, its commands and its tclrs interpreter. The
/// primary is never deleted, and freeing it would pull the tables out from
/// under Tk.
unsafe extern "C" fn delete_interp(interp: *mut c_void) {
    entered!("tcl_DeleteInterp");
    let h = (*(interp as *mut HostInterp)).host;
    if h == CURRENT.load(Ordering::Relaxed) {
        return;
    }
    // Tcl runs each command's delete procedure and each association's delete
    // procedure as the interpreter goes away (`generic/tclBasic.c`'s
    // `DeleteInterpProc`). A command whose `clientData` is a Tk allocation
    // would otherwise leak it.
    for cmd in &(*h).commands {
        run_delete_proc(cmd);
    }
    super::interp::forget(h);
    drop(Box::from_raw(h));
    drop(Box::from_raw(interp as *mut HostInterp));
}

/// Slot 108.
unsafe extern "C" fn delete_hash_entry(e: *mut TclHashEntry) {
    entered!("tcl_DeleteHashEntry");
    super::hash::delete_entry(e)
}

/// Slot 109. Frees the entries; the `Tcl_HashTable` is Tk's own memory.
unsafe extern "C" fn delete_hash_table(t: *mut TclHashTable) {
    entered!("tcl_DeleteHashTable");
    super::hash::delete_table(t)
}

/// Slot 150. NULL for an unknown name, and `*procPtr` is only written when the
/// entry exists (`generic/tclBasic.c`).
unsafe extern "C" fn get_assoc_data(
    interp: *mut c_void,
    name: *const c_char,
    proc_out: *mut *mut c_void,
) -> *mut c_void {
    entered!("tcl_GetAssocData");
    let text = String::from_utf8_lossy(CStr::from_ptr(name).to_bytes()).into_owned();
    note("GetAssocData", &text);
    let h = &mut *(*(interp as *mut HostInterp)).host;
    match h.assoc_data.iter().find(|(n, _, _)| *n == text) {
        Some((_, p, d)) => {
            if !proc_out.is_null() {
                *proc_out = *p;
            }
            *d
        }
        None => ptr::null_mut(),
    }
}

/// Slot 223. Replaces an existing entry of the same name.
unsafe extern "C" fn set_assoc_data(
    interp: *mut c_void,
    name: *const c_char,
    proc_: *mut c_void,
    client_data: *mut c_void,
) {
    entered!("tcl_SetAssocData");
    let text = String::from_utf8_lossy(CStr::from_ptr(name).to_bytes()).into_owned();
    note("SetAssocData", &text);
    let h = &mut *(*(interp as *mut HostInterp)).host;
    match h.assoc_data.iter_mut().find(|(n, _, _)| *n == text) {
        Some(e) => {
            e.1 = proc_;
            e.2 = client_data;
        }
        None => h.assoc_data.push((text, proc_, client_data)),
    }
}

/// Slot 159. 0 for an unknown command, and `infoPtr` is left untouched in that
/// case (`generic/tclBasic.c`).
///
/// The `isNativeObjectProc` this reports is 1 for every command, because every
/// command here came in through `Tcl_CreateObjCommand` or its `2` variant and
/// this side does not record which. Tk reads the field
/// (`tk9.0.4/generic/tkWindow.c:962-967`) to decide whether to cache Tcl's own
/// `update` implementation, and 1 sends it down the branch that caches nothing
/// unusual.
unsafe extern "C" fn get_command_info(
    interp: *mut c_void,
    name: *const c_char,
    info: *mut TclCmdInfo,
) -> c_int {
    entered!("tcl_GetCommandInfo");
    let text = String::from_utf8_lossy(CStr::from_ptr(name).to_bytes()).into_owned();
    note("GetCommandInfo", &text);
    let h = &mut *(*(interp as *mut HostInterp)).host;
    let Some(cmd) = h.commands.iter().find(|c| c.name == text) else {
        return 0;
    };
    (*info).is_native_object_proc = 1;
    (*info).obj_proc = cmd.proc_;
    (*info).obj_client_data = cmd.client_data;
    (*info).proc = ptr::null_mut();
    (*info).client_data = ptr::null_mut();
    (*info).delete_proc = cmd.delete_proc;
    (*info).delete_data = cmd.client_data;
    (*info).namespace_ptr = ptr::null_mut();
    (*info).obj_proc2 = ptr::null_mut();
    (*info).obj_client_data2 = ptr::null_mut();
    1
}

/// The startup script `Tcl_SetStartupScript` last recorded, and the encoding
/// name that came with it.
///
/// Tcl keeps both in thread-specific data (`generic/tclMain.c`'s
/// `ThreadSpecificData.path` and `.encoding`); one process-wide pair is the
/// same thing for a host with one interpreter thread.
static STARTUP_SCRIPT: AtomicPtr<TclObj> = AtomicPtr::new(ptr::null_mut());
static STARTUP_ENCODING: AtomicPtr<TclObj> = AtomicPtr::new(ptr::null_mut());

/// Slot 623. `Tcl_Obj *Tcl_GetStartupScript(const char **encodingPtr)`
/// (`generic/tclMain.c:257-273`).
///
/// NULL means "this process was not started with a script", and that is the
/// truthful answer for a host that was not: `Tcl_MainEx` is what sets it, from
/// `argv`, and nothing here runs `Tcl_MainEx`.
///
/// The answer is load-bearing on macOS. `TkpInit` opens a console window when
/// stdin is not a terminal *and* there is no startup script
/// (`tk9.0.4/macosx/tkMacOSXInit.c:585`), so returning NULL under a redirected
/// stdin sends Tk into `Tk_CreateConsoleWindow` and the channel subsystem
/// behind it.
///
/// # Safety
/// `encoding_out` is null or writable.
unsafe extern "C" fn get_startup_script(encoding_out: *mut *const c_char) -> *mut TclObj {
    entered!("tcl_GetStartupScript");
    if !encoding_out.is_null() {
        let enc = STARTUP_ENCODING.load(Ordering::Relaxed);
        *encoding_out = if enc.is_null() {
            ptr::null()
        } else {
            (*enc).bytes
        };
    }
    STARTUP_SCRIPT.load(Ordering::Relaxed)
}

/// Record the file this process was started with, as `Tcl_MainEx` does from
/// `argv` (`generic/tclMain.c:336-338`).
///
/// Written straight into the pair rather than through the slot below, because
/// this is the *host* saying what it was started with and not a call Tk made:
/// going through the slot would put a `tkslot tcl_SetStartupScript` line in a
/// log whose whole value is that every line in it is Tk's.
///
/// The answer matters on macOS. `TkpInit` opens a console window — and with it
/// the channel subsystem — when stdin is not a terminal *and* there is no
/// startup script (`tk9.0.4/macosx/tkMacOSXInit.c:585`), so a session that ran
/// a script file and did not say so would take a branch `wish script.tcl` does
/// not.
pub fn set_startup_file(path: &str) {
    unsafe {
        let kept = retain(obj::new_string(path.as_bytes()));
        obj::release(STARTUP_SCRIPT.swap(kept, Ordering::Relaxed));
    }
}

/// Slot 622. `void Tcl_SetStartupScript(Tcl_Obj *path, const char *encoding)`
/// (`generic/tclMain.c:206-241`): both values are reference-counted by the
/// callee, and the previous pair is released.
///
/// # Safety
/// `path` is null or a live `Tcl_Obj`; `encoding` is null or a NUL-terminated
/// string.
unsafe extern "C" fn set_startup_script(path: *mut TclObj, encoding: *const c_char) {
    entered!("tcl_SetStartupScript");
    let kept = if path.is_null() {
        ptr::null_mut()
    } else {
        retain(path)
    };
    obj::release(STARTUP_SCRIPT.swap(kept, Ordering::Relaxed));
    let enc = if encoding.is_null() {
        ptr::null_mut()
    } else {
        retain(obj::new_string(CStr::from_ptr(encoding).to_bytes()))
    };
    obj::release(STARTUP_ENCODING.swap(enc, Ordering::Relaxed));
}

/// Slot 581. `int Tcl_Canceled(Tcl_Interp *interp, int flags)`
/// (`generic/tclBasic.c:5231-5243`): `TCL_OK` unless the script in progress has
/// been cancelled.
///
/// `Tk_MainLoop` and `update` ask before each pass so that a cancelled script
/// stops running. Nothing in this host cancels anything — `Tcl_CancelEval` is
/// not implemented and the `CANCELED` flag it sets does not exist here — so the
/// answer is the C's own answer for an interpreter with the flag clear
/// (`generic/tclBasic.c:5240-5242`), and will stay correct for as long as that
/// remains true.
unsafe extern "C" fn canceled(_interp: *mut c_void, _flags: c_int) -> c_int {
    entered!("tcl_Canceled");
    TCL_OK
}

/// Slot 515. `Tcl_Command Tcl_FindCommand(Tcl_Interp *, const char *,
/// Tcl_Namespace *, int)` (`generic/tclNamesp.c:2926-2947`): the command token,
/// or NULL when there is no such command.
///
/// Tk asks this only as a yes/no question — "did a script define
/// `::tk::mac::Quit`?" and its siblings
/// (`tk9.0.4/macosx/tkMacOSXHLEvents.c:116,132,149,242,620`,
/// `tk9.0.4/macosx/tkMacOSXMenus.c:180,204,220`) — and the answer decides
/// whether an Apple Event is forwarded to a script or handled by the default.
/// A host with no such command must answer NULL, and does.
///
/// The C's resolution order is: global namespace when the name starts with
/// `::` or `TCL_GLOBAL_ONLY` is set, otherwise the context namespace and then
/// global (`generic/tclNamesp.c:2963-2975`). This host's command table is flat
/// and every name Tk registered into it is the name Tk spelled, so the two
/// spellings of a global command — with and without the leading `::` — are
/// both tried and nothing else is.
unsafe extern "C" fn find_command(
    interp: *mut c_void,
    name: *const c_char,
    _context_ns: *mut TclNamespace,
    _flags: c_int,
) -> *mut c_void {
    entered!("tcl_FindCommand");
    let text = String::from_utf8_lossy(CStr::from_ptr(name).to_bytes()).into_owned();
    note("FindCommand", &text);
    let h = &mut *(*(interp as *mut HostInterp)).host;
    let bare = text.strip_prefix("::").unwrap_or(&text);
    let qualified = format!("::{bare}");
    match h
        .commands
        .iter()
        .find(|c| c.name == text || c.name == bare || c.name == qualified)
    {
        Some(c) => &**c as *const HostCommand as *mut c_void,
        None => ptr::null_mut(),
    }
}

/// Slot 166. Tcl guarantees a value here even when nothing has been set
/// (`generic/tclBasic.c` keeps an always-live `objResultPtr`), so an empty one
/// is created on demand rather than returning NULL.
unsafe extern "C" fn get_obj_result(interp: *mut c_void) -> *mut TclObj {
    entered!("tcl_GetObjResult");
    let h = &mut *(*(interp as *mut HostInterp)).host;
    if h.result.is_null() {
        h.result = retain(obj::alloc());
    }
    h.result
}

/// Slot 187. Tcl would attach a variable trace that keeps the named Tcl
/// variable and the C storage at `addr` in step. Nothing reads either during
/// `Tk_Init` — the five links Tk makes are read later, from scripts
/// (`tk9.0.4/generic/tkWindow.c:900,907`,
/// `tk9.0.4/macosx/tkMacOSXDraw.c:89,99,103`) — so the link is recorded and
/// `TCL_OK` returned. This is a placeholder, and the first script that touches
/// one of those variables is where it stops being adequate.
unsafe extern "C" fn link_var(
    interp: *mut c_void,
    name: *const c_char,
    _addr: *mut c_void,
    ty: c_int,
) -> c_int {
    entered!("tcl_LinkVar");
    let text = String::from_utf8_lossy(CStr::from_ptr(name).to_bytes()).into_owned();
    note("LinkVar", &text);
    let h = &mut *(*(interp as *mut HostInterp)).host;
    h.linked_vars.push((text, ty));
    TCL_OK
}

/// Slot 145.
unsafe extern "C" fn first_hash_entry(
    t: *mut TclHashTable,
    s: *mut TclHashSearch,
) -> *mut TclHashEntry {
    entered!("tcl_FirstHashEntry");
    super::hash::first_entry(t, s)
}

/// Slot 181. Fills in Tk's own memory, including the `findProc` and
/// `createProc` that every later lookup calls directly
/// (`generic/tcl.h:2607-2610`).
unsafe extern "C" fn init_hash_table(t: *mut TclHashTable, key_type: c_int) {
    entered!("tcl_InitHashTable");
    super::hash::init(t, key_type)
}

/// Slot 288. The handlers are kept but never run: this process aborts inside
/// Tk rather than exiting, so there is no point at which Tcl would call them.
unsafe extern "C" fn create_thread_exit_handler(proc_: *mut c_void, client_data: *mut c_void) {
    entered!("tcl_CreateThreadExitHandler");
    host().exit_handlers.push((proc_, client_data));
}

/// Slot 93. `void Tcl_CreateExitHandler(Tcl_ExitProc *, void *)`
/// (`generic/tclEvent.c`), which pushes onto the front of a process-wide list
/// under `exitMutex`.
///
/// Recorded, never run: the C's list is walked by `Tcl_Finalize`, and this host
/// has nothing that calls it. Tk registers one per main window
/// (`tk9.0.4/generic/tkWindow.c`), so the count is a measurement of how many
/// windows a session created.
unsafe extern "C" fn create_exit_handler(proc_: *mut c_void, client_data: *mut c_void) {
    entered!("tcl_CreateExitHandler");
    host().exit_handlers.push((proc_, client_data));
}

/// Slot 193.
unsafe extern "C" fn next_hash_entry(s: *mut TclHashSearch) -> *mut TclHashEntry {
    entered!("tcl_NextHashEntry");
    super::hash::next_entry(s)
}

/// Slot 117.
unsafe extern "C" fn dstring_append(
    ds: *mut TclDString,
    bytes: *const c_char,
    length: isize,
) -> *mut c_char {
    entered!("tcl_DStringAppend");
    dstring::append(ds, bytes, length)
}

/// Slot 118.
unsafe extern "C" fn dstring_append_element(
    ds: *mut TclDString,
    element: *const c_char,
) -> *mut c_char {
    entered!("tcl_DStringAppendElement");
    dstring::append_element(ds, element)
}

/// Slot 125.
unsafe extern "C" fn dstring_start_sublist(ds: *mut TclDString) {
    entered!("tcl_DStringStartSublist");
    dstring::start_sublist(ds);
}

/// Slot 119.
unsafe extern "C" fn dstring_end_sublist(ds: *mut TclDString) {
    entered!("tcl_DStringEndSublist");
    dstring::end_sublist(ds);
}

/// Slot 124.
unsafe extern "C" fn dstring_set_length(ds: *mut TclDString, length: isize) {
    entered!("tcl_DStringSetLength");
    dstring::set_length(ds, length);
}

/// Slot 120.
unsafe extern "C" fn dstring_free(ds: *mut TclDString) {
    entered!("tcl_DStringFree");
    dstring::free(ds);
}

/// Slot 122. `generic/tclUtil.c`'s version points `string` at `staticSpace`
/// and zeroes it; `Tcl_DStringValue` reads that field directly
/// (`generic/tcl.h:893`), so it may never be left NULL.
unsafe extern "C" fn dstring_init(ds: *mut TclDString) {
    entered!("tcl_DStringInit");
    dstring::init(ds);
}

/// `TCL_DOUBLE_SPACE` (`generic/tcl.h:901-902`: `TCL_MAX_PREC + 10`, so 27):
/// the buffer size every `Tcl_PrintDouble` caller promises.
const TCL_DOUBLE_SPACE: usize = 27;

/// Slot 202. `Tcl_PrintDouble` (`generic/tclUtil.c:3116-3122`): a decimal form
/// that always reads back as a float rather than an integer.
///
/// Reached from Tk through `UpdateStringOfMM`
/// (`tk9.0.4/generic/tkObj.c:130`, body at `tkObj.c:~180`), which is the only
/// `updateStringProc` in Tk's twelve object types. `crate::runtime::format_double`
/// is this crate's own answer to the same question and is what a value
/// converted here would stringify to anywhere else in tclrs.
///
/// The output is clamped to `TCL_DOUBLE_SPACE - 1` bytes plus the NUL, because
/// the buffer belongs to the caller and that is all it promised.
unsafe extern "C" fn print_double(_interp: *mut c_void, value: f64, dst: *mut c_char) {
    entered!("tcl_PrintDouble");
    let text = crate::runtime::format_double(value);
    let mut n = text.len().min(TCL_DOUBLE_SPACE - 1);
    while n > 0 && !text.is_char_boundary(n) {
        n -= 1;
    }
    ptr::copy_nonoverlapping(text.as_ptr() as *const c_char, dst, n);
    *dst.add(n) = 0;
}

/// Slot 685. `Tcl_DStringToObj` (`generic/tclUtil.c:3005-3041`).
unsafe extern "C" fn dstring_to_obj(ds: *mut TclDString) -> *mut TclObj {
    entered!("tcl_DStringToObj");
    dstring::to_obj(ds)
}

/// Slot 123. `Tcl_DStringResult` is `Tcl_SetObjResult(interp,
/// Tcl_DStringToObj(dsPtr))` and nothing else (`generic/tclUtil.c:2940-2947`);
/// the two inner calls are made directly rather than through the table so the
/// log shows one call where Tk made one.
unsafe extern "C" fn dstring_result(interp: *mut c_void, ds: *mut TclDString) {
    entered!("tcl_DStringResult");
    let o = dstring::to_obj(ds);
    install_result(interp, o);
}

/// Slot 121. `Tcl_DStringGetResult` (`generic/tclUtil.c:2969-2981`): the
/// interpreter's result into the dstring, and the result reset.
unsafe extern "C" fn dstring_get_result(interp: *mut c_void, ds: *mut TclDString) {
    entered!("tcl_DStringGetResult");
    let h = &mut *(*(interp as *mut HostInterp)).host;
    let text = if h.result.is_null() {
        Vec::new()
    } else {
        obj::string_of(h.result).to_vec()
    };
    dstring::free(ds);
    dstring::append(ds, text.as_ptr() as *const c_char, text.len() as isize);
    clear_result(interp);
}

/// Slot 185. Not a safe interpreter: the safe path in `Initialize` wants a
/// parent interpreter and `::safe::TkInit` in it
/// (`tk9.0.4/generic/tkWindow.c:3242-3304`), neither of which exists here.
unsafe extern "C" fn is_safe(_interp: *mut c_void) -> c_int {
    entered!("tcl_IsSafe");
    0
}

/// Slot 211. Tk registers ten of its own types
/// (`tk9.0.4/generic/tkObj.c:1223-1232`). Keeping the pointers is what lets
/// `tclFreeObj` call the right `freeIntRepProc` later.
unsafe extern "C" fn register_obj_type(ty: *const TclObjType) {
    entered!("tcl_RegisterObjType");
    note("RegisterObjType", &objtype::name_of(ty));
    objtype::register(ty);
    // Off by default. Putting a type through its own procs calls back into Tk
    // and, for `mm`, back through `Tcl_Alloc` and `Tcl_PrintDouble`, so the
    // extra `tkslot` lines would be this side's calls masquerading as Tk's in a
    // log whose whole value is that it is not.
    if std::env::var_os("TCLRS_TK_EXERCISE_TYPES").is_some() {
        let line = objtype::exercise(ty);
        let mut err = std::io::stderr().lock();
        use std::io::Write;
        let _ = writeln!(err, "tkobjtype {line}");
        let _ = err.flush();
    }
}

/// Drop the interpreter's result, releasing this side's reference to it.
unsafe fn clear_result(interp: *mut c_void) {
    let h = &mut *(*(interp as *mut HostInterp)).host;
    let old = std::mem::replace(&mut h.result, ptr::null_mut());
    obj::release(old);
}

/// Make `o` the interpreter's result, taking a reference to it and dropping the
/// one held by the value it replaces.
unsafe fn install_result(interp: *mut c_void, o: *mut TclObj) {
    let kept = retain(o);
    let h = &mut *(*(interp as *mut HostInterp)).host;
    let old = std::mem::replace(&mut h.result, kept);
    obj::release(old);
}

/// Slot 217.
unsafe extern "C" fn reset_result(interp: *mut c_void) {
    entered!("tcl_ResetResult");
    clear_result(interp);
}

/// Slot 228, declared variadic: `void Tcl_SetErrorCode(Tcl_Interp *, ...)`.
///
/// Written with the fixed argument only and none of the variadic ones. That is
/// sound on both calling conventions this targets: the caller lays the variadic
/// arguments out and tears them down, and a callee that never reads them cannot
/// disagree with it about where they were. Under AAPCS64 in particular every
/// variadic argument goes on the stack while the fixed one stays in a register,
/// so reading `interp` is unaffected by ignoring the rest. Rust cannot *define*
/// a variadic function on stable, so this is also the only shape available.
///
/// Tk sets an error code when it is about to fail
/// (`tk9.0.4/generic/tkWindow.c:2795`); the codes are read by scripts, and no
/// script runs here.
unsafe extern "C" fn set_error_code(_interp: *mut c_void) {
    entered!("tcl_SetErrorCode");
}

/// Slot 235.
unsafe extern "C" fn set_obj_result(interp: *mut c_void, o: *mut TclObj) {
    entered!("tcl_SetObjResult");
    note("SetObjResult", &obj::text_of(o));
    install_result(interp, o);
}

/// Slot 305. One zeroed block per key address, kept alive for the process.
/// Tk stores its per-thread window bookkeeping here and expects it zeroed on
/// first use (`tk9.0.4/generic/tkWindow.c:3234` reads `numMainWindows` from
/// it straight away).
unsafe extern "C" fn get_thread_data(key: *mut c_void, size: isize) -> *mut c_void {
    entered!("tcl_GetThreadData");
    let h = host();
    let k = key as usize;
    if let Some((_, p)) = h.thread_data.iter().find(|(a, _)| *a == k) {
        return *p;
    }
    let p = libc::calloc(1, size as usize);
    assert!(!p.is_null(), "out of memory allocating thread data");
    h.thread_data.push((k, p));
    p
}

/// Look up a variable in the flat store.
unsafe fn var_of(
    interp: *mut c_void,
    part1: *const c_char,
    part2: *const c_char,
) -> Option<*mut TclObj> {
    let (n, i) = var_key(part1, part2);
    let h = &mut *(*(interp as *mut HostInterp)).host;
    h.vars
        .iter()
        .find(|(vn, vi, _)| *vn == n && *vi == i)
        .map(|(_, _, v)| *v)
}

/// The `(name, index)` pair a two-part variable reference names.
unsafe fn var_key(part1: *const c_char, part2: *const c_char) -> (String, String) {
    let name = String::from_utf8_lossy(CStr::from_ptr(part1).to_bytes()).into_owned();
    let index = if part2.is_null() {
        String::new()
    } else {
        String::from_utf8_lossy(CStr::from_ptr(part2).to_bytes()).into_owned()
    };
    (name, index)
}

/// Slot 176. NULL for an unset variable — which for `argv0` is what makes
/// `TkpGetAppName` fall back to the literal `"tk"`
/// (`tk9.0.4/macosx/tkMacOSXInit.c:789-791`).
unsafe extern "C" fn get_var2(
    interp: *mut c_void,
    part1: *const c_char,
    part2: *const c_char,
    _flags: c_int,
) -> *const c_char {
    entered!("tcl_GetVar2");
    match var_of(interp, part1, part2) {
        // Through `obj::string_of` and not `(*v).bytes` directly: a value whose
        // string rep was dropped when its internal rep changed has NULL there
        // until the type rebuilds it, and returning that would read as "no such
        // variable" (`tk9.0.4/macosx/tkMacOSXInit.c:789-791` acts on exactly
        // that answer).
        Some(v) => {
            obj::string_of(v);
            (*v).bytes
        }
        None => ptr::null(),
    }
}

/// Slot 238.
///
/// The store behind this is flat: a `(name, index)` pair to a value, with no
/// namespaces, no variable traces, no `upvar` links and no distinction between
/// a scalar and an array. That is enough for the handful of variables Tk sets
/// while initialising and is not enough for anything a script does, which is
/// where this stops being a placeholder and starts being wrong.
///
/// The returned pointer is the value's string rep, which stays alive because
/// the old value is retained rather than freed on overwrite. Tcl has the same
/// aliasing hazard and solves it the same way.
unsafe extern "C" fn set_var2(
    interp: *mut c_void,
    part1: *const c_char,
    part2: *const c_char,
    value: *const c_char,
    _flags: c_int,
) -> *const c_char {
    entered!("tcl_SetVar2");
    let (n, i) = var_key(part1, part2);
    note("SetVar2", &n);
    let o = retain(obj::new_string(CStr::from_ptr(value).to_bytes()));
    let h = &mut *(*(interp as *mut HostInterp)).host;
    match h.vars.iter_mut().find(|(vn, vi, _)| *vn == n && *vi == i) {
        Some(e) => e.2 = o,
        None => h.vars.push((n, i, o)),
    }
    (*o).bytes
}

/// A `Tcl_Mutex` is an opaque `void *` that Tcl fills in on first use
/// (`generic/tclThread.c`), so the caller's slot starts as NULL and the lock
/// itself has to be created lazily behind it. This does the same with a
/// `pthread_mutex_t`.
///
/// # Safety
/// `m` points at a `Tcl_Mutex` the caller owns.
unsafe fn mutex_of(m: *mut *mut c_void) -> *mut libc::pthread_mutex_t {
    if (*m).is_null() {
        let p = libc::malloc(std::mem::size_of::<libc::pthread_mutex_t>())
            as *mut libc::pthread_mutex_t;
        assert!(!p.is_null(), "out of memory allocating a mutex");
        libc::pthread_mutex_init(p, ptr::null());
        *m = p as *mut c_void;
    }
    *m as *mut libc::pthread_mutex_t
}

/// Slot 308.
unsafe extern "C" fn mutex_lock(m: *mut *mut c_void) {
    entered!("tcl_MutexLock");
    libc::pthread_mutex_lock(mutex_of(m));
}

/// Slot 309.
unsafe extern "C" fn mutex_unlock(m: *mut *mut c_void) {
    entered!("tcl_MutexUnlock");
    libc::pthread_mutex_unlock(mutex_of(m));
}

/// Slot 306. NULL means "no such variable", which for `argv` sends `Initialize`
/// straight past its whole argument-parsing block
/// (`tk9.0.4/generic/tkWindow.c:3312-3341`) — the honest answer for an
/// embedding that has not set one.
unsafe extern "C" fn get_var2_ex(
    interp: *mut c_void,
    part1: *const c_char,
    part2: *const c_char,
    _flags: c_int,
) -> *mut TclObj {
    entered!("tcl_GetVar2Ex");
    var_of(interp, part1, part2).unwrap_or(ptr::null_mut())
}

/// Slot 335. Tk title-cases the application name to make the window class
/// (`tk9.0.4/generic/tkWindow.c:3373`). Tcl's own version
/// (`generic/tclUtf.c`) title-cases the first character and lower-cases the
/// rest over the full Unicode tables; this covers ASCII only and leaves other
/// bytes alone, which is enough to produce a class name and no more.
unsafe extern "C" fn utf_to_title(src: *mut c_char) -> isize {
    entered!("tcl_UtfToTitle");
    let mut i = 0isize;
    loop {
        let c = *src.offset(i) as u8;
        if c == 0 {
            break;
        }
        if c.is_ascii() {
            *src.offset(i) = if i == 0 {
                c.to_ascii_uppercase() as c_char
            } else {
                c.to_ascii_lowercase() as c_char
            };
        }
        i += 1;
    }
    i
}

/// Slot 482. Wall clock, the same source `generic/tclUnixTime.c` reads.
unsafe extern "C" fn get_time(t: *mut TclTime) {
    entered!("tcl_GetTime");
    let mut tv = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    libc::gettimeofday(&mut tv, ptr::null_mut());
    (*t).sec = tv.tv_sec;
    (*t).usec = tv.tv_usec as std::ffi::c_long;
}

/// Slot 494. Replaces the value for an existing key, appends otherwise, which
/// is `generic/tclDictObj.c`'s contract.
unsafe extern "C" fn dict_obj_put(
    _interp: *mut c_void,
    dict: *mut TclObj,
    key: *mut TclObj,
    value: *mut TclObj,
) -> c_int {
    entered!("tcl_DictObjPut");
    let want = obj::string_of(key).to_vec();
    let kept = retain(value);
    let d = objtype::dict_of(dict);
    if let Some(slot) = d.pairs.iter_mut().find(|(k, _)| obj::string_of(*k) == want) {
        let old = std::mem::replace(&mut slot.1, kept);
        obj::release(old);
    } else {
        d.pairs.push((retain(key), kept));
    }
    objtype::invalidate(dict);
    TCL_OK
}

/// Slot 495. `Tcl_DictObjGet`: `*out` is NULL for a missing key and the result
/// is still `TCL_OK`, which is how every caller distinguishes "absent" from
/// "not a dictionary" (`generic/tclDictObj.c`).
unsafe extern "C" fn dict_obj_get(
    _interp: *mut c_void,
    dict: *mut TclObj,
    key: *mut TclObj,
    out: *mut *mut TclObj,
) -> c_int {
    entered!("tcl_DictObjGet");
    let want = obj::string_of(key).to_vec();
    let d = objtype::dict_of(dict);
    *out = d
        .pairs
        .iter()
        .find(|(k, _)| obj::string_of(*k) == want)
        .map(|(_, v)| *v)
        .unwrap_or(ptr::null_mut());
    TCL_OK
}

/// Slot 496. `Tcl_DictObjRemove`: a missing key is not an error.
unsafe extern "C" fn dict_obj_remove(
    _interp: *mut c_void,
    dict: *mut TclObj,
    key: *mut TclObj,
) -> c_int {
    entered!("tcl_DictObjRemove");
    let want = obj::string_of(key).to_vec();
    let d = objtype::dict_of(dict);
    if let Some(at) = d.pairs.iter().position(|(k, _)| obj::string_of(*k) == want) {
        let (k, v) = d.pairs.remove(at);
        obj::release(k);
        obj::release(v);
        objtype::invalidate(dict);
    }
    TCL_OK
}

/// Slot 663. `Tcl_DictObjSize`.
unsafe extern "C" fn dict_obj_size(
    _interp: *mut c_void,
    dict: *mut TclObj,
    out: *mut isize,
) -> c_int {
    entered!("tcl_DictObjSize");
    *out = objtype::dict_of(dict).pairs.len() as isize;
    TCL_OK
}

/// Slot 498. `Tcl_DictObjFirst`.
///
/// `Tcl_DictSearch` (`generic/tcl.h:1262-1268`, measured `sizeof` 24 with
/// `next` 0, `epoch` 8, `dictionaryPtr` 16) is caller-allocated, like every
/// other struct Tk declares on its stack. The header says outright that its
/// fields belong to `tclDictObj.c` and no one else, which is what makes it
/// usable as this side's own cursor: `next` holds the index reached so far and
/// `dictionaryPtr` the dictionary being walked.
unsafe extern "C" fn dict_obj_first(
    _interp: *mut c_void,
    dict: *mut TclObj,
    search: *mut obj::TclDictSearch,
    key_out: *mut *mut TclObj,
    value_out: *mut *mut TclObj,
    done: *mut c_int,
) -> c_int {
    entered!("tcl_DictObjFirst");
    // Force the conversion here so that a search over a value that is still a
    // string sees the pairs, not zero of them.
    objtype::dict_of(dict);
    (*search).next = ptr::null_mut();
    (*search).epoch = 1;
    (*search).dictionary_ptr = dict as *mut c_void;
    dict_search_step(search, key_out, value_out, done);
    TCL_OK
}

/// Slot 499. `Tcl_DictObjNext`.
unsafe extern "C" fn dict_obj_next(
    search: *mut obj::TclDictSearch,
    key_out: *mut *mut TclObj,
    value_out: *mut *mut TclObj,
    done: *mut c_int,
) {
    entered!("tcl_DictObjNext");
    dict_search_step(search, key_out, value_out, done);
}

/// Slot 500. `Tcl_DictObjDone`: end a search early. Nothing is allocated per
/// search, so this only has to make a second `Tcl_DictObjNext` safe.
unsafe extern "C" fn dict_obj_done(search: *mut obj::TclDictSearch) {
    entered!("tcl_DictObjDone");
    (*search).epoch = 0;
    (*search).dictionary_ptr = ptr::null_mut();
}

/// One step of a dictionary walk, shared by `Tcl_DictObjFirst` and
/// `Tcl_DictObjNext`.
unsafe fn dict_search_step(
    search: *mut obj::TclDictSearch,
    key_out: *mut *mut TclObj,
    value_out: *mut *mut TclObj,
    done: *mut c_int,
) {
    let dict = (*search).dictionary_ptr as *mut TclObj;
    let at = (*search).next as usize;
    let finished = if (*search).epoch == 0 || dict.is_null() {
        true
    } else {
        let d = objtype::dict_of(dict);
        match d.pairs.get(at) {
            Some((k, v)) => {
                if !key_out.is_null() {
                    *key_out = *k;
                }
                if !value_out.is_null() {
                    *value_out = *v;
                }
                (*search).next = (at + 1) as *mut c_void;
                false
            }
            None => true,
        }
    };
    if finished {
        (*search).epoch = 0;
        if !key_out.is_null() {
            *key_out = ptr::null_mut();
        }
        if !value_out.is_null() {
            *value_out = ptr::null_mut();
        }
    }
    if !done.is_null() {
        *done = c_int::from(finished);
    }
}

/// Slot 506.
///
/// Diverges from Tcl deliberately and visibly: `generic/tclNamesp.c` refuses to
/// create a namespace that already exists, and this returns the existing one
/// instead. Tk asks for `::tk::mac` from two different files
/// (`tk9.0.4/macosx/tkMacOSXDraw.c:85`, `tk9.0.4/macosx/tkMacOSXFont.c:1535`)
/// and treats NULL as "already there, carry on", so both answers get Tk through;
/// returning the record keeps the registry a straight list of what Tk asked for.
unsafe extern "C" fn create_namespace(
    interp: *mut c_void,
    name: *const c_char,
    client_data: *mut c_void,
    delete_proc: *const c_void,
) -> *mut TclNamespace {
    entered!("tcl_CreateNamespace");
    let text = String::from_utf8_lossy(CStr::from_ptr(name).to_bytes()).into_owned();
    note("CreateNamespace", &text);
    let h = &mut *(*(interp as *mut HostInterp)).host;
    if let Some((_, p)) = h.namespaces.iter().find(|(n, _)| *n == text) {
        return *p;
    }
    let tail = text.rsplit("::").next().unwrap_or(&text).to_string();
    let ns = Box::into_raw(Box::new(TclNamespace {
        name: dup_cstring(&tail),
        full_name: dup_cstring(&text),
        client_data,
        delete_proc,
        parent_ptr: ptr::null_mut(),
    }));
    h.namespaces.push((text, ns));
    ns
}

/// Slot 509. `int Tcl_Export(Tcl_Interp *, Tcl_Namespace *, const char *, int)`
/// (`generic/tclNamesp.c:1454-1465`).
///
/// Ttk exports every widget command from `::ttk` on its way up
/// (`tk9.0.4/generic/ttk/ttkInit.c`), which is the only reason this slot is
/// reached during `Tk_Init`.
///
/// Three of the C's four behaviours are here: reset the list first when asked
/// (`generic/tclNamesp.c:1487-1499`), refuse a pattern carrying a namespace
/// qualifier (`generic/tclNamesp.c:1505-1514`), and ignore a pattern already in
/// the list (`generic/tclNamesp.c:1520-1530`). The fourth — growing the array
/// in powers of two from five (`generic/tclNamesp.c:1536-1544`) — is an
/// allocation strategy, not a behaviour, and a `Vec` has its own.
///
/// The qualifier test is written out rather than routed through
/// `TclGetNamespaceForQualName`: that function resolves a qualified name
/// against the namespace tree, and the only thing this call site uses it for is
/// to discover that the pattern contained `::` at all
/// (`generic/tclNamesp.c:1509`).
unsafe extern "C" fn export(
    interp: *mut c_void,
    ns: *mut TclNamespace,
    pattern: *const c_char,
    reset_list_first: c_int,
) -> c_int {
    entered!("tcl_Export");
    let text = String::from_utf8_lossy(CStr::from_ptr(pattern).to_bytes()).into_owned();
    note("Export", &text);
    let h = &mut *(*(interp as *mut HostInterp)).host;
    let key = ns as usize;
    let slot = match h.exports.iter().position(|(k, _)| *k == key) {
        Some(i) => i,
        None => {
            h.exports.push((key, Vec::new()));
            h.exports.len() - 1
        }
    };
    if reset_list_first != 0 {
        h.exports[slot].1.clear();
    }
    if text.contains("::") {
        set_result_bytes(
            interp,
            format!("invalid export pattern \"{text}\": pattern can't specify a namespace")
                .as_bytes(),
        );
        return TCL_ERROR;
    }
    if !h.exports[slot].1.contains(&text) {
        h.exports[slot].1.push(text);
    }
    TCL_OK
}

/// Slot 541. An ensemble is a command whose subcommands are looked up in a
/// dictionary at call time (`generic/tclEnsemble.c`). Nothing dispatches
/// commands in this phase, so the token is a command-table entry like any
/// other and the ensemble behaviour behind it does not exist.
unsafe extern "C" fn create_ensemble(
    interp: *mut c_void,
    name: *const c_char,
    _ns: *mut TclNamespace,
    _flags: c_int,
) -> *mut c_void {
    entered!("tcl_CreateEnsemble");
    let text = String::from_utf8_lossy(CStr::from_ptr(name).to_bytes()).into_owned();
    note("CreateEnsemble", &text);
    let h = &mut *(*(interp as *mut HostInterp)).host;
    h.commands.push(Box::new(HostCommand {
        name: text,
        proc_: ptr::null_mut(),
        client_data: ptr::null_mut(),
        delete_proc: ptr::null_mut(),
        proc2: false,
        ensemble_map: ptr::null_mut(),
    }));
    &mut **h.commands.last_mut().unwrap() as *mut HostCommand as *mut c_void
}

/// Slot 544. Tcl keeps the dictionary and consults it when the ensemble is
/// invoked (`generic/tclEnsemble.c`). Nothing invokes one here, so the mapping
/// is attached to the command record and goes no further.
unsafe extern "C" fn set_ensemble_mapping_dict(
    _interp: *mut c_void,
    token: *mut c_void,
    dict: *mut TclObj,
) -> c_int {
    entered!("tcl_SetEnsembleMappingDict");
    let cmd = &mut *(token as *mut HostCommand);
    cmd.ensemble_map = retain(dict);
    TCL_OK
}

/// Slot 542. NULL means "no such ensemble yet", which is what sends Tk on to
/// `Tcl_CreateEnsemble` (`tk9.0.4/generic/tkUtil.c:1198-1206`).
unsafe extern "C" fn find_ensemble(
    _interp: *mut c_void,
    name: *mut TclObj,
    _flags: c_int,
) -> *mut c_void {
    entered!("tcl_FindEnsemble");
    note("FindEnsemble", &obj::text_of(name));
    ptr::null_mut()
}

/// Slot 514. NULL for an unknown namespace, which is how Tk decides to create
/// one (`tk9.0.4/generic/tkUtil.c:1189-1191`).
unsafe extern "C" fn find_namespace(
    interp: *mut c_void,
    name: *const c_char,
    _context: *mut TclNamespace,
    _flags: c_int,
) -> *mut TclNamespace {
    entered!("tcl_FindNamespace");
    let text = String::from_utf8_lossy(CStr::from_ptr(name).to_bytes()).into_owned();
    note("FindNamespace", &text);
    let h = &mut *(*(interp as *mut HostInterp)).host;
    h.namespaces
        .iter()
        .find(|(n, _)| *n == text)
        .map(|(_, p)| *p)
        .unwrap_or(ptr::null_mut())
}

/// A NUL-terminated copy of `s` in `malloc` storage, so it can be handed to C
/// and freed with `Tcl_Free`.
unsafe fn dup_cstring(s: &str) -> *mut c_char {
    let p = libc::malloc(s.len() + 1) as *mut c_char;
    assert!(!p.is_null(), "out of memory copying a string");
    ptr::copy_nonoverlapping(s.as_ptr() as *const c_char, p, s.len());
    *p.add(s.len()) = 0;
    p
}

/// Slot 505. Tcl's version stores the table under a namespace variable and
/// makes a `::tk::pkgconfig` command out of it (`generic/tclPkgConfig.c`).
/// Nothing in `Tk_Init` reads it back, so keeping the pointer would serve no
/// one; this records the call and drops it.
unsafe extern "C" fn register_config(
    _interp: *mut c_void,
    _pkg: *const c_char,
    _cfg: *const c_void,
    _enc: *const c_char,
) {
    entered!("tcl_RegisterConfig");
}

/// Slot 651. `Tcl_GetString` is a macro over this with a NULL length
/// (`generic/tclDecls.h:4034`), so this one slot serves both.
unsafe extern "C" fn get_string_from_obj(o: *mut TclObj, len: *mut isize) -> *mut c_char {
    entered!("tcl_GetStringFromObj");
    let b = obj::string_of(o);
    if !len.is_null() {
        *len = b.len() as isize;
    }
    (*o).bytes
}

/// Slot 661. The array handed back has to stay valid while Tk walks it, so it
/// is the list's own storage rather than a copy.
unsafe extern "C" fn list_obj_get_elements(
    _interp: *mut c_void,
    list: *mut TclObj,
    objc: *mut isize,
    objv: *mut *mut *mut TclObj,
) -> c_int {
    entered!("tcl_ListObjGetElements");
    let l = objtype::list_of(list);
    *objc = l.elems.len() as isize;
    *objv = l.elems.as_mut_ptr();
    TCL_OK
}
