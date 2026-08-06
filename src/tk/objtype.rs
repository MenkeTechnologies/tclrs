//! `Tcl_ObjType`: the registry, the four procs, and the host's own types.
//!
//! A `Tcl_ObjType` (`generic/tcl.h:657-698`) is a name and four function
//! pointers, plus Tcl 9's abstract-list block. What makes it interesting here is
//! that Tk does not reach any of it through the stub table. It reads
//! `objPtr->typePtr`, compares it against its own types by *pointer*, and calls
//! the procs out of the struct directly.
//!
//! # What Tk's twelve types actually look like
//!
//! `TkRegisterObjTypes` registers ten (`tk9.0.4/generic/tkObj.c:1223-1232`):
//! `border` (`tk3d.c:49`), `bitmap` (`tkBitmap.c:128`), `color`
//! (`tkColor.c:59`), `cursor` (`tkCursor.c:62`), `font` (`tkFont.c:356`), `mm`
//! (`tkObj.c:126`), `pixel` (`tkObj.c:103`), `statekey` (`tkUtil.c:26`),
//! `window` (`tkObj.c:141`) and `textindex` (`tkTextIndex.c:78`). Two more are
//! file-static and never registered at all, yet are still written into
//! `objPtr->typePtr`: `option` (`tkConfig.c:154`, installed at
//! `tkConfig.c:1274`) and `style` (`tkStyle.c:153`, installed at
//! `tkStyle.c:1456`).
//!
//! Reading those twelve definitions changes what a host has to provide:
//!
//! * **Not one of them defines a `setFromAnyProc`.** `Tcl_ConvertToType` to any
//!   Tk type is therefore an error by construction (`generic/tclObj.c:947-954`).
//!   Tk converts by hand instead: `InitBorderObj` calls `Tcl_GetString`, then
//!   the *current* type's `freeIntRepProc`, then assigns `typePtr` and
//!   `internalRep` itself (`tk9.0.4/generic/tk3d.c:1331-1348`), and eleven other
//!   places do the same (`tkBitmap.c:986`, `tkColor.c:751`, `tkCursor.c:777`,
//!   `tkFont.c:1408`, `tkObj.c:590`, `tkObj.c:872`, `tkObj.c:985`,
//!   `tkStyle.c:1448`, `tkUtil.c:1059`, `tkConfig.c:1269`, `tkTextIndex.c:242`,
//!   `macosx/tkMacOSXEmbed.c:172`).
//!
//! * **Only one of the twelve defines an `updateStringProc`** — `mm`
//!   (`tkObj.c:130`). The other eleven rely on the rule that a type without one
//!   must never let `bytes` become NULL (`generic/tclObj.c:1723-1731`), which is
//!   why every one of those conversion sites calls `Tcl_GetString` *before*
//!   changing the type.
//!
//! * **Ten of the twelve define a `freeIntRepProc`**, and Tk calls it directly
//!   on whatever type the value already had. So a host type whose internal rep
//!   owns memory and has no `freeIntRepProc` does not leak in some rare path —
//!   it leaks every time Tk shimmers one of this side's values to one of its
//!   own. That is the single hardest constraint in this file, and it is why the
//!   host list and dictionary types below carry a full set of procs rather than
//!   the NULLs a first sketch would put there.
//!
//! # The host's own types
//!
//! Named after Tcl's own (`generic/tclObj.c:227-250`,
//! `generic/tclListObj.c:152`, `generic/tclDictObj.c`, `generic/tclStringObj.c:78`)
//! because names are how `Tcl_GetObjType` answers and Tk asks exactly once, in
//! `GetTypeCache` (`tk9.0.4/generic/tkObj.c:192-209`); everywhere else it
//! compares pointers, which no name can affect.

use std::ffi::{c_int, c_void, CStr};
use std::ptr;
use std::sync::Mutex;

use super::abi::{TclObj, TclObjInternalRep, TclObjType, TCL_ERROR, TCL_OK};
use super::obj;

/// `TCL_OBJTYPE_V0` (`generic/tcl.h:701-702`): version 0 and no abstract-list
/// procs. Every host type below is V0, as ten of Tk's twelve are.
const OBJTYPE_V0: usize = 0;

/// The list behind a value of [`LIST_TYPE`], reached through
/// `internalRep.twoPtrValue.ptr1`.
///
/// Each element is a counted reference: the list holds one, and drops it in
/// [`free_list_rep`]. The `Vec` may reallocate, which is why the elements are
/// `*mut TclObj` and not owned values — the objects themselves never move
/// (`obj`'s rule 1), only the array of pointers to them does.
pub struct HostList {
    pub elems: Vec<*mut TclObj>,
}

/// The pairs behind a value of [`DICT_TYPE`].
///
/// Tcl's dictionary is a hash that preserves insertion order
/// (`generic/tclDictObj.c`); a vector has the same ordering and the same
/// answers at the sizes Tk builds during initialisation, which is one entry per
/// `Tcl_DictObjPut` at `tk9.0.4/generic/tkUtil.c:1215-1223`.
pub struct HostDict {
    pub pairs: Vec<(*mut TclObj, *mut TclObj)>,
}

// ---------------------------------------------------------------------------
// The type tables
// ---------------------------------------------------------------------------

/// `list` — the shape of `tclListType` (`generic/tclListObj.c:152-159`): all
/// four procs present.
pub static LIST_TYPE: TclObjType = TclObjType {
    name: c"list".as_ptr(),
    free_internal_rep_proc: free_list_rep as *const c_void,
    dup_internal_rep_proc: dup_list_rep as *const c_void,
    update_string_proc: update_string_of_list as *const c_void,
    set_from_any_proc: set_list_from_any as *const c_void,
    version: OBJTYPE_V0,
    length_proc: ptr::null(),
    index_proc: ptr::null(),
    slice_proc: ptr::null(),
    reverse_proc: ptr::null(),
    get_elements_proc: ptr::null(),
    set_element_proc: ptr::null(),
    replace_proc: ptr::null(),
    in_oper_proc: ptr::null(),
};

