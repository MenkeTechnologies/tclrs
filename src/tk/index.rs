//! `Tcl_GetIndexFromObjStruct`, ported from `generic/tclIndexObj.c`.
//!
//! This is the slot Tk asks for once the object layer and the evaluator are
//! both behind the table: every widget option name, every enumerated option
//! value and every `Tk_ConfigureWidget` keyword is resolved through it. It gets
//! a module of its own for the same reason the C gives it a file of its own —
//! it carries a `Tcl_ObjType` (`tclIndexType`) and a private internal rep
//! (`IndexRep`) that exist only to cache one lookup.
//!
//! Three things about the C are worth stating, because none of them is visible
//! in the declaration:
//!
//! 1. **The width of `*indexPtr` arrives in `flags`.** The public name is a
//!    macro that ORs `sizeof(*(indexPtr))<<1` into the flag word
//!    (`generic/tclIndexObj.c:365-366`), and the function masks that back out
//!    with `flags &= (30-(int)(sizeof(int)<<1))` — 22 on every platform where
//!    `int` is 4 bytes — and switches on the remainder
//!    (`generic/tclIndexObj.c:296-313`). So a caller writing into a `short`
//!    and a caller writing into an `int` reach the same slot with different
//!    flags, and a host that ignores the low bits corrupts four bytes of the
//!    caller's stack for every two-byte destination. Tk has both kinds.
//! 2. **The table is not an array of `char *`.** It is an array of *structures*
//!    whose first member is a `char *`, walked with a caller-supplied byte
//!    stride (`generic/tclIndexObj.c:66-71`), so the entry at index `i` is at
//!    `table + offset*i` and the list ends at the first NULL.
//! 3. **A unique abbreviation matches.** An exact hit always wins; failing
//!    that, exactly one prefix match is accepted unless `TCL_EXACT` is set
//!    (`generic/tclIndexObj.c:249-277`).

use std::ffi::{c_char, c_int, c_void, CStr};
use std::ptr;

use super::abi::{RawStub, TclObj, TclObjInternalRep, TclObjType, TclStubs, TCL_ERROR, TCL_OK};
use super::generated::TCL_NAMES;
use super::trace::{record, Table};
use super::{host, obj, objtype};

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

/// `TCL_EXACT` (`generic/tcl.h:948`).
const TCL_EXACT: c_int = 1;
/// `TCL_NULL_OK` (`generic/tcl.h:949`).
const TCL_NULL_OK: c_int = 32;
/// `TCL_INDEX_TEMP_TABLE` (`generic/tcl.h:950`).
const TCL_INDEX_TEMP_TABLE: c_int = 64;
/// `TCL_INDEX_NONE` (`generic/tcl.h:2292`).
const TCL_INDEX_NONE: isize = -1;

/// The mask the C applies before switching on the destination width:
/// `30-(int)(sizeof(int)<<1)` with a 4-byte `int`
/// (`generic/tclIndexObj.c:298`). Written as the expression rather than as
/// `22` so the derivation stays checkable.
const WIDTH_MASK: c_int = 30 - ((size_of::<c_int>() as c_int) << 1);

/// `IndexRep` (`generic/tclIndexObj.c:56-60`), the cached lookup.
///
/// `#[repr(C)]` because the C declares it "keep in sync with tclTestObj.c" and
/// its address is what goes in `internalRep.twoPtrValue.ptr1`; nothing outside
/// this file reads it, but a layout that matches is free and a layout that
/// silently does not is the class of bug this whole tree is about.
#[repr(C)]
struct IndexRep {
    table_ptr: *const c_void,
    offset: isize,
    index: isize,
}

