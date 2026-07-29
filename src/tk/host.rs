//! The stand-in interpreter that Tk is handed, and the slots it has so far.
//!
//! This is not an interpreter. It is the smallest thing that satisfies Tk's
//! demands one at a time, so that the *next* demand becomes visible. Anything
//! implemented here was implemented because a run stopped on it and the Tk
//! source at that call site said what the answer had to be; nothing was added
//! speculatively.
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
    /// The `Tcl_ObjType`s Tk registered (`tk9.0.4/generic/tkObj.c:1223-1232`).
    pub obj_types: Vec<*const TclObjType>,
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
    /// Variables Tk linked to C storage: name and `TCL_LINK_*` type.
    pub linked_vars: Vec<(String, c_int)>,
    /// Exit handlers Tk registered. Recorded, never run.
    pub exit_handlers: Vec<(*mut c_void, *mut c_void)>,
    /// Live objects this side allocated, for the leak/ownership report.
    pub objs_created: u64,
    pub objs_freed: u64,
}

/// The `Tcl_ObjType` marking a value whose internal rep is a list owned by this
/// side. Tk compares `objPtr->typePtr` against the types it registered itself;
/// a pointer that is none of them reads as "not mine", which is the truth.
static HOST_LIST_TYPE: TclObjType = TclObjType {
    name: c"tclrs-host-list".as_ptr(),
    free_internal_rep_proc: ptr::null(),
    dup_internal_rep_proc: ptr::null(),
    update_string_proc: ptr::null(),
    set_from_any_proc: ptr::null(),
    version: 0,
    length_proc: ptr::null(),
    index_proc: ptr::null(),
    slice_proc: ptr::null(),
    reverse_proc: ptr::null(),
    get_elements_proc: ptr::null(),
    set_element_proc: ptr::null(),
    replace_proc: ptr::null(),
    in_oper_proc: ptr::null(),
};

/// One entry of the command table. Boxed, so the address returned as the
/// `Tcl_Command` token stays valid as the table grows.
pub struct HostCommand {
    pub name: String,
    pub proc_: *mut c_void,
    pub client_data: *mut c_void,
    pub delete_proc: *mut c_void,
    /// The subcommand dictionary, for a command created as an ensemble.
    pub ensemble_map: *mut TclObj,
}

/// The list behind a value of [`HOST_LIST_TYPE`], stored in `internalRep.ptr1`.
struct HostList {
    elems: Vec<*mut TclObj>,
}

/// The `Tcl_ObjType` for a value whose internal rep is a dictionary owned by
/// this side. Tcl's own dictionary is a hash with insertion order preserved
/// (`generic/tclDictObj.c`); this keeps the pairs in a vector, which has the
/// same ordering and the same answers at the sizes Tk builds here.
static HOST_DICT_TYPE: TclObjType = TclObjType {
    name: c"tclrs-host-dict".as_ptr(),
    free_internal_rep_proc: ptr::null(),
    dup_internal_rep_proc: ptr::null(),
    update_string_proc: ptr::null(),
    set_from_any_proc: ptr::null(),
    version: 0,
    length_proc: ptr::null(),
    index_proc: ptr::null(),
    slice_proc: ptr::null(),
    reverse_proc: ptr::null(),
    get_elements_proc: ptr::null(),
    set_element_proc: ptr::null(),
    replace_proc: ptr::null(),
    in_oper_proc: ptr::null(),
};

/// The pairs behind a value of [`HOST_DICT_TYPE`].
struct HostDict {
    pairs: Vec<(*mut TclObj, *mut TclObj)>,
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

pub fn build() -> *mut HostInterp {
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
    let tcl_plat = Box::new(TclPlatStubs {
        magic: TCL_STUB_MAGIC,
        hooks: ptr::null(),
        slots: TCL_PLAT_TRAPS,
    });
    let tcl_int_plat = Box::new(TclIntPlatStubs {
        magic: TCL_STUB_MAGIC,
        hooks: ptr::null(),
        slots: TCL_INT_PLAT_TRAPS,
    });

    unsafe { install_impls(&mut tcl, degraded()) };

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
    unsafe { wrap_interp(host) }
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
        obj_types: Vec::new(),
        thread_data: Vec::new(),
        result: ptr::null_mut(),
        commands: Vec::new(),
        vars: Vec::new(),
        assoc_data: Vec::new(),
        namespaces: Vec::new(),
        linked_vars: Vec::new(),
        exit_handlers: Vec::new(),
        objs_created: 0,
        objs_freed: 0,
    }
}