/// `dict` — the shape of `tclDictType` (`generic/tclDictObj.c`).
pub static DICT_TYPE: TclObjType = TclObjType {
    name: c"dict".as_ptr(),
    free_internal_rep_proc: free_dict_rep as *const c_void,
    dup_internal_rep_proc: dup_dict_rep as *const c_void,
    update_string_proc: update_string_of_dict as *const c_void,
    set_from_any_proc: set_dict_from_any as *const c_void,
    version: OBJTYPE_V0,
    length_proc: ptr::null(),
    index_proc: ptr::null(),
    slice_proc: ptr::null(),
    reverse_proc: ptr::null(),
    get_elements_proc: ptr::null(),
    set_element_proc: ptr::null(),
    replace_proc: ptr::null(),
    in_oper_proc: ptr::null(),
};

/// `int` — `tclIntType` (`generic/tclObj.c:243-250`): no free, no dup, because
/// the value is the internal rep. The bitwise copy `Tcl_DuplicateObj` falls back
/// to when `dupIntRepProc` is NULL (`generic/tclObj.c:1551-1554`) is exactly
/// right for a rep that owns nothing, and that is why Tcl leaves it NULL.
pub static WIDE_TYPE: TclObjType = TclObjType {
    name: c"int".as_ptr(),
    free_internal_rep_proc: ptr::null(),
    dup_internal_rep_proc: ptr::null(),
    update_string_proc: update_string_of_wide as *const c_void,
    set_from_any_proc: set_wide_from_any as *const c_void,
    version: OBJTYPE_V0,
    length_proc: ptr::null(),
    index_proc: ptr::null(),
    slice_proc: ptr::null(),
    reverse_proc: ptr::null(),
    get_elements_proc: ptr::null(),
    set_element_proc: ptr::null(),
    replace_proc: ptr::null(),
    in_oper_proc: ptr::null(),
};

/// `double` — `tclDoubleType` (`generic/tclObj.c:235-242`).
///
/// This is the one host type Tk looks for by identity rather than by name:
/// `GetTypeCache` builds a stack `Tcl_Obj` reading `"0.0"`, calls
/// `Tcl_GetDoubleFromObj` on it and keeps whatever `typePtr` came back
/// (`tk9.0.4/generic/tkObj.c:198-207`), then compares later values against it
/// (`tkObj.c:525`, `tkObj.c:805`). A `Tcl_GetDoubleFromObj` that left `typePtr`
/// alone would make Tk cache NULL and treat every untyped value as a double.
pub static DOUBLE_TYPE: TclObjType = TclObjType {
    name: c"double".as_ptr(),
    free_internal_rep_proc: ptr::null(),
    dup_internal_rep_proc: ptr::null(),
    update_string_proc: update_string_of_double as *const c_void,
    set_from_any_proc: set_double_from_any as *const c_void,
    version: OBJTYPE_V0,
    length_proc: ptr::null(),
    index_proc: ptr::null(),
    slice_proc: ptr::null(),
    reverse_proc: ptr::null(),
    get_elements_proc: ptr::null(),
    set_element_proc: ptr::null(),
    replace_proc: ptr::null(),
    in_oper_proc: ptr::null(),
};

/// `boolean` — `tclBooleanType` (`generic/tclObj.c:227-233`), with the
/// `updateStringProc` deliberately NULL as Tcl leaves it. A boolean is only ever
/// produced from a value that already had a string rep, so the rule at
/// `generic/tclObj.c:1723-1731` holds without one.
pub static BOOLEAN_TYPE: TclObjType = TclObjType {
    name: c"boolean".as_ptr(),
    free_internal_rep_proc: ptr::null(),
    dup_internal_rep_proc: ptr::null(),
    update_string_proc: ptr::null(),
    set_from_any_proc: set_boolean_from_any as *const c_void,
    version: OBJTYPE_V0,
    length_proc: ptr::null(),
    index_proc: ptr::null(),
    slice_proc: ptr::null(),
    reverse_proc: ptr::null(),
    get_elements_proc: ptr::null(),
    set_element_proc: ptr::null(),
    replace_proc: ptr::null(),
    in_oper_proc: ptr::null(),
};

/// The host's own types, in the order [`register_host_types`] registers them.
pub static HOST_TYPES: [&TclObjType; 5] = [
    &LIST_TYPE,
    &DICT_TYPE,
    &WIDE_TYPE,
    &DOUBLE_TYPE,
    &BOOLEAN_TYPE,
];

// ---------------------------------------------------------------------------
// The registry
// ---------------------------------------------------------------------------

/// A registered type. A bare `*const TclObjType` is not `Send`, and the
/// registry is behind a `Mutex` exactly as Tcl's is
/// (`generic/tclObj.c:27,814-817`), so the pointer is wrapped rather than the
/// lock abandoned. Sound because a `Tcl_ObjType` is written once and read
/// forever — Tcl's own are `const` for that reason.
#[derive(Clone, Copy)]
struct Registered(*const TclObjType);
unsafe impl Send for Registered {}

/// The process-wide type table, keyed by name.
///
/// Tcl's is a `Tcl_HashTable` of `typePtr->name` to `typePtr`, guarded by a
/// mutex and shared by every interpreter (`generic/tclObj.c:806-818`). A vector
/// is the same table at this size, and the replace-by-name rule the header
/// documents (`generic/tclObj.c:799-801`) is reproduced.
static TYPES: Mutex<Vec<(String, Registered)>> = Mutex::new(Vec::new());