/// `tclIndexType` (`generic/tclIndexObj.c:39-46`).
///
/// Not registered: `TclInitObjSubsystem` registers eight types and this is not
/// one of them (`generic/tclObj.c:378-385`), so `Tcl_GetObjType("index")`
/// answers NULL in real Tcl too and no caller can reach the type by name.
static INDEX_TYPE: TclObjType = TclObjType {
    name: c"index".as_ptr(),
    free_internal_rep_proc: free_index as *const c_void,
    dup_internal_rep_proc: dup_index as *const c_void,
    update_string_proc: update_string_of_index as *const c_void,
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

/// `STRING_AT(table, offset)` (`generic/tclIndexObj.c:66-67`): the `char *` at
/// a byte displacement from the table's start.
///
/// # Safety
/// `table.byte_add(offset)` must be a readable `*const c_char`.
unsafe fn string_at(table: *const c_void, offset: isize) -> *const c_char {
    *(table.byte_offset(offset) as *const *const c_char)
}

/// One table entry's bytes, stopping at the NUL the C's `strcmp` would.
///
/// # Safety
/// `p` must be a NUL-terminated string.
unsafe fn entry_bytes(p: *const c_char) -> &'static [u8] {
    CStr::from_ptr(p).to_bytes()
}

/// `FreeIndex` (`generic/tclIndexObj.c:446-452`).
///
/// # Safety
/// `o` must carry an [`IndexRep`] this file allocated.
unsafe extern "C" fn free_index(o: *mut TclObj) {
    let rep = (*o).internal_rep.ptr1 as *mut IndexRep;
    if !rep.is_null() {
        drop(Box::from_raw(rep));
    }
    (*o).type_ptr = ptr::null();
}

/// `DupIndex` (`generic/tclIndexObj.c:414-427`): a fresh `IndexRep` holding a
/// copy, because the original is owned by the source value.
///
/// # Safety
/// `src` must carry an [`IndexRep`]; `dup` must be fresh host storage.
unsafe extern "C" fn dup_index(src: *mut TclObj, dup: *mut TclObj) {
    let from = (*src).internal_rep.ptr1 as *const IndexRep;
    let copy = Box::into_raw(Box::new(IndexRep {
        table_ptr: (*from).table_ptr,
        offset: (*from).offset,
        index: (*from).index,
    }));
    let ir = TclObjInternalRep {
        ptr1: copy as *mut c_void,
        ptr2: ptr::null_mut(),
    };
    objtype::store_internal_rep(dup, &INDEX_TYPE, &ir);
}

/// `UpdateStringOfIndex` (`generic/tclIndexObj.c:385-393`): the table entry the
/// index selects, in full — "no abbreviation is ever generated"
/// (`generic/tclIndexObj.c:373-374`), which is what makes the cache safe to
/// keep on a value whose string rep a caller may later read back.
///
/// # Safety
/// `o` must carry an [`IndexRep`] whose table is still live.
unsafe extern "C" fn update_string_of_index(o: *mut TclObj) {
    let rep = &*((*o).internal_rep.ptr1 as *const IndexRep);
    // `EXPAND_OF` (`generic/tclIndexObj.c:70-71`).
    let text: &[u8] = if rep.index != TCL_INDEX_NONE {
        entry_bytes(string_at(rep.table_ptr, rep.offset * rep.index))
    } else {
        b""
    };
    obj::init_string_rep(o, text.as_ptr() as *const c_char, text.len());
}

