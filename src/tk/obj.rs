//! The shadow `Tcl_Obj`: its storage, its ownership discipline, and the bridge
//! to this crate's own values.
//!
//! [`super::abi::TclObj`] is the layout — 48 bytes, five fields, measured with
//! `offsetof` against `generic/tcl.h:744-765` and pinned by `tests/tk_abi.rs`.
//! This module is everything else about it: who allocates one, who may move it,
//! who frees it, and how a value that Tk built as a string becomes a list.
//!
//! # Why this cannot be answered from behind the stub table
//!
//! Three of the operations Tk performs on a value never reach the table:
//!
//! * `Tcl_IncrRefCount(objPtr)` is `((void)++(objPtr)->refCount)`
//!   (`generic/tcl.h:2517-2519`).
//! * `Tcl_DecrRefCount(objPtr)` is `if (_objPtr->refCount-- <= 1) {
//!   TclFreeObj(_objPtr); }` (`generic/tcl.h:2524-2531`) — the count is written
//!   back *before* the one call that does reach the table.
//! * `Tcl_IsShared(objPtr)` is `((objPtr)->refCount > 1)`
//!   (`generic/tcl.h:2532-2534`).
//!
//! So `refCount` at offset 0 of writable memory is not a convention this side
//! may choose; it is compiled into Tk.
//!
//! # Ownership discipline
//!
//! Every raw pointer that crosses this boundary obeys one of these rules, and
//! each is asserted rather than assumed wherever an assertion is possible.
//!
//! 1. **Storage is pinned.** A host-created value is one 48-byte
//!    `libc::malloc` block. It is never `realloc`ed, never moved, never copied
//!    by value, and never handed out by reference into a `Vec` that could
//!    reallocate. The address Tk receives stays that value's identity for its
//!    whole life — and only for that: the allocator hands the same block out
//!    again afterwards, so an address is an identity *while the object is
//!    live* and not a moment longer. [`serial_of`] is the identity that
//!    outlasts the object. `libc::malloc` and not Rust's allocator because the same
//!    block may be freed through `Tcl_Free` (`generic/tcl.h:2451-2463` makes
//!    `ckfree` an alias for it outside Tcl's own build).
//!
//! 2. **Not every `Tcl_Obj *` is host storage.** Tk builds them on its own C
//!    stack. `TkpScanWindowId` fills four fields of a stack `Tcl_Obj` and points
//!    `bytes` at a string it does not own (`tk9.0.4/macosx/tkMacOSXEmbed.c:160-165`),
//!    and `GetTypeCache` fills three and leaves `refCount` *uninitialised*
//!    (`tk9.0.4/generic/tkObj.c:201-206`). Therefore no host function on a read
//!    path may touch `refCount`, free `bytes`, or keep a pointer to an object
//!    past the call that was handed it. [`is_host_allocated`] is how a function
//!    that must know, knows.
//!
//! 3. **`tclFreeObj` is the single destruction path.** It is the only place a
//!    `Tcl_Obj` block is freed. It asserts on entry that the count has already
//!    been driven to zero — which is direct evidence that Tk's inline arithmetic
//!    landed on offset 0 of a value Tcl never allocated — and that the object is
//!    one this side allocated, which is direct evidence that nothing has handed
//!    a stack object to the free path.
//!
//! 4. **`bytes` is owned by the object.** It is either NULL, meaning "no string
//!    rep, regenerate it from the internal rep", or a `libc::malloc` block of
//!    `length + 1` whose last byte is 0. There is no shared empty-string
//!    sentinel here, unlike Tcl's `&tclEmptyString` (`generic/tclInt.h:4491-4492`
//!    and the `bytes != &tclEmptyString` guard at `generic/tclInt.h:4565`), so
//!    freeing is unconditional and one branch shorter.
//!
//! 5. **`typePtr` outlives every object of its type.** Host types are `static`;
//!    Tk's ten are `static const` inside a dylib that [`super::load`] never
//!    closes. Neither is ever freed, which is what makes a bare pointer
//!    comparison the identity test for a type.
//!
//! 6. **`internalRep` is owned by whatever `typePtr` says.** Nothing else may
//!    free it, and it is released only through that type's `freeIntRepProc`.
//!    See [`super::objtype`] for why a host type without one is a leak Tk
//!    causes on purpose.