/// `Tcl_RegisterObjType` (`generic/tclObj.c:806-818`).
///
/// # Safety
/// `ty` must point at a `Tcl_ObjType` that lives for the rest of the process,
/// which the header states as a requirement in so many words
/// (`generic/tclObj.c:808-810`: "storage must be statically allocated (must live
/// forever)").
pub unsafe fn register(ty: *const TclObjType) {
    let name = name_of(ty);
    let mut table = TYPES.lock().expect("type registry poisoned");
    match table.iter_mut().find(|(n, _)| *n == name) {
        Some(slot) => slot.1 = Registered(ty),
        None => table.push((name, Registered(ty))),
    }
}

/// Register the host's own types, once, the way `TclInitObjSubsystem` does
/// (`generic/tclObj.c:370-386`).
pub fn register_host_types() {
    for ty in HOST_TYPES {
        unsafe { register(ty as *const TclObjType) };
    }
}

/// `Tcl_GetObjType` (`generic/tclObj.c:895-909`): NULL when the name is unknown.
pub fn lookup(name: &str) -> *const TclObjType {
    TYPES
        .lock()
        .expect("type registry poisoned")
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, t)| t.0)
        .unwrap_or(ptr::null())
}

/// Every registered type name, in registration order.
///
/// Tcl's `Tcl_AppendAllObjTypes` walks a hash table and so has no defined order
/// (`generic/tclObj.c:868-874`); this one is ordered, which is a strictly
/// stronger promise and makes the probe's output comparable between runs.
pub fn registered_names() -> Vec<String> {
    TYPES
        .lock()
        .expect("type registry poisoned")
        .iter()
        .map(|(n, _)| n.clone())
        .collect()
}

/// How many types are registered. Reported by the probe: Tk registering ten of
/// its own is the measurement that says `TkRegisterObjTypes` ran.
pub fn registered_count() -> usize {
    TYPES.lock().expect("type registry poisoned").len()
}

/// The registered types, for a caller that wants to exercise them.
///
/// # Safety
/// The returned pointers are only valid for as long as the code that registered
/// them is loaded, which for Tk's ten is the life of the process
/// ([`super::load`] never calls `dlclose`).
pub fn registered_types() -> Vec<*const TclObjType> {
    TYPES
        .lock()
        .expect("type registry poisoned")
        .iter()
        .map(|(_, t)| t.0)
        .collect()
}

/// A type's name.
///
/// # Safety
/// `ty` must point at a live `Tcl_ObjType` whose `name` is a NUL-terminated
/// string, which the struct's own definition requires
/// (`generic/tcl.h:658`, and `Tcl_AppendAllObjTypes` relies on it outright at
/// `generic/tclObj.c:863-866`).
pub unsafe fn name_of(ty: *const TclObjType) -> String {
    if ty.is_null() || (*ty).name.is_null() {
        return "<none>".to_string();
    }
    String::from_utf8_lossy(CStr::from_ptr((*ty).name).to_bytes()).into_owned()
}

// ---------------------------------------------------------------------------
// The four procs, called on a type this side does not own
// ---------------------------------------------------------------------------

/// `TclFreeInternalRep` (`generic/tclInt.h:4544-4550`): call the type's
/// `freeIntRepProc` if it has one, then clear `typePtr`.
///
/// Clearing `typePtr` afterwards is Tcl's, not the proc's. Tk's own procs
/// mostly do it themselves — `FreeBorderObjProc` ends with
/// `objPtr->typePtr = NULL` (`tk9.0.4/generic/tk3d.c:522`) — but a type is not
/// required to, so the caller does it unconditionally.
///
/// # Safety
/// `obj` must point at a live `Tcl_Obj`. If its type is Tk's, this calls into
/// Tk, which will read `internalRep` as whatever that type put there.
pub unsafe fn free_internal_rep(obj: *mut TclObj) {
    let ty = (*obj).type_ptr;
    if ty.is_null() {
        return;
    }
    if !(*ty).free_internal_rep_proc.is_null() {
        let f: unsafe extern "C" fn(*mut TclObj) =
            std::mem::transmute((*ty).free_internal_rep_proc);
        f(obj);
    }
    (*obj).type_ptr = ptr::null();
    (*obj).internal_rep.ptr1 = ptr::null_mut();
    (*obj).internal_rep.ptr2 = ptr::null_mut();
}

/// The internal-rep half of `SetDuplicateObj` (`generic/tclObj.c:1548-1555`).
///
/// The NULL-`dupIntRepProc` fallback is a *bitwise* copy of the whole 16-byte
/// union plus the type pointer, which is only correct for a rep that owns
/// nothing — and is precisely what Tcl does.
///
/// A detail worth stating because it is invisible in the C: Tk's dup procs do
/// not necessarily write the whole union. `DupBorderObjProc` writes `typePtr`
/// and `ptr1` and leaves `ptr2` alone (`tk9.0.4/generic/tk3d.c:560-572`), so the
/// destination's `ptr2` has to already be something safe. It is: `dup` comes
/// from [`obj::alloc`], which zeroes the union.
///
/// # Safety
/// `src` may be any live `Tcl_Obj`; `dup` must be fresh host storage with no
/// internal rep of its own.
pub unsafe fn dup_internal_rep(src: *mut TclObj, dup: *mut TclObj) {
    let ty = (*src).type_ptr;
    if ty.is_null() {
        return;
    }
    if (*ty).dup_internal_rep_proc.is_null() {
        (*dup).internal_rep = (*src).internal_rep;
        (*dup).type_ptr = ty;
        return;
    }
    let f: unsafe extern "C" fn(*mut TclObj, *mut TclObj) =
        std::mem::transmute((*ty).dup_internal_rep_proc);
    f(src, dup);
}

/// Call the type's `updateStringProc`.
///
/// # Safety
/// `(*obj).type_ptr` must be non-NULL with a non-NULL `updateStringProc`;
/// [`obj::string_of`] checks both before calling.
pub unsafe fn call_update_string(obj: *mut TclObj) {
    let ty = (*obj).type_ptr;
    let f: unsafe extern "C" fn(*mut TclObj) = std::mem::transmute((*ty).update_string_proc);
    f(obj);
}