/// Slots that have been given a body, in install order. Reported by the probe
/// so "how many of the table is real" is a measured number.
pub fn implemented() -> Vec<(usize, &'static str)> {
    let mut scratch = TclStubs {
        magic: TCL_STUB_MAGIC,
        hooks: ptr::null(),
        slots: TCL_TRAPS,
    };
    let mut out = Vec::new();
    unsafe {
        for i in install_impls(&mut scratch, degraded()) {
            out.push((i, TCL_NAMES[i]));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Object plumbing
// ---------------------------------------------------------------------------

/// Allocate a `Tcl_Obj` whose string rep is `bytes`.
///
/// `libc::malloc` rather than Rust's allocator because the same memory has to
/// be freeable through `Tcl_Free`, which Tk calls on blocks this side returned
/// (`ckfree` is `Tcl_Free` for anything not built as part of Tcl —
/// `generic/tcl.h:2451-2463`).
unsafe fn new_obj(bytes: &[u8]) -> *mut TclObj {
    let h = host();
    h.objs_created += 1;
    let p = libc::malloc(std::mem::size_of::<TclObj>()) as *mut TclObj;
    assert!(!p.is_null(), "out of memory allocating Tcl_Obj");
    let s = libc::malloc(bytes.len() + 1) as *mut c_char;
    assert!(!s.is_null(), "out of memory allocating string rep");
    ptr::copy_nonoverlapping(bytes.as_ptr() as *const c_char, s, bytes.len());
    *s.add(bytes.len()) = 0;
    ptr::write(
        p,
        TclObj {
            ref_count: 0,
            bytes: s,
            length: bytes.len() as isize,
            type_ptr: ptr::null(),
            internal_rep: TclObjInternalRep {
                ptr1: ptr::null_mut(),
                ptr2: ptr::null_mut(),
            },
        },
    );
    p
}

/// The string rep of `obj` as bytes, regenerating it if a `Tcl_ObjType` has
/// invalidated it.
unsafe fn obj_bytes(obj: *mut TclObj) -> &'static [u8] {
    if (*obj).bytes.is_null() {
        let ty = (*obj).type_ptr;
        assert!(
            !ty.is_null() && !(*ty).update_string_proc.is_null(),
            "value has no string rep and no updateStringProc to make one"
        );
        let f: unsafe extern "C" fn(*mut TclObj) = std::mem::transmute((*ty).update_string_proc);
        f(obj);
        assert!(!(*obj).bytes.is_null(), "updateStringProc left bytes NULL");
    }
    std::slice::from_raw_parts((*obj).bytes as *const u8, (*obj).length as usize)
}

/// Replace `obj`'s string rep with the bytes of `s`.
unsafe fn set_string(obj: *mut TclObj, s: &[u8]) {
    if !(*obj).bytes.is_null() {
        libc::free((*obj).bytes as *mut c_void);
    }
    let p = libc::malloc(s.len() + 1) as *mut c_char;
    assert!(!p.is_null(), "out of memory rebuilding string rep");
    ptr::copy_nonoverlapping(s.as_ptr() as *const c_char, p, s.len());
    *p.add(s.len()) = 0;
    (*obj).bytes = p;
    (*obj).length = s.len() as isize;
}

/// The list behind `obj`, converting its string rep into one if it is not a
/// list yet.
///
/// This exists because Tk relies on it. `Initialize` builds the command that
/// creates the main window as a *string* — `Tcl_NewStringObj("toplevel . -class",
/// TCL_INDEX_NONE)` — and then calls `Tcl_ListObjAppendElement` on it
/// (`tk9.0.4/generic/tkWindow.c:3382-3384`). In Tcl that works because a value
/// carries a string rep and an internal rep at once and converts between them
/// on demand; the conversion here is `crate::list::split`, this crate's own
/// reading of Tcl list syntax.
unsafe fn list_of(obj: *mut TclObj) -> &'static mut HostList {
    if !std::ptr::eq((*obj).type_ptr, &HOST_LIST_TYPE) {
        let text = String::from_utf8_lossy(obj_bytes(obj)).into_owned();
        let words = crate::list::split(&text)
            .unwrap_or_else(|e| panic!("value is not a well formed Tcl list: {e}"));
        let elems = words
            .iter()
            .map(|w| {
                let e = new_obj(w.as_bytes());
                (*e).ref_count += 1;
                e
            })
            .collect();
        free_internal_rep(obj);
        (*obj).type_ptr = &HOST_LIST_TYPE;
        (*obj).internal_rep.ptr1 = Box::into_raw(Box::new(HostList { elems })) as *mut c_void;
    }
    &mut *((*obj).internal_rep.ptr1 as *mut HostList)
}

/// Regenerate the string rep of a list value from its elements.
///
/// Tcl would instead drop the string rep and rebuild it lazily through the
/// type's `updateStringProc`. Doing it eagerly keeps every value in this host
/// carrying a valid string rep at all times, which is what makes
/// `Tcl_GetStringFromObj` a lookup rather than a callback.
unsafe fn sync_list_string(obj: *mut TclObj) {
    let l = &*((*obj).internal_rep.ptr1 as *mut HostList);
    let words: Vec<String> = l
        .elems
        .iter()
        .map(|e| String::from_utf8_lossy(obj_bytes(*e)).into_owned())
        .collect();
    set_string(obj, crate::list::join(&words).as_bytes());
}

/// The dictionary behind `obj`, converting its string rep into one if needed.
///
/// Tk starts a dictionary from `Tcl_NewObj()` — an empty string — and fills it
/// with `Tcl_DictObjPut` (`tk9.0.4/generic/tkUtil.c:1215-1223`), so the same
/// on-demand conversion a list needs applies here.
unsafe fn dict_of(obj: *mut TclObj) -> &'static mut HostDict {
    if !std::ptr::eq((*obj).type_ptr, &HOST_DICT_TYPE) {
        let text = String::from_utf8_lossy(obj_bytes(obj)).into_owned();
        let words = crate::list::split(&text)
            .unwrap_or_else(|e| panic!("value is not a well formed Tcl dictionary: {e}"));
        assert!(
            words.len().is_multiple_of(2),
            "dictionary has an odd number of elements"
        );
        let pairs = words
            .chunks(2)
            .map(|kv| {
                let k = new_obj(kv[0].as_bytes());
                let v = new_obj(kv[1].as_bytes());
                (*k).ref_count += 1;
                (*v).ref_count += 1;
                (k, v)
            })
            .collect();
        free_internal_rep(obj);
        (*obj).type_ptr = &HOST_DICT_TYPE;
        (*obj).internal_rep.ptr1 = Box::into_raw(Box::new(HostDict { pairs })) as *mut c_void;
    }
    &mut *((*obj).internal_rep.ptr1 as *mut HostDict)
}