use std::collections::BTreeMap;
use std::ffi::{c_char, c_void};
use std::mem::{offset_of, size_of};
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use fusevm::Value;

use super::abi::{TclObj, TclObjInternalRep};
use super::objtype;

// The layout claims this module's safety rests on, checked at compile time so
// that a change to `abi::TclObj` cannot reach a run. `tests/tk_abi.rs` checks
// the same numbers against the C header; these stop the build instead.
const _: () = assert!(size_of::<TclObj>() == 48);
const _: () = assert!(offset_of!(TclObj, ref_count) == 0);
const _: () = assert!(offset_of!(TclObj, bytes) == 8);
const _: () = assert!(offset_of!(TclObj, length) == 16);
const _: () = assert!(offset_of!(TclObj, type_ptr) == 24);
const _: () = assert!(offset_of!(TclObj, internal_rep) == 32);
const _: () = assert!(size_of::<TclObjInternalRep>() == 16);

/// `TCL_INDEX_NONE` (`generic/tcl.h`, measured as -1). `TclFreeObj` writes it
/// into `length` to mark an object as being deleted rather than shimmered
/// (`generic/tclObj.c:1404-1405`).
pub const TCL_INDEX_NONE: isize = -1;

/// `Tcl_DictSearch` (`generic/tcl.h:1262-1268`). Measured `sizeof` 24; `next`
/// 0, `epoch` 8, `dictionaryPtr` 16.
///
/// Caller-allocated, like `Tcl_DString` and `Tcl_CmdInfo`: Tk declares one on
/// its stack and hands over a pointer. The header states that its fields belong
/// to `tclDictObj.c` and nothing outside it (`generic/tcl.h:1257-1259`), which
/// is what makes them free for this side to use as a cursor of its own shape.
/// It lives here rather than in [`super::abi`] because nothing but the
/// dictionary slots has any business with it.
#[repr(C)]
pub struct TclDictSearch {
    /// Where the walk has reached. `tclDictObj.c` keeps a hash-search position;
    /// this side keeps the index of the next pair.
    pub next: *mut c_void,
    /// Nonzero while the search is live, 0 once it has ended — which is the
    /// role Tcl gives it too (`generic/tcl.h:1265-1266`).
    pub epoch: usize,
    /// The dictionary being walked.
    pub dictionary_ptr: *mut c_void,
}

/// Every live object this side allocated: where it is, and which object it is.
///
/// The key is the mechanism behind rule 2 — it is the only way to tell a value
/// that came out of [`alloc`] from one Tk built on its stack, and the free path
/// refuses to run on anything that is not in it.
///
/// The value is that object's serial number, and it is here because an address
/// is not an identity. `libc::malloc` reuses a block as soon as it is freed, so
/// two objects whose lifetimes do not overlap can share an address, and no
/// amount of looking at the key distinguishes them. The serial does: it comes
/// from [`NEXT_SERIAL`] and is never issued twice in a process. See
/// [`serial_of`] for which of the two questions a caller wants.
///
/// A `BTreeMap` because it is `const`-constructible and this is not a hot path
/// — `Tk_Init` allocates in the hundreds.
static LIVE: Mutex<BTreeMap<usize, u64>> = Mutex::new(BTreeMap::new());

/// The next serial number to issue. Monotone and never reset, so a serial names
/// one object for the whole life of the process even after its address has been
/// handed to something else.
static NEXT_SERIAL: AtomicU64 = AtomicU64::new(1);

/// Lifetime counters, reported by the probe.
static CREATED: AtomicU64 = AtomicU64::new(0);
static FREED: AtomicU64 = AtomicU64::new(0);