/// `Tcl_ConvertToType` (`generic/tclObj.c:931-957`).
///
/// A target type with no `setFromAnyProc` is an error, which is the answer for
/// all twelve of Tk's types and is why Tk never calls this.
///
/// # Safety
/// `obj` must point at a live `Tcl_Obj` and `ty` at a live `Tcl_ObjType`.
pub unsafe fn convert_to_type(
    interp: *mut c_void,
    obj: *mut TclObj,
    ty: *const TclObjType,
) -> c_int {
    if std::ptr::eq((*obj).type_ptr, ty) {
        return TCL_OK;
    }
    if (*ty).set_from_any_proc.is_null() {
        return TCL_ERROR;
    }
    let f: unsafe extern "C" fn(*mut c_void, *mut TclObj) -> c_int =
        std::mem::transmute((*ty).set_from_any_proc);
    f(interp, obj)
}

/// `Tcl_StoreInternalRep` (`generic/tclObj.c:1910-1927`): shimmer out of the
/// current type, then adopt the given rep. A NULL `ir` leaves the value with no
/// internal rep at all, which is the documented way to say "forget this".
///
/// # Safety
/// `obj` must point at a live `Tcl_Obj`; `ty` must outlive it; `ir`, when
/// non-NULL, must describe a rep `ty`'s procs can handle.
pub unsafe fn store_internal_rep(
    obj: *mut TclObj,
    ty: *const TclObjType,
    ir: *const TclObjInternalRep,
) {
    free_internal_rep(obj);
    if !ir.is_null() {
        (*obj).internal_rep = *ir;
        (*obj).type_ptr = ty;
    }
}

/// `TclFetchInternalRep` (`generic/tclInt.h:4736-4739`): the rep when the type
/// matches, NULL otherwise.
///
/// # Safety
/// `obj` must point at a live `Tcl_Obj`.
pub unsafe fn fetch_internal_rep(
    obj: *mut TclObj,
    ty: *const TclObjType,
) -> *mut TclObjInternalRep {
    if std::ptr::eq((*obj).type_ptr, ty) {
        ptr::addr_of_mut!((*obj).internal_rep)
    } else {
        ptr::null_mut()
    }
}

// ---------------------------------------------------------------------------
// Type identity
// ---------------------------------------------------------------------------

/// Whether `ty` is the host's list type. Pointer identity, which is how Tk asks
/// the same question of its own types (`tk9.0.4/generic/tk3d.c:1254`).
pub fn is_list(ty: *const TclObjType) -> bool {
    std::ptr::eq(ty, &LIST_TYPE)
}

/// Whether `ty` is the host's dictionary type.
pub fn is_dict(ty: *const TclObjType) -> bool {
    std::ptr::eq(ty, &DICT_TYPE)
}

/// Whether `ty` is the host's integer type.
pub fn is_wide(ty: *const TclObjType) -> bool {
    std::ptr::eq(ty, &WIDE_TYPE)
}

/// Whether `ty` is the host's double type.
pub fn is_double(ty: *const TclObjType) -> bool {
    std::ptr::eq(ty, &DOUBLE_TYPE)
}

/// Whether `ty` is the host's boolean type.
pub fn is_boolean(ty: *const TclObjType) -> bool {
    std::ptr::eq(ty, &BOOLEAN_TYPE)
}

// ---------------------------------------------------------------------------
// The scalar reps, stored inline
// ---------------------------------------------------------------------------

/// The `wideValue` arm of `Tcl_ObjInternalRep` (`generic/tcl.h:721`), which
/// shares its first eight bytes with `twoPtrValue.ptr1` — offset 32 of the
/// object either way.
///
/// # Safety
/// `obj`'s type must be [`WIDE_TYPE`] or [`BOOLEAN_TYPE`].
pub unsafe fn wide_bits(obj: *mut TclObj) -> i64 {
    (*obj).internal_rep.ptr1 as i64
}

/// Store an integer in the `wideValue` arm.
///
/// # Safety
/// `obj` must point at a live `Tcl_Obj` whose old rep has been released.
unsafe fn set_wide_bits(obj: *mut TclObj, v: i64) {
    (*obj).internal_rep.ptr1 = v as *mut c_void;
    (*obj).internal_rep.ptr2 = ptr::null_mut();
}

/// The `doubleValue` arm (`generic/tcl.h:718`), read out of the same eight
/// bytes.
///
/// # Safety
/// `obj`'s type must be [`DOUBLE_TYPE`].
pub unsafe fn double_bits(obj: *mut TclObj) -> f64 {
    f64::from_bits((*obj).internal_rep.ptr1 as u64)
}

/// Store a double in the `doubleValue` arm.
///
/// # Safety
/// As [`set_wide_bits`].
unsafe fn set_double_bits(obj: *mut TclObj, v: f64) {
    (*obj).internal_rep.ptr1 = v.to_bits() as *mut c_void;
    (*obj).internal_rep.ptr2 = ptr::null_mut();
}

// ---------------------------------------------------------------------------
// Constructors
// ---------------------------------------------------------------------------

/// A list value holding `elems`, taking a reference to each.
///
/// # Safety
/// Each element must be a live `Tcl_Obj` the caller is willing to share.
pub unsafe fn new_list(elems: &[*mut TclObj]) -> *mut TclObj {
    let o = obj::alloc();
    for e in elems {
        obj::incr_ref(*e);
    }
    (*o).type_ptr = &LIST_TYPE;
    (*o).internal_rep.ptr1 = Box::into_raw(Box::new(HostList {
        elems: elems.to_vec(),
    })) as *mut c_void;
    (*o).internal_rep.ptr2 = ptr::null_mut();
    // The string rep is now stale: it says "" and the value is a list. Tcl
    // would have built the object with no string rep at all; this drops the one
    // `alloc` made, which is the same state.
    obj::invalidate_string_rep(o);
    o
}