/// Slot 304: `int Tcl_GetIndexFromObjStruct(Tcl_Interp *, Tcl_Obj *,
/// const void *, Tcl_Size, const char *, int, void *)`
/// (`generic/tclIndexObj.c:180-193`).
///
/// # Safety
/// The arguments must be what `tclDecls.h` declares. `table` must be a
/// NULL-terminated table of structures whose first member is a `const char *`,
/// spaced `offset` bytes apart, and it must outlive any value this caches the
/// lookup on — which is Tcl's own requirement, since the cached `IndexRep`
/// holds the bare pointer.
pub unsafe extern "C" fn get_index_from_obj_struct(
    interp: *mut c_void,
    o: *mut TclObj,
    table: *const c_void,
    offset: isize,
    msg: *const c_char,
    flags: c_int,
    index_out: *mut c_void,
) -> c_int {
    entered!("tcl_GetIndexFromObjStruct");

    // `generic/tclIndexObj.c:203-210`. A stride smaller than the pointer it is
    // meant to step over cannot describe any table, so this is a caller bug and
    // is reported as one rather than walked.
    if offset < size_of::<*const c_char>() as isize {
        if !interp.is_null() {
            host::set_result_bytes(
                interp,
                format!("Invalid struct offset value {offset}.").as_bytes(),
            );
        }
        return TCL_ERROR;
    }

    let cacheable = !o.is_null() && (flags & TCL_INDEX_TEMP_TABLE) == 0;

    // A valid cached result from a previous lookup
    // (`generic/tclIndexObj.c:215-227`).
    let mut index = TCL_INDEX_NONE;
    let mut cached = false;
    if cacheable {
        let ir = objtype::fetch_internal_rep(o, &INDEX_TYPE);
        if !ir.is_null() {
            let rep = &*((*ir).ptr1 as *const IndexRep);
            if rep.table_ptr == table && rep.offset == offset && rep.index != TCL_INDEX_NONE {
                index = rep.index;
                cached = true;
            }
        }
    }

    if !cached {
        // `key = objPtr ? TclGetString(objPtr) : ""`
        // (`generic/tclIndexObj.c:232`). The C reads it as a C string, so a
        // value carrying an embedded NUL is compared only up to it.
        let key: &[u8] = if o.is_null() {
            b""
        } else {
            let all = obj::string_of(o);
            match all.iter().position(|b| *b == 0) {
                Some(n) => &all[..n],
                None => all,
            }
        };

        // `generic/tclIndexObj.c:236-238`: an empty value with `TCL_NULL_OK`
        // is not an error, and leaves the index as TCL_INDEX_NONE.
        if !(key.is_empty() && (flags & TCL_NULL_OK) != 0) {
            // The scan (`generic/tclIndexObj.c:246-269`): an exact match always
            // wins; a single prefix match is an abbreviation; more than one is
            // ambiguous unless something matched exactly.
            let mut num_abbrev = 0usize;
            let mut idx: isize = 0;
            let mut exact = false;
            loop {
                let entry = string_at(table, offset * idx);
                if entry.is_null() {
                    break;
                }
                let bytes = entry_bytes(entry);
                if bytes == key {
                    index = idx;
                    exact = true;
                    break;
                }
                if bytes.starts_with(key) {
                    num_abbrev += 1;
                    index = idx;
                }
                idx += 1;
            }

            // `generic/tclIndexObj.c:275-277`.
            if !exact && ((flags & TCL_EXACT) != 0 || key.is_empty() || num_abbrev != 1) {
                return error(interp, table, offset, msg, key, num_abbrev, flags);
            }
        }

        // Cache the found representation (`generic/tclIndexObj.c:285-297`).
        if cacheable && index != TCL_INDEX_NONE {
            let ir = objtype::fetch_internal_rep(o, &INDEX_TYPE);
            if ir.is_null() {
                let rep = Box::into_raw(Box::new(IndexRep {
                    table_ptr: table,
                    offset,
                    index,
                }));
                let new = TclObjInternalRep {
                    ptr1: rep as *mut c_void,
                    ptr2: ptr::null_mut(),
                };
                objtype::store_internal_rep(o, &INDEX_TYPE, &new);
            } else {
                let rep = &mut *((*ir).ptr1 as *mut IndexRep);
                rep.table_ptr = table;
                rep.offset = offset;
                rep.index = index;
            }
        }
    }

    // `uncachedDone` (`generic/tclIndexObj.c:299-317`). The destination's width
    // came in the flag word; anything the mask leaves that is not one of the
    // three recognised encodings falls through to `int`, exactly as the C does.
    if !index_out.is_null() {
        match flags & WIDTH_MASK {
            w if w == (size_of::<u16>() as c_int) << 1 => *(index_out as *mut u16) = index as u16,
            w if w == (size_of::<u8>() as c_int) << 1 => *(index_out as *mut u8) = index as u8,
            w if w == (size_of::<i64>() as c_int) << 1 => *(index_out as *mut i64) = index as i64,
            w if w == (size_of::<i32>() as c_int) << 1 => *(index_out as *mut i32) = index as i32,
            _ => *(index_out as *mut c_int) = index as c_int,
        }
    }
    TCL_OK
}