/// `(created, freed, live)` since the process started.
pub fn counts() -> (u64, u64, usize) {
    let live = LIVE.lock().expect("object registry poisoned").len();
    (
        CREATED.load(Ordering::Relaxed),
        FREED.load(Ordering::Relaxed),
        live,
    )
}

/// Whether `obj` is a value [`alloc`] produced and [`free_obj`] has not yet
/// taken back.
///
/// This is a question about an *address*, and it is the question a stub body
/// has: it is handed a bare `Tcl_Obj *` and must decide whether the memory is
/// this side's before it touches it, with nothing else to go on. It cannot tell
/// one object from the next one the allocator puts at the same address, so
/// nothing that outlives the call it was asked in may rely on it. A caller that
/// held a pointer across a free wants [`serial_of`] instead.
///
/// # Safety
/// None: only the pointer's numeric value is read, never the memory. That is
/// the point — it is the question a function has to answer *before* it is
/// allowed to dereference an object as host storage.
pub fn is_host_allocated(obj: *const TclObj) -> bool {
    LIVE.lock()
        .expect("object registry poisoned")
        .contains_key(&(obj as usize))
}

/// Which object is living at `obj`, or `None` if none is.
///
/// The identity question, as against [`is_host_allocated`]'s address question.
/// Storage is pinned (rule 1) and only [`free_obj`] ever removes an entry, so
/// for a serial `s` observed while an object was live:
///
/// * `serial_of(p) == Some(s)` means that object is still live;
/// * anything else — `None`, or `Some` of a different serial — means the free
///   path ran on it. A later allocation landing on the same address cannot
///   disguise that, because it carries a serial of its own.
///
/// # Safety
/// None, for the same reason as [`is_host_allocated`]: the pointer is compared,
/// never dereferenced.
pub fn serial_of(obj: *const TclObj) -> Option<u64> {
    LIVE.lock()
        .expect("object registry poisoned")
        .get(&(obj as usize))
        .copied()
}

/// A fresh value with the empty string as its string rep.
///
/// The shape is `TclNewObj` (`generic/tclInt.h:4301-4308`): count 0, an empty
/// string rep, no type. It differs in two ways, both deliberate:
///
/// * Tcl points `bytes` at the shared `tclEmptyString`; this allocates, per
///   rule 4 of the module discipline.
/// * Tcl leaves `internalRep` holding whatever the recycled block had — sound
///   there only because `typePtr` is NULL and nothing may read it. This zeroes
///   it, so that a stale pointer is never one missed `typePtr` assignment away
///   from being dereferenced.
///
/// # Safety
/// The returned pointer is pinned host storage. The caller owns it and must
/// hand it to Tk or release it through [`free_obj`]; it starts unreferenced,
/// which is exactly Tcl's contract for `Tcl_NewObj`
/// (`generic/tclObj.c:1149-1151`).
pub unsafe fn alloc() -> *mut TclObj {
    new_string(b"")
}

/// A fresh value whose string rep is a copy of `bytes`.
///
/// # Safety
/// As [`alloc`].
pub unsafe fn new_string(bytes: &[u8]) -> *mut TclObj {
    let p = libc::malloc(size_of::<TclObj>()) as *mut TclObj;
    assert!(!p.is_null(), "out of memory allocating Tcl_Obj");
    ptr::write(
        p,
        TclObj {
            ref_count: 0,
            bytes: ptr::null_mut(),
            length: 0,
            type_ptr: ptr::null(),
            internal_rep: TclObjInternalRep {
                ptr1: ptr::null_mut(),
                ptr2: ptr::null_mut(),
            },
        },
    );
    set_string(p, bytes);
    CREATED.fetch_add(1, Ordering::Relaxed);
    let serial = NEXT_SERIAL.fetch_add(1, Ordering::Relaxed);
    let displaced = LIVE
        .lock()
        .expect("object registry poisoned")
        .insert(p as usize, serial);
    assert!(
        displaced.is_none(),
        "malloc returned {p:?} for a second Tcl_Obj while the first is still live"
    );
    p
}