/// A dictionary value holding `pairs`, taking a reference to each key and value.
///
/// # Safety
/// As [`new_list`].
pub unsafe fn new_dict(pairs: &[(*mut TclObj, *mut TclObj)]) -> *mut TclObj {
    let o = obj::alloc();
    for (k, v) in pairs {
        obj::incr_ref(*k);
        obj::incr_ref(*v);
    }
    (*o).type_ptr = &DICT_TYPE;
    (*o).internal_rep.ptr1 = Box::into_raw(Box::new(HostDict {
        pairs: pairs.to_vec(),
    })) as *mut c_void;
    (*o).internal_rep.ptr2 = ptr::null_mut();
    obj::invalidate_string_rep(o);
    o
}

/// An integer value, carrying both reps — as `Tcl_NewWideIntObj` does
/// (`generic/tclObj.c`), except that the string rep is built now rather than
/// left to `updateStringProc`.
///
/// # Safety
/// The result is pinned host storage with count 0.
pub unsafe fn new_wide(v: i64) -> *mut TclObj {
    let o = obj::new_string(v.to_string().as_bytes());
    (*o).type_ptr = &WIDE_TYPE;
    set_wide_bits(o, v);
    o
}

/// A double value.
///
/// # Safety
/// As [`new_wide`].
pub unsafe fn new_double(v: f64) -> *mut TclObj {
    let o = obj::new_string(crate::runtime::format_double(v).as_bytes());
    (*o).type_ptr = &DOUBLE_TYPE;
    set_double_bits(o, v);
    o
}

/// A boolean value. Its string rep is `1` or `0`, which is what Tcl's own
/// boolean stringifies to, and it has to exist because [`BOOLEAN_TYPE`] has no
/// `updateStringProc`.
///
/// # Safety
/// As [`new_wide`].
pub unsafe fn new_boolean(v: bool) -> *mut TclObj {
    let o = obj::new_string(if v { b"1" } else { b"0" });
    (*o).type_ptr = &BOOLEAN_TYPE;
    set_wide_bits(o, i64::from(v));
    o
}

// ---------------------------------------------------------------------------
// Shimmering: string to internal rep
// ---------------------------------------------------------------------------

/// The list behind `obj`, parsing its string rep into one if it is not a list
/// yet.
///
/// This is the direction Tk depends on. `Initialize` builds the command that
/// creates the main window as a *string* — `Tcl_NewStringObj("toplevel . -class",
/// TCL_INDEX_NONE)` — and then calls `Tcl_ListObjAppendElement` on it
/// (`tk9.0.4/generic/tkWindow.c:3382-3384`). In Tcl that works because a value
/// carries a string rep and an internal rep at once and converts between them on
/// demand; the parse here is `crate::list::split`, this crate's own reading of
/// Tcl list syntax.
///
/// # Safety
/// `obj` must point at a live `Tcl_Obj` whose string rep is a well formed list —
/// which is asserted, since a value that is not one is a caller error and Tcl
/// answers it with `TCL_ERROR` from a path this host does not have.
pub unsafe fn list_of(obj: *mut TclObj) -> &'static mut HostList {
    if !is_list((*obj).type_ptr) {
        let text = obj::text_of(obj);
        let words = crate::list::split(&text)
            .unwrap_or_else(|e| panic!("value is not a well formed Tcl list: {e}"));
        let elems: Vec<*mut TclObj> = words
            .iter()
            .map(|w| {
                let e = obj::new_string(w.as_bytes());
                obj::incr_ref(e);
                e
            })
            .collect();
        free_internal_rep(obj);
        (*obj).type_ptr = &LIST_TYPE;
        (*obj).internal_rep.ptr1 = Box::into_raw(Box::new(HostList { elems })) as *mut c_void;
    }
    &mut *((*obj).internal_rep.ptr1 as *mut HostList)
}

/// The dictionary behind `obj`, parsing its string rep into one if needed.
///
/// Tk starts a dictionary from `Tcl_NewObj()` — an empty string — and fills it
/// with `Tcl_DictObjPut` (`tk9.0.4/generic/tkUtil.c:1215-1223`), so the same
/// on-demand conversion a list needs applies here.
///
/// # Safety
/// As [`list_of`], and the string rep must have an even number of elements.
pub unsafe fn dict_of(obj: *mut TclObj) -> &'static mut HostDict {
    if !is_dict((*obj).type_ptr) {
        let text = obj::text_of(obj);
        let words = crate::list::split(&text)
            .unwrap_or_else(|e| panic!("value is not a well formed Tcl dictionary: {e}"));
        assert!(
            words.len().is_multiple_of(2),
            "missing value to go with key: a dictionary needs an even number of \
             elements, and this one has {}",
            words.len()
        );
        let pairs: Vec<(*mut TclObj, *mut TclObj)> = words
            .chunks(2)
            .map(|kv| {
                let k = obj::new_string(kv[0].as_bytes());
                let v = obj::new_string(kv[1].as_bytes());
                obj::incr_ref(k);
                obj::incr_ref(v);
                (k, v)
            })
            .collect();
        free_internal_rep(obj);
        (*obj).type_ptr = &DICT_TYPE;
        (*obj).internal_rep.ptr1 = Box::into_raw(Box::new(HostDict { pairs })) as *mut c_void;
    }
    &mut *((*obj).internal_rep.ptr1 as *mut HostDict)
}