/// The `error:` label (`generic/tclIndexObj.c:320-362`).
///
/// The C builds the message with `Tcl_AppendStringsToObj` and leaves it as the
/// interpreter result; this builds the same bytes and sets the same result. The
/// wording is load-bearing — Tk's own test suite matches on it — so it is
/// reproduced clause for clause: entries with an empty name are skipped at the
/// head, listed with a comma in the middle, and the last is introduced by
/// " or ".
///
/// # Safety
/// As [`get_index_from_obj_struct`].
unsafe fn error(
    interp: *mut c_void,
    table: *const c_void,
    offset: isize,
    msg: *const c_char,
    key: &[u8],
    num_abbrev: usize,
    flags: c_int,
) -> c_int {
    if interp.is_null() {
        return TCL_ERROR;
    }
    let what = if msg.is_null() {
        Vec::new()
    } else {
        entry_bytes(msg).to_vec()
    };

    // `generic/tclIndexObj.c:329-332`: skip the leading entries whose name is
    // the empty string, which is how a table marks a hidden option.
    let mut idx: isize = 0;
    loop {
        let e = string_at(table, offset * idx);
        if e.is_null() || !entry_bytes(e).is_empty() {
            break;
        }
        idx += 1;
    }

    let mut out: Vec<u8> = Vec::new();
    // `generic/tclIndexObj.c:333-335`.
    out.extend_from_slice(if num_abbrev > 1 && (flags & TCL_EXACT) == 0 {
        b"ambiguous ".as_slice()
    } else {
        b"bad ".as_slice()
    });
    out.extend_from_slice(&what);
    out.extend_from_slice(b" \"");
    out.extend_from_slice(key);

    if string_at(table, offset * idx).is_null() {
        // `generic/tclIndexObj.c:336-337`.
        out.extend_from_slice(b"\": no valid options");
    } else {
        // `generic/tclIndexObj.c:338-341`.
        out.extend_from_slice(b"\": must be ");
        out.extend_from_slice(entry_bytes(string_at(table, offset * idx)));
        idx += 1;
        let mut count = 0usize;
        loop {
            let entry = string_at(table, offset * idx);
            if entry.is_null() {
                break;
            }
            let bytes = entry_bytes(entry);
            let last = string_at(table, offset * (idx + 1)).is_null();
            // `generic/tclIndexObj.c:343-351`.
            if last && (flags & TCL_NULL_OK) == 0 {
                if count > 0 {
                    out.extend_from_slice(b",");
                }
                out.extend_from_slice(b" or ");
                out.extend_from_slice(bytes);
            } else if !bytes.is_empty() {
                out.extend_from_slice(b", ");
                out.extend_from_slice(bytes);
                count += 1;
            }
            idx += 1;
        }
        // `generic/tclIndexObj.c:352-354`.
        if (flags & TCL_NULL_OK) != 0 {
            out.extend_from_slice(b", or \"\"");
        }
    }

    host::set_result_bytes(interp, &out);
    TCL_ERROR
}

/// Patch this module's slot into `t`, returning its index.
///
/// One slot, not two. `Tcl_GetIndexFromObj` is not in the table at all in Tcl
/// 9: it is a macro over this function with `sizeof(char *)` as the stride
/// (`generic/tclDecls.h:3939-3941`), so a caller that writes
/// `Tcl_GetIndexFromObj` arrives here.
///
/// # Safety
/// The erased signature is the one `tclDecls.h` gives the slot, quoted on the
/// line above it.
pub unsafe fn install_impls(t: &mut TclStubs) -> Vec<usize> {
    vec![
        // int (*tcl_GetIndexFromObjStruct)(Tcl_Interp *, Tcl_Obj *, const void *,
        //     Tcl_Size, const char *, int, void *) /* 304 */
        install(
            t,
            "tcl_GetIndexFromObjStruct",
            get_index_from_obj_struct as *const (),
        ),
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