/// `Tcl_IncrRefCount`, for the host side of the boundary.
///
/// Tk uses the macro (`generic/tcl.h:2517-2519`) and never calls here; this
/// exists so that host code takes a reference the same way and the two agree.
///
/// # Safety
/// `obj` must point at a live `Tcl_Obj`. It may be Tk's stack storage, in which
/// case the caller has already decided that a reference is legitimate — which,
/// per rule 2, it never is for a value the host intends to outlive the call.
pub unsafe fn incr_ref(obj: *mut TclObj) {
    (*obj).ref_count += 1;
}

/// `Tcl_DecrRefCount` (`generic/tcl.h:2524-2531`): post-decrement, and free
/// once the old value was 1 or less.
///
/// # Safety
/// `obj` must be a reference this side took with [`incr_ref`].
pub unsafe fn decr_ref(obj: *mut TclObj) {
    if (*obj).ref_count <= 1 {
        (*obj).ref_count -= 1;
        free_obj(obj);
    } else {
        (*obj).ref_count -= 1;
    }
}

/// Take the reference held by a slot in host state and drop it, for a slot that
/// may be NULL.
///
/// # Safety
/// As [`decr_ref`], plus `obj` may be NULL.
pub unsafe fn release(obj: *mut TclObj) {
    if !obj.is_null() {
        decr_ref(obj);
    }
}

/// `TclFreeObj` (`generic/tclObj.c:1394-1482`), the single destruction path.
///
/// The order is Tcl's and matters: the string rep goes first so that
/// `length = TCL_INDEX_NONE` can signal deletion rather than shimmering to a
/// `freeIntRepProc` that checks (`generic/tclObj.c:1398-1405`), and only then is
/// the internal rep released.
///
/// # Safety
/// `obj` must be host storage with its reference count already driven to zero —
/// both of which are asserted, because a violation of either is the exact shape
/// an ABI mistake takes.
pub unsafe fn free_obj(obj: *mut TclObj) {
    if obj.is_null() {
        return;
    }
    // The interesting assertion of the whole exercise. Tk reached this through
    // the `Tcl_DecrRefCount` macro, which decremented `objPtr->refCount` in
    // place before calling (`generic/tcl.h:2524-2531`). This block came from
    // `libc::malloc` on the Rust side, so a count of zero or below here is
    // direct evidence that Tk's inline refcount arithmetic worked on a value
    // Tcl never allocated. Anything else means the layout is wrong.
    assert!(
        (*obj).ref_count <= 0,
        "TclFreeObj reached with refCount {}; Tk's inline Tcl_DecrRefCount did \
         not land on Tcl_Obj offset 0",
        (*obj).ref_count
    );
    // Rule 2: Tk builds `Tcl_Obj`s on its own stack
    // (`tk9.0.4/macosx/tkMacOSXEmbed.c:160-165`,
    // `tk9.0.4/generic/tkObj.c:201-206`). Freeing one would be a wild `free`,
    // and it would happen silently. It never should: Tk releases those by
    // calling `freeIntRepProc` directly and letting the frame go
    // (`tk9.0.4/macosx/tkMacOSXEmbed.c:172-174`).
    assert!(
        is_host_allocated(obj),
        "TclFreeObj called on {obj:?}, which this side never allocated — a \
         caller-owned Tcl_Obj must never reach the free path"
    );

    invalidate_string_rep(obj);
    (*obj).length = TCL_INDEX_NONE;
    objtype::free_internal_rep(obj);

    LIVE.lock()
        .expect("object registry poisoned")
        .remove(&(obj as usize));
    FREED.fetch_add(1, Ordering::Relaxed);
    libc::free(obj as *mut c_void);
}