/// The string rep of a value that has been changed through its internal rep is
/// no longer true, so drop it; [`obj::string_of`] will ask the type to rebuild
/// it.
///
/// This is the other direction of shimmering, and doing it lazily rather than
/// rebuilding the string on the spot is what makes `updateStringProc` a live
/// part of the contract instead of dead weight.
///
/// # Safety
/// `obj` must be host storage.
pub unsafe fn invalidate(obj: *mut TclObj) {
    obj::invalidate_string_rep(obj);
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

/// `freeIntRepProc` for [`LIST_TYPE`].
///
/// Tk calls this directly, out of `objPtr->typePtr`, whenever it shimmers a
/// value this side made a list into one of its own types
/// (`tk9.0.4/generic/tk3d.c:1343-1345` and eleven other sites). Without it the
/// `HostList` box leaks on every such conversion — silently, since nothing else
/// ever looks at it again.
unsafe extern "C" fn free_list_rep(o: *mut TclObj) {
    let rep = (*o).internal_rep.ptr1 as *mut HostList;
    if rep.is_null() {
        return;
    }
    let list = Box::from_raw(rep);
    for e in &list.elems {
        obj::decr_ref(*e);
    }
    drop(list);
    (*o).internal_rep.ptr1 = ptr::null_mut();
}

/// `dupIntRepProc` for [`LIST_TYPE`].
///
/// The elements are shared, not copied, and each gains a reference — which is
/// what `Tcl_DuplicateObj`'s own documentation says a list does
/// (`generic/tclObj.c:1529-1534`).
unsafe extern "C" fn dup_list_rep(src: *mut TclObj, dup: *mut TclObj) {
    let rep = &*((*src).internal_rep.ptr1 as *mut HostList);
    for e in &rep.elems {
        obj::incr_ref(*e);
    }
    (*dup).type_ptr = (*src).type_ptr;
    (*dup).internal_rep.ptr1 = Box::into_raw(Box::new(HostList {
        elems: rep.elems.clone(),
    })) as *mut c_void;
    (*dup).internal_rep.ptr2 = ptr::null_mut();
}

/// `updateStringProc` for [`LIST_TYPE`]: the canonical list form, built by this
/// crate's own `list::join`.
unsafe extern "C" fn update_string_of_list(o: *mut TclObj) {
    let rep = &*((*o).internal_rep.ptr1 as *mut HostList);
    let words: Vec<String> = rep.elems.iter().map(|e| obj::text_of(*e)).collect();
    obj::set_string(o, crate::list::join(&words).as_bytes());
}

/// `setFromAnyProc` for [`LIST_TYPE`]: the string rep read as a list.
unsafe extern "C" fn set_list_from_any(_interp: *mut c_void, o: *mut TclObj) -> c_int {
    if is_list((*o).type_ptr) {
        return TCL_OK;
    }
    let text = obj::text_of(o);
    match crate::list::split(&text) {
        Ok(words) => {
            let elems: Vec<*mut TclObj> = words
                .iter()
                .map(|w| {
                    let e = obj::new_string(w.as_bytes());
                    obj::incr_ref(e);
                    e
                })
                .collect();
            free_internal_rep(o);
            (*o).type_ptr = &LIST_TYPE;
            (*o).internal_rep.ptr1 = Box::into_raw(Box::new(HostList { elems })) as *mut c_void;
            TCL_OK
        }
        Err(_) => TCL_ERROR,
    }
}

// ---------------------------------------------------------------------------
// dict
// ---------------------------------------------------------------------------

/// `freeIntRepProc` for [`DICT_TYPE`]. See [`free_list_rep`] for why this may
/// not be NULL.
unsafe extern "C" fn free_dict_rep(o: *mut TclObj) {
    let rep = (*o).internal_rep.ptr1 as *mut HostDict;
    if rep.is_null() {
        return;
    }
    let dict = Box::from_raw(rep);
    for (k, v) in &dict.pairs {
        obj::decr_ref(*k);
        obj::decr_ref(*v);
    }
    drop(dict);
    (*o).internal_rep.ptr1 = ptr::null_mut();
}

/// `dupIntRepProc` for [`DICT_TYPE`].
unsafe extern "C" fn dup_dict_rep(src: *mut TclObj, dup: *mut TclObj) {
    let rep = &*((*src).internal_rep.ptr1 as *mut HostDict);
    for (k, v) in &rep.pairs {
        obj::incr_ref(*k);
        obj::incr_ref(*v);
    }
    (*dup).type_ptr = (*src).type_ptr;
    (*dup).internal_rep.ptr1 = Box::into_raw(Box::new(HostDict {
        pairs: rep.pairs.clone(),
    })) as *mut c_void;
    (*dup).internal_rep.ptr2 = ptr::null_mut();
}

/// `updateStringProc` for [`DICT_TYPE`]: key and value alternating, as a list.
unsafe extern "C" fn update_string_of_dict(o: *mut TclObj) {
    let rep = &*((*o).internal_rep.ptr1 as *mut HostDict);
    let mut words: Vec<String> = Vec::with_capacity(rep.pairs.len() * 2);
    for (k, v) in &rep.pairs {
        words.push(obj::text_of(*k));
        words.push(obj::text_of(*v));
    }
    obj::set_string(o, crate::list::join(&words).as_bytes());
}

/// `setFromAnyProc` for [`DICT_TYPE`].
unsafe extern "C" fn set_dict_from_any(_interp: *mut c_void, o: *mut TclObj) -> c_int {
    if is_dict((*o).type_ptr) {
        return TCL_OK;
    }
    let text = obj::text_of(o);
    let Ok(words) = crate::list::split(&text) else {
        return TCL_ERROR;
    };
    if !words.len().is_multiple_of(2) {
        return TCL_ERROR;
    }
    let pairs: Vec<(*mut TclObj, *mut TclObj)> = words
        .chunks(2)
        .map(|kv| {
            let k = obj::new_string(kv[0].as_bytes());
            let v = obj::new_string(kv[1].as_bytes());
            obj::incr_ref(k);
            obj::incr_ref(v);
            (k, v)
        })
        .collect();
    free_internal_rep(o);
    (*o).type_ptr = &DICT_TYPE;
    (*o).internal_rep.ptr1 = Box::into_raw(Box::new(HostDict { pairs })) as *mut c_void;
    TCL_OK
}

// ---------------------------------------------------------------------------
// int, double, boolean
// ---------------------------------------------------------------------------

/// `updateStringProc` for [`WIDE_TYPE`], the counterpart of
/// `UpdateStringOfInt` (`generic/tclObj.c:2636`).
unsafe extern "C" fn update_string_of_wide(o: *mut TclObj) {
    let v = wide_bits(o);
    obj::set_string(o, v.to_string().as_bytes());
}

/// `setFromAnyProc` for [`WIDE_TYPE`] — `SetIntFromAny`
/// (`generic/tclObj.c:2608`) over this crate's integer grammar.
unsafe extern "C" fn set_wide_from_any(_interp: *mut c_void, o: *mut TclObj) -> c_int {
    if is_wide((*o).type_ptr) {
        return TCL_OK;
    }
    let text = obj::text_of(o);
    match crate::list::wide(&text) {
        Ok(v) => {
            free_internal_rep(o);
            (*o).type_ptr = &WIDE_TYPE;
            set_wide_bits(o, v);
            TCL_OK
        }
        Err(_) => TCL_ERROR,
    }
}

/// `updateStringProc` for [`DOUBLE_TYPE`] — `UpdateStringOfDouble`
/// (`generic/tclObj.c:2522-2532`), which formats with `Tcl_PrintDouble`; this
/// crate's `format_double` is the same answer.
unsafe extern "C" fn update_string_of_double(o: *mut TclObj) {
    let v = double_bits(o);
    obj::set_string(o, crate::runtime::format_double(v).as_bytes());
}

/// `setFromAnyProc` for [`DOUBLE_TYPE`] — `SetDoubleFromAny`
/// (`generic/tclObj.c:2493-2500`).
unsafe extern "C" fn set_double_from_any(_interp: *mut c_void, o: *mut TclObj) -> c_int {
    if is_double((*o).type_ptr) {
        return TCL_OK;
    }
    let text = obj::text_of(o);
    match crate::list::parse_double(&text) {
        Some(v) => {
            free_internal_rep(o);
            (*o).type_ptr = &DOUBLE_TYPE;
            set_double_bits(o, v);
            TCL_OK
        }
        None => TCL_ERROR,
    }
}

/// `setFromAnyProc` for [`BOOLEAN_TYPE`] — `TclSetBooleanFromAny`
/// (`generic/tclObj.c:2100`), whose word table this crate already carries as
/// `runtime::tcl_bool`.
///
/// The string rep is left alone, which it must be: [`BOOLEAN_TYPE`] has no
/// `updateStringProc`, so this is the type that would panic in
/// `Tcl_GetStringFromObj` if it ever let `bytes` go NULL.
unsafe extern "C" fn set_boolean_from_any(_interp: *mut c_void, o: *mut TclObj) -> c_int {
    if is_boolean((*o).type_ptr) {
        return TCL_OK;
    }
    let value = fusevm::Value::Str(std::sync::Arc::new(obj::text_of(o)));
    match crate::runtime::tcl_bool(&value) {
        Ok(b) => {
            free_internal_rep(o);
            (*o).type_ptr = &BOOLEAN_TYPE;
            set_wide_bits(o, i64::from(b));
            TCL_OK
        }
        Err(_) => TCL_ERROR,
    }
}

// ---------------------------------------------------------------------------
// Exercising a type this side does not own
// ---------------------------------------------------------------------------

/// `sizeof(MMRep)` (`tk9.0.4/generic/tkObj.c:59-64`): `double value` at 0,
/// `int units` at 8, `Tk_Window tkwin` at 16, `double returnValue` at 24.
const MM_REP_SIZE: usize = 32;
/// Offset of `MMRep.units`, which `UpdateStringOfMM` insists is -1 before it
/// will run (`tk9.0.4/generic/tkObj.c:167-171`).
const MM_REP_UNITS: usize = 8;

/// `sizeof(WindowRep)` (`tk9.0.4/generic/tkObj.c:72-77`): `Tk_Window tkwin`,
/// `TkMainInfo *mainPtr`, `size_t epoch`. `DupWindowInternalRep` reads all
/// three out of the source without a NULL check
/// (`tk9.0.4/generic/tkObj.c:~1090`), so a NULL rep faults there — measured:
/// the run died silently right after the `Tcl_Alloc` that proc makes for the
/// copy.
const WINDOW_REP_SIZE: usize = 24;

/// What a seeded internal rep looked like, for the report line.
fn seed_name(name: &str) -> &'static str {
    match name {
        "mm" => "MMRep{units=-1}",
        "window" => "WindowRep{zeroed}",
        _ => "NULL",
    }
}