/// Regenerate the string rep of a dictionary from its pairs.
unsafe fn sync_dict_string(obj: *mut TclObj) {
    let d = &*((*obj).internal_rep.ptr1 as *mut HostDict);
    let mut words: Vec<String> = Vec::with_capacity(d.pairs.len() * 2);
    for (k, v) in &d.pairs {
        words.push(String::from_utf8_lossy(obj_bytes(*k)).into_owned());
        words.push(String::from_utf8_lossy(obj_bytes(*v)).into_owned());
    }
    set_string(obj, crate::list::join(&words).as_bytes());
}

/// Release whatever internal rep `obj` currently holds.
unsafe fn free_internal_rep(obj: *mut TclObj) {
    let ty = (*obj).type_ptr;
    if ty.is_null() {
        return;
    }
    if std::ptr::eq(ty, &HOST_LIST_TYPE) {
        drop(Box::from_raw((*obj).internal_rep.ptr1 as *mut HostList));
    } else if std::ptr::eq(ty, &HOST_DICT_TYPE) {
        drop(Box::from_raw((*obj).internal_rep.ptr1 as *mut HostDict));
    } else if !(*ty).free_internal_rep_proc.is_null() {
        let f: unsafe extern "C" fn(*mut TclObj) =
            std::mem::transmute((*ty).free_internal_rep_proc);
        f(obj);
    }
    (*obj).type_ptr = ptr::null();
    (*obj).internal_rep.ptr1 = ptr::null_mut();
    (*obj).internal_rep.ptr2 = ptr::null_mut();
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
// The implemented slots
// ---------------------------------------------------------------------------

/// Patch every implemented slot into `t`, returning their indices.
///
/// # Safety
/// Each `as *const ()` below erases a signature that must match the header
/// exactly; the comment on each line is the `tclDecls.h` declaration it was
/// written from.
unsafe fn install_impls(t: &mut TclStubs, degraded: bool) -> Vec<usize> {
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
    ];
    if degraded {
        // void (*tcl_AppendStringsToObj)(Tcl_Obj *objPtr, ...) /* 15 */
        slots.push(install(
            t,
            "tcl_AppendStringsToObj",
            append_strings_to_obj as *const (),
        ));
    }
    slots
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
unsafe extern "C" fn append_strings_to_obj(obj: *mut TclObj) {
    entered!("tcl_AppendStringsToObj");
    note(
        "DEGRADED-AppendStringsToObj",
        &String::from_utf8_lossy(obj_bytes(obj)),
    );
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
unsafe extern "C" fn tcl_free_obj(obj: *mut TclObj) {
    entered!("tclFreeObj");
    if obj.is_null() {
        return;
    }
    // The interesting assertion of the whole exercise. Tk reached this slot
    // through the `Tcl_DecrRefCount` macro, which decremented `objPtr->refCount`
    // *in place* before calling here (`generic/tcl.h:2524-2531`). This object's
    // memory came from `libc::malloc` on the Rust side, so a count of zero or
    // below here is direct evidence that Tk's inline refcount arithmetic worked
    // on a value Tcl never allocated. Anything else means the layout is wrong.
    assert!(
        (*obj).ref_count <= 0,
        "TclFreeObj reached with refCount {}; Tk's inline Tcl_DecrRefCount did \
         not land on Tcl_Obj offset 0",
        (*obj).ref_count
    );
    let h = host();
    h.objs_freed += 1;
    free_internal_rep(obj);
    if !(*obj).bytes.is_null() {
        libc::free((*obj).bytes as *mut c_void);
    }
    libc::free(obj as *mut c_void);
}

/// Slot 44.
unsafe extern "C" fn list_obj_append_element(
    _interp: *mut c_void,
    list: *mut TclObj,
    obj: *mut TclObj,
) -> c_int {
    entered!("tcl_ListObjAppendElement");
    let l = list_of(list);
    (*obj).ref_count += 1;
    l.elems.push(obj);
    sync_list_string(list);
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
    let l = list_of(list);
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
    *out = list_of(list).elems.len() as isize;
    TCL_OK
}

/// Slot 53. `objv` may be NULL, in which case `objc` is a capacity hint and the
/// list starts empty (`tk9.0.4/generic/tkWindow.c:3280` passes `2, NULL`).
unsafe extern "C" fn new_list_obj(objc: isize, objv: *const *mut TclObj) -> *mut TclObj {
    entered!("tcl_NewListObj");
    let obj = new_obj(b"");
    let mut elems = Vec::new();
    if !objv.is_null() {
        for i in 0..objc.max(0) {
            let e = *objv.offset(i);
            (*e).ref_count += 1;
            elems.push(e);
        }
    }
    (*obj).type_ptr = &HOST_LIST_TYPE;
    (*obj).internal_rep.ptr1 = Box::into_raw(Box::new(HostList { elems })) as *mut c_void;
    obj
}

/// Slot 55.
unsafe extern "C" fn new_empty_obj() -> *mut TclObj {
    entered!("tcl_NewObj");
    new_obj(b"")
}

/// Slot 56.
unsafe extern "C" fn new_string_obj(bytes: *const c_char, length: isize) -> *mut TclObj {
    entered!("tcl_NewStringObj");
    let b = c_bytes(bytes, length);
    note("NewStringObj", &String::from_utf8_lossy(b));
    new_obj(b)
}

/// Slot 64. Tk truncates the class name to the length `Tcl_UtfToTitle` reported
/// (`tk9.0.4/generic/tkWindow.c:3374`), so only shrinking has to work here.
unsafe extern "C" fn set_obj_length(obj: *mut TclObj, length: isize) {
    entered!("tcl_SetObjLength");
    assert!(
        length <= (*obj).length,
        "growing a value is not implemented; Tk only ever shrank one here"
    );
    (*obj).length = length;
    *(*obj).bytes.offset(length) = 0;
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
    record_command(interp, name, proc_, client_data, delete_proc)
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
    record_command(interp, name, proc_, client_data, delete_proc)
}

/// Add a command to the table and return its token.
unsafe fn record_command(
    interp: *mut c_void,
    name: *const c_char,
    proc_: *mut c_void,
    client_data: *mut c_void,
    delete_proc: *mut c_void,
) -> *mut c_void {
    let text = String::from_utf8_lossy(CStr::from_ptr(name).to_bytes()).into_owned();
    note("CreateObjCommand", &text);
    let h = &mut *(*(interp as *mut HostInterp)).host;
    h.commands.push(Box::new(HostCommand {
        name: text,
        proc_,
        client_data,
        delete_proc,
        ensemble_map: ptr::null_mut(),
    }));
    &mut **h.commands.last_mut().unwrap() as *mut HostCommand as *mut c_void
}

/// Slot 94.
///
/// Tk builds a throwaway interpreter to hold the option database while it
/// parses it, then deletes it (`tk9.0.4/generic/tkOption.c:1496-1499`). There is
/// no placeholder for this: the return value has to be an interpreter Tk can
/// drive through the same table. What it gets is a second host with its own
/// commands, variables and result, sharing the process-wide tables and thread
/// data.
unsafe extern "C" fn create_interp() -> *mut c_void {
    entered!("tcl_CreateInterp");
    let host = Box::into_raw(Box::new(empty_host()));
    wrap_interp(host) as *mut c_void
}

/// Slot 111. Frees the child host. The primary is never deleted, and freeing it
/// would pull the tables out from under Tk.
unsafe extern "C" fn delete_interp(interp: *mut c_void) {
    entered!("tcl_DeleteInterp");
    let h = (*(interp as *mut HostInterp)).host;
    if h != CURRENT.load(Ordering::Relaxed) {
        drop(Box::from_raw(h));
        drop(Box::from_raw(interp as *mut HostInterp));
    }
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

/// Slot 166. Tcl guarantees a value here even when nothing has been set
/// (`generic/tclBasic.c` keeps an always-live `objResultPtr`), so an empty one
/// is created on demand rather than returning NULL.
unsafe extern "C" fn get_obj_result(interp: *mut c_void) -> *mut TclObj {
    entered!("tcl_GetObjResult");
    let h = &mut *(*(interp as *mut HostInterp)).host;
    if h.result.is_null() {
        let obj = new_obj(b"");
        (*obj).ref_count += 1;
        h.result = obj;
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

/// Slot 193.
unsafe extern "C" fn next_hash_entry(s: *mut TclHashSearch) -> *mut TclHashEntry {
    entered!("tcl_NextHashEntry");
    super::hash::next_entry(s)
}

/// Slot 117. Grows out of `staticSpace` into a `Tcl_Alloc` block the same way
/// `generic/tclUtil.c` does, because `Tcl_DStringFree` distinguishes the two
/// cases by comparing `string` against `staticSpace`.
unsafe extern "C" fn dstring_append(
    ds: *mut TclDString,
    bytes: *const c_char,
    length: isize,
) -> *mut c_char {
    entered!("tcl_DStringAppend");
    let add = c_bytes(bytes, length);
    let need = (*ds).length as usize + add.len() + 1;
    if need > (*ds).space_avl as usize {
        let cap = need * 2;
        let fresh = libc::malloc(cap) as *mut c_char;
        assert!(!fresh.is_null(), "out of memory growing Tcl_DString");
        ptr::copy_nonoverlapping((*ds).string, fresh, (*ds).length as usize);
        if (*ds).string != (*ds).static_space.as_mut_ptr() {
            libc::free((*ds).string as *mut c_void);
        }
        (*ds).string = fresh;
        (*ds).space_avl = cap as isize;
    }
    ptr::copy_nonoverlapping(
        add.as_ptr() as *const c_char,
        (*ds).string.offset((*ds).length),
        add.len(),
    );
    (*ds).length += add.len() as isize;
    *(*ds).string.offset((*ds).length) = 0;
    (*ds).string
}

/// Slot 124. Only ever called with 0 here, to reuse a `Tcl_DString`
/// (`tk9.0.4/generic/tkUtil.c:1208`).
unsafe extern "C" fn dstring_set_length(ds: *mut TclDString, length: isize) {
    entered!("tcl_DStringSetLength");
    assert!(
        length <= (*ds).length,
        "growing a Tcl_DString through Tcl_DStringSetLength is not implemented"
    );
    (*ds).length = length;
    *(*ds).string.offset(length) = 0;
}

/// Slot 120.
unsafe extern "C" fn dstring_free(ds: *mut TclDString) {
    entered!("tcl_DStringFree");
    if (*ds).string != (*ds).static_space.as_mut_ptr() {
        libc::free((*ds).string as *mut c_void);
    }
    reset_dstring(ds);
}

/// The body of `Tcl_DStringInit`, without the trace line, so that
/// `Tcl_DStringFree` reusing it does not appear in the log as a call Tk made.
unsafe fn reset_dstring(ds: *mut TclDString) {
    (*ds).string = (*ds).static_space.as_mut_ptr();
    (*ds).length = 0;
    (*ds).space_avl = TCL_DSTRING_STATIC_SIZE as isize;
    (*ds).static_space[0] = 0;
}

/// Slot 122. `generic/tclUtil.c`'s version points `string` at `staticSpace`
/// and zeroes it; `Tcl_DStringValue` reads that field directly
/// (`generic/tcl.h:893`), so it may never be left NULL.
unsafe extern "C" fn dstring_init(ds: *mut TclDString) {
    entered!("tcl_DStringInit");
    reset_dstring(ds);
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
    host().obj_types.push(ty);
}

/// Slot 217.
unsafe extern "C" fn reset_result(interp: *mut c_void) {
    entered!("tcl_ResetResult");
    let h = &mut *(*(interp as *mut HostInterp)).host;
    let old = std::mem::replace(&mut h.result, ptr::null_mut());
    if !old.is_null() {
        (*old).ref_count -= 1;
        if (*old).ref_count <= 0 {
            tcl_free_obj(old);
        }
    }
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
unsafe extern "C" fn set_obj_result(interp: *mut c_void, obj: *mut TclObj) {
    entered!("tcl_SetObjResult");
    note("SetObjResult", &String::from_utf8_lossy(obj_bytes(obj)));
    let h = &mut *(*(interp as *mut HostInterp)).host;
    (*obj).ref_count += 1;
    let old = std::mem::replace(&mut h.result, obj);
    if !old.is_null() {
        (*old).ref_count -= 1;
        if (*old).ref_count <= 0 {
            tcl_free_obj(old);
        }
    }
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
        Some(v) => (*v).bytes,
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
    let obj = new_obj(CStr::from_ptr(value).to_bytes());
    (*obj).ref_count += 1;
    let h = &mut *(*(interp as *mut HostInterp)).host;
    match h.vars.iter_mut().find(|(vn, vi, _)| *vn == n && *vi == i) {
        Some(e) => e.2 = obj,
        None => h.vars.push((n, i, obj)),
    }
    (*obj).bytes
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
    let want = obj_bytes(key).to_vec();
    let d = dict_of(dict);
    (*value).ref_count += 1;
    if let Some(slot) = d.pairs.iter_mut().find(|(k, _)| obj_bytes(*k) == want) {
        let old = std::mem::replace(&mut slot.1, value);
        (*old).ref_count -= 1;
        if (*old).ref_count <= 0 {
            tcl_free_obj(old);
        }
    } else {
        (*key).ref_count += 1;
        d.pairs.push((key, value));
    }
    sync_dict_string(dict);
    TCL_OK
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
    (*dict).ref_count += 1;
    cmd.ensemble_map = dict;
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
    note("FindEnsemble", &String::from_utf8_lossy(obj_bytes(name)));
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
unsafe extern "C" fn get_string_from_obj(obj: *mut TclObj, len: *mut isize) -> *mut c_char {
    entered!("tcl_GetStringFromObj");
    let b = obj_bytes(obj);
    if !len.is_null() {
        *len = b.len() as isize;
    }
    (*obj).bytes
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
    let l = list_of(list);
    *objc = l.elems.len() as isize;
    *objv = l.elems.as_mut_ptr();
    TCL_OK
}