/// `Tcl_DuplicateObj` (`generic/tclObj.c:1558-1567`) over the `SetDuplicateObj`
/// macro (`generic/tclObj.c:1539-1556`).
///
/// Two details of that macro are load-bearing and easy to lose. A source with no
/// string rep produces a duplicate with no string rep — not an empty one — and a
/// type with no `dupIntRepProc` gets a *bitwise* copy of the internal rep, which
/// is only sound for a rep that owns nothing. Both are reproduced exactly.
///
/// # Safety
/// `obj` may be any live `Tcl_Obj`, including Tk's stack storage: this reads it
/// and never retains it. The result is fresh host storage with count 0.
pub unsafe fn duplicate(obj: *mut TclObj) -> *mut TclObj {
    let dup = alloc();
    if (*obj).bytes.is_null() {
        invalidate_string_rep(dup);
    } else {
        set_string(dup, string_bytes_unchecked(obj));
    }
    objtype::dup_internal_rep(obj, dup);
    dup
}

/// The string rep as it stands, without asking a type to produce one.
///
/// # Safety
/// `(*obj).bytes` must be non-NULL.
unsafe fn string_bytes_unchecked(obj: *mut TclObj) -> &'static [u8] {
    std::slice::from_raw_parts((*obj).bytes as *const u8, (*obj).length as usize)
}

/// `TclHasStringRep` (`generic/tclInt.h:4597-4598`).
///
/// # Safety
/// `obj` must point at a live `Tcl_Obj`.
pub unsafe fn has_string_rep(obj: *mut TclObj) -> bool {
    !(*obj).bytes.is_null()
}

/// `TclInvalidateStringRep` (`generic/tclInt.h:4561-4570`).
///
/// # Safety
/// `obj` must be host storage: this frees `bytes`, and rule 4 only holds for
/// objects [`alloc`] produced. Tk's stack objects point `bytes` at storage they
/// do not own (`tk9.0.4/macosx/tkMacOSXEmbed.c:163`).
pub unsafe fn invalidate_string_rep(obj: *mut TclObj) {
    if !(*obj).bytes.is_null() {
        libc::free((*obj).bytes as *mut c_void);
        (*obj).bytes = ptr::null_mut();
    }
}

/// Replace the string rep with a copy of `s`.
///
/// # Safety
/// As [`invalidate_string_rep`].
pub unsafe fn set_string(obj: *mut TclObj, s: &[u8]) {
    invalidate_string_rep(obj);
    let p = libc::malloc(s.len() + 1) as *mut c_char;
    assert!(!p.is_null(), "out of memory allocating string rep");
    ptr::copy_nonoverlapping(s.as_ptr() as *const c_char, p, s.len());
    *p.add(s.len()) = 0;
    (*obj).bytes = p;
    (*obj).length = s.len() as isize;
}

/// `Tcl_InitStringRep` (`generic/tclObj.c:1790-1841`), in the three shapes its
/// own header comment defines (`generic/tclObj.c:1755-1776`): allocate, allocate
/// and copy, or truncate in place.
///
/// # Safety
/// As [`set_string`], and `bytes` must address `num` readable bytes when it is
/// not NULL. The `objPtr->bytes != NULL && bytes != NULL` combination is
/// rejected, as Tcl's own `assert` does (`generic/tclObj.c:1796`).
pub unsafe fn init_string_rep(obj: *mut TclObj, bytes: *const c_char, num: usize) -> *mut c_char {
    assert!(
        (*obj).bytes.is_null() || bytes.is_null(),
        "Tcl_InitStringRep called with both an existing string rep and new bytes \
         (generic/tclObj.c:1796)"
    );
    if (*obj).bytes.is_null() {
        let p = libc::malloc(num + 1) as *mut c_char;
        assert!(!p.is_null(), "out of memory allocating string rep");
        if !bytes.is_null() && num > 0 {
            ptr::copy_nonoverlapping(bytes, p, num);
        }
        *p.add(num) = 0;
        (*obj).bytes = p;
        // Allocation-only leaves the value empty; allocate-and-copy sets the
        // length to what was copied (`generic/tclObj.c:1758-1770`).
        (*obj).length = if bytes.is_null() { 0 } else { num as isize };
    } else {
        // Truncate. The caller guarantees the earlier allocation was big
        // enough (`generic/tclObj.c:1772-1776`).
        assert!(
            num as isize <= (*obj).length || num == 0,
            "Tcl_InitStringRep truncation to {num} is longer than the {} bytes \
             already allocated",
            (*obj).length
        );
        (*obj).length = num as isize;
        *(*obj).bytes.add(num) = 0;
    }
    (*obj).bytes
}