/// Put a type through its own procs and say what happened.
///
/// The point is not to test Tk. It is to check that the contract this side
/// implements is the one Tk's procs expect: that `dupIntRepProc` finds a
/// destination whose union is already safe to overwrite in part, that
/// `freeIntRepProc` finds the rep where it left it, and — for `mm`, the only Tk
/// type with an `updateStringProc` (`tk9.0.4/generic/tkObj.c:130`) — that a
/// string rep Tk writes into a `Tcl_Obj` this side allocated satisfies
/// [`obj::string_of`]'s NUL check.
///
/// Every rep is seeded with the state Tk's own conversion sites leave —
/// `internalRep.twoPtrValue.ptr1 = NULL` and a valid string rep
/// (`tk9.0.4/generic/tk3d.c:1341-1347`) — and every one of the ten was read
/// first to confirm it tolerates that. Two need more than zero:
///
/// * `mm` dereferences its rep in `DupMMInternalRep`
///   (`tk9.0.4/generic/tkObj.c:83`) and in `UpdateStringOfMM`, so it is given a
///   real `MMRep` in `Tcl_Alloc` storage — which is also what
///   `FreeMMInternalRep`'s unconditional `ckfree` requires.
/// * `textindex` dereferences `indexPtr->textPtr` in
///   `FreeTextIndexInternalRep` (`tk9.0.4/generic/tkTextIndex.c:93`) with no
///   NULL check at all, and building a `TkTextIndex` means reproducing a
///   struct that is not part of any contract. Its procs are reported and not
///   called.
///
/// # Safety
/// `ty` must be a `Tcl_ObjType` Tk registered, still loaded.
pub unsafe fn exercise(ty: *const TclObjType) -> String {
    let name = name_of(ty);
    let mut did: Vec<&str> = Vec::new();

    if name == "textindex" {
        return format!(
            "{name} version={} free={} dup={} update={} setany={} exercised=none \
             (FreeTextIndexInternalRep dereferences its rep unconditionally, \
             tkTextIndex.c:93)",
            (*ty).version,
            u8::from(!(*ty).free_internal_rep_proc.is_null()),
            u8::from(!(*ty).dup_internal_rep_proc.is_null()),
            u8::from(!(*ty).update_string_proc.is_null()),
            u8::from(!(*ty).set_from_any_proc.is_null()),
        );
    }

    let probe = obj::new_string(b"0");
    (*probe).type_ptr = ty;
    (*probe).internal_rep.ptr1 = ptr::null_mut();
    (*probe).internal_rep.ptr2 = ptr::null_mut();
    if name == "mm" {
        let rep = libc::calloc(1, MM_REP_SIZE) as *mut u8;
        assert!(!rep.is_null(), "out of memory seeding an MMRep");
        ptr::write_unaligned(rep as *mut f64, 42.0);
        ptr::write_unaligned(rep.add(MM_REP_UNITS) as *mut c_int, -1);
        (*probe).internal_rep.ptr1 = rep as *mut c_void;
    } else if name == "window" {
        let rep = libc::calloc(1, WINDOW_REP_SIZE);
        assert!(!rep.is_null(), "out of memory seeding a WindowRep");
        (*probe).internal_rep.ptr1 = rep;
    }

    if !(*ty).dup_internal_rep_proc.is_null() {
        let dup = obj::alloc();
        dup_internal_rep(probe, dup);
        assert!(
            std::ptr::eq((*dup).type_ptr, ty),
            "{name}'s dupIntRepProc did not set the duplicate's typePtr"
        );
        free_internal_rep(dup);
        obj::free_obj(dup);
        did.push("dup");
    }

    let mut produced = String::new();
    if !(*ty).update_string_proc.is_null() {
        obj::invalidate_string_rep(probe);
        // `obj::string_of` reproduces both of Tcl's panics
        // (`generic/tclObj.c:1723-1738`), so this call is the check: a string
        // rep Tk writes into a `Tcl_Obj` this side malloc'ed has to come back
        // NUL-terminated at `length`.
        produced = obj::text_of(probe);
        assert!(
            !produced.is_empty(),
            "{name}'s updateStringProc produced an empty string rep"
        );
        did.push("updateString");
    }

    if !(*ty).free_internal_rep_proc.is_null() {
        free_internal_rep(probe);
        assert!(
            (*probe).type_ptr.is_null(),
            "{name}'s freeIntRepProc left the type in place"
        );
        did.push("free");
    }
    obj::free_obj(probe);

    let done = if did.is_empty() {
        "none (all four procs are NULL)".to_string()
    } else {
        did.join("+")
    };
    let string_rep = if produced.is_empty() {
        String::new()
    } else {
        format!(" string={produced:?}")
    };
    format!(
        "{name} version={} free={} dup={} update={} setany={} seed={} exercised={done}{string_rep}",
        (*ty).version,
        u8::from(!(*ty).free_internal_rep_proc.is_null()),
        u8::from(!(*ty).dup_internal_rep_proc.is_null()),
        u8::from(!(*ty).update_string_proc.is_null()),
        u8::from(!(*ty).set_from_any_proc.is_null()),
        seed_name(&name),
    )
}