/// `Tcl_GetStringFromObj` (`generic/tclObj.c:1707-1744`): the string rep, made
/// by the type's `updateStringProc` if there is not one already.
///
/// Both of Tcl's panics are reproduced as assertions, because both are ways a
/// type gets its contract wrong that are otherwise silent:
///
/// * a NULL `bytes` with no `updateStringProc` means a type that promised never
///   to invalidate its string rep and then did (`generic/tclObj.c:1723-1731`);
/// * a proc that returns without a NUL at `bytes[length]` leaves every later
///   `strlen` reading past the end (`generic/tclObj.c:1733-1738`).
///
/// # Safety
/// `obj` must point at a live `Tcl_Obj`. It may be Tk's stack storage.
pub unsafe fn string_of(obj: *mut TclObj) -> &'static [u8] {
    if (*obj).bytes.is_null() {
        let ty = (*obj).type_ptr;
        assert!(
            !ty.is_null(),
            "value has neither a string rep nor a type; at least one of \
             objPtr->bytes and objPtr->typePtr must be set \
             (generic/tclObj.c:1717-1720)"
        );
        assert!(
            !(*ty).update_string_proc.is_null(),
            "UpdateStringProc should not be invoked for type {} \
             (generic/tclObj.c:1729-1730)",
            objtype::name_of(ty)
        );
        objtype::call_update_string(obj);
        assert!(
            !(*obj).bytes.is_null() && *(*obj).bytes.offset((*obj).length) == 0,
            "UpdateStringProc for type {} failed to create a valid string rep \
             (generic/tclObj.c:1733-1738)",
            objtype::name_of(ty)
        );
    }
    string_bytes_unchecked(obj)
}

/// The string rep as a `String`, for host code that wants to reason about it.
///
/// # Safety
/// As [`string_of`].
pub unsafe fn text_of(obj: *mut TclObj) -> String {
    String::from_utf8_lossy(string_of(obj)).into_owned()
}

/// `Tcl_SetObjLength` (`generic/tclStringObj.c`): set the number of bytes,
/// growing the allocation when it has to.
///
/// Shrinking is what Tk asks for during initialisation
/// (`tk9.0.4/generic/tkWindow.c:3374` truncates a class name), but growing is
/// the documented half of the same call and a `Tcl_DString` handed to
/// `Tcl_DStringToObj` can leave a value that a later grow has to reallocate, so
/// both are here.
///
/// # Safety
/// `obj` must be host storage with a string rep.
pub unsafe fn set_obj_length(obj: *mut TclObj, length: isize) {
    assert!(length >= 0, "Tcl_SetObjLength called with length {length}");
    objtype::free_internal_rep(obj);
    if (*obj).bytes.is_null() {
        init_string_rep(obj, ptr::null(), length as usize);
        (*obj).length = length;
        *(*obj).bytes.offset(length) = 0;
        return;
    }
    if length > (*obj).length {
        let grown = libc::realloc((*obj).bytes as *mut c_void, length as usize + 1) as *mut c_char;
        assert!(!grown.is_null(), "out of memory growing a string rep");
        (*obj).bytes = grown;
    }
    (*obj).length = length;
    *(*obj).bytes.offset(length) = 0;
}

/// Append bytes to the string rep, dropping any internal rep that no longer
/// describes it.
///
/// # Safety
/// `obj` must be host storage.
pub unsafe fn append_bytes(obj: *mut TclObj, add: &[u8]) {
    // The value's meaning is about to change, so a cached internal rep is now a
    // lie. Tcl's own append path does the same thing by going through the
    // string type's growth, which frees any other type first.
    let mut text = if (*obj).bytes.is_null() {
        string_of(obj).to_vec()
    } else {
        string_bytes_unchecked(obj).to_vec()
    };
    objtype::free_internal_rep(obj);
    text.extend_from_slice(add);
    set_string(obj, &text);
}

// ---------------------------------------------------------------------------
// The bridge to this crate's values
// ---------------------------------------------------------------------------

/// A `Tcl_Obj` carrying `value`.
///
/// Every arm produces a value with a valid string rep, and the four that have
/// one also carry the matching internal rep, so that a round trip through Tk
/// does not lose the type. Which type pointer each gets is [`super::objtype`]'s
/// business; the names are Tcl's own (`generic/tclObj.c:227-250`,
/// `generic/tclListObj.c:152`, `generic/tclDictObj.c`), because Tk looks types
/// up by name in one place — `Tcl_GetObjType` behind `GetTypeCache`
/// (`tk9.0.4/generic/tkObj.c:192-209`) — and by pointer everywhere else.
///
/// # Safety
/// The result is pinned host storage with count 0, owned by the caller.
pub unsafe fn from_value(value: &Value) -> *mut TclObj {
    match value {
        Value::Undef => alloc(),
        Value::Bool(b) => objtype::new_boolean(*b),
        Value::Int(i) => objtype::new_wide(*i),
        Value::Status(c) => objtype::new_wide(*c as i64),
        Value::NativeFn(n) => objtype::new_wide(*n as i64),
        Value::Obj(h) => objtype::new_wide(*h as i64),
        Value::Float(f) => objtype::new_double(*f),
        Value::Str(s) => new_string(s.as_bytes()),
        Value::Ref(inner) => from_value(inner),
        Value::Array(items) => {
            let elems: Vec<*mut TclObj> = items.iter().map(|v| from_value(v)).collect();
            objtype::new_list(&elems)
        }
        Value::Hash(map) => {
            // `HashMap` has no order and Tcl's dictionary preserves insertion
            // order, so the pairs are sorted by key: an arbitrary order that at
            // least does not change between two runs over the same map.
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            let pairs: Vec<(*mut TclObj, *mut TclObj)> = keys
                .into_iter()
                .map(|k| (new_string(k.as_bytes()), from_value(&map[k])))
                .collect();
            objtype::new_dict(&pairs)
        }
    }
}

/// The value `obj` carries.
///
/// The internal rep decides, and an untyped value is a string: that is the
/// whole of Tcl's answer to "what type is this", and reproducing it means a
/// value Tk shimmered to one of *its* ten types reads back as its string rep
/// rather than as something this side invented.
///
/// # Safety
/// `obj` must point at a live `Tcl_Obj`. It may be Tk's stack storage: nothing
/// here retains it or touches `refCount`.
pub unsafe fn to_value(obj: *mut TclObj) -> Value {
    let ty = (*obj).type_ptr;
    if ty.is_null() {
        return Value::Str(std::sync::Arc::new(text_of(obj)));
    }
    if objtype::is_list(ty) {
        let elems = objtype::list_of(obj).elems.clone();
        return Value::Array(elems.into_iter().map(|e| to_value(e)).collect());
    }
    if objtype::is_dict(ty) {
        let pairs = objtype::dict_of(obj).pairs.clone();
        let mut map = std::collections::HashMap::new();
        for (k, v) in pairs {
            map.insert(text_of(k), to_value(v));
        }
        return Value::Hash(map);
    }
    if objtype::is_wide(ty) {
        return Value::Int((*obj).internal_rep.ptr1 as i64);
    }
    if objtype::is_boolean(ty) {
        return Value::Bool((*obj).internal_rep.ptr1 as i64 != 0);
    }
    if objtype::is_double(ty) {
        return Value::Float(objtype::double_bits(obj));
    }
    Value::Str(std::sync::Arc::new(text_of(obj)))
}