// ---------------------------------------------------------------------------
// The numeric readers behind the Tcl_Get*FromObj slots
// ---------------------------------------------------------------------------

/// `Tcl_GetDoubleFromObj`'s answer (`generic/tclObj.c:2438-2471`): an existing
/// double or integer rep first, then a conversion.
///
/// # Safety
/// `obj` must point at a live `Tcl_Obj`. It may be Tk's stack storage — this is
/// the path `GetTypeCache` takes (`tk9.0.4/generic/tkObj.c:205`), so nothing
/// here may touch `refCount` or free `bytes`, and it does not.
pub unsafe fn double_of(obj: *mut TclObj) -> Option<f64> {
    let ty = (*obj).type_ptr;
    if is_double(ty) {
        return Some(double_bits(obj));
    }
    if is_wide(ty) || is_boolean(ty) {
        return Some(wide_bits(obj) as f64);
    }
    if set_double_from_any(ptr::null_mut(), obj) == TCL_OK {
        return Some(double_bits(obj));
    }
    None
}

/// `Tcl_GetWideIntFromObj`'s answer.
///
/// # Safety
/// As [`double_of`].
pub unsafe fn wide_of(obj: *mut TclObj) -> Option<i64> {
    let ty = (*obj).type_ptr;
    if is_wide(ty) || is_boolean(ty) {
        return Some(wide_bits(obj));
    }
    if set_wide_from_any(ptr::null_mut(), obj) == TCL_OK {
        return Some(wide_bits(obj));
    }
    None
}

/// `Tcl_GetBooleanFromObj`'s answer.
///
/// # Safety
/// As [`double_of`].
pub unsafe fn bool_of(obj: *mut TclObj) -> Option<bool> {
    let ty = (*obj).type_ptr;
    if is_boolean(ty) || is_wide(ty) {
        return Some(wide_bits(obj) != 0);
    }
    if is_double(ty) {
        return Some(double_bits(obj) != 0.0);
    }
    if set_boolean_from_any(ptr::null_mut(), obj) == TCL_OK {
        return Some(wide_bits(obj) != 0);
    }
    None
}
