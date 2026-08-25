//! The object layer Tk sees: layout, ownership, shimmering and the type
//! contract.
//!
//! Two kinds of test are here, and the first kind is the point.
//!
//! A layout assertion written as `offset_of!(TclObj, type_ptr) == 24` only
//! restates the Rust declaration; if the declaration is what drifted, it drifts
//! with it. The tests below instead reproduce *what Tk's compiled code does* —
//! a `++` at byte 0, a 16-byte copy from byte 24, a function-pointer call
//! through byte 80 — and then check the Rust view agrees. A wrong offset makes
//! those disagree, which is a test failure rather than the silent corruption it
//! would otherwise be at run time.
//!
//! The second kind exercises the contract: that a value built as a string can
//! be consumed as a list, that a type's `freeIntRepProc` runs when Tk shimmers
//! the value out from under it, and that a `Tcl_Obj` Tk built on its own stack
//! never reaches the free path.
//!
//! None of these needs Tk installed. The last test does, and skips itself when
//! the dylib is missing.

#![cfg(feature = "tk")]

use std::ffi::{c_int, c_void};
use std::mem::{offset_of, size_of};
use std::ptr;

use fusevm::Value;
use tclrs::tk::abi::*;
use tclrs::tk::{dstring, hash, obj, objtype};

// ---------------------------------------------------------------------------
// What Tk's compiled code does to a Tcl_Obj
// ---------------------------------------------------------------------------

/// `Tcl_IncrRefCount(objPtr)` is `((void)++(objPtr)->refCount)`
/// (`generic/tcl.h:2517-2519`) — an increment of a `Tcl_Size` at byte 0 of the
/// object, compiled into Tk and unreachable from any stub table.
///
/// This reproduces it as a raw write and checks that the Rust field is the one
/// that moved. Move `ref_count` and this fails; leave it and nothing else in
/// the crate would notice until Tk corrupted `bytes` instead.
#[test]
fn the_incr_ref_count_macro_lands_on_byte_zero() {
    unsafe {
        let o = obj::new_string(b"x");
        assert_eq!((*o).ref_count, 0);

        let as_bytes = o as *mut u8;
        let count = as_bytes as *mut isize;
        *count += 1;
        assert_eq!((*o).ref_count, 1, "++ at byte 0 did not reach ref_count");

        // And nothing else moved: `bytes` at 8, `length` at 16.
        assert_eq!(*(as_bytes.add(8) as *const usize), (*o).bytes as usize);
        assert_eq!(*(as_bytes.add(16) as *const isize), 1);

        *count -= 1;
        obj::free_obj(o);
    }
}

/// `_DupBorderObjProc` copies `typePtr` and `internalRep.twoPtrValue.ptr1` with
/// one 16-byte load and store from byte 0x18 — `ldur q0,[x0,#0x18]` /
/// `stur q0,[x1,#0x18]` — because the two fields are adjacent
/// (`generic/tcl.h:759-764`) and the compiler merged them.
///
/// The bytes at 24..40 must therefore be exactly `typePtr` followed by the
/// first half of the internal rep. This does that copy by hand and checks both
/// ends.
#[test]
fn a_sixteen_byte_copy_from_byte_24_moves_type_ptr_and_ptr1() {
    unsafe {
        let src = obj::new_string(b"src");
        let dst = obj::new_string(b"dst");
        (*src).type_ptr = &objtype::LIST_TYPE;
        (*src).internal_rep.ptr1 = 0xabcd_ef01 as *mut c_void;
        (*src).internal_rep.ptr2 = 0x1234_5678 as *mut c_void;

        ptr::copy_nonoverlapping((src as *const u8).add(24), (dst as *mut u8).add(24), 16);

        assert!(
            std::ptr::eq((*dst).type_ptr, &objtype::LIST_TYPE),
            "byte 24 is not typePtr"
        );
        assert_eq!(
            (*dst).internal_rep.ptr1 as usize,
            0xabcd_ef01,
            "byte 32 is not internalRep.twoPtrValue.ptr1"
        );
        assert!(
            (*dst).internal_rep.ptr2.is_null(),
            "the 16-byte copy reached ptr2, so the two fields are not where the \
             disassembly says"
        );
        // `dst`'s own string rep is untouched by a copy that starts at 24.
        assert_eq!(obj::string_of(dst), b"dst");

        (*src).type_ptr = ptr::null();
        (*dst).type_ptr = ptr::null();
        obj::free_obj(src);
        obj::free_obj(dst);
    }
}

/// `TCL_OBJTYPE_V1(a)` stores `offsetof(Tcl_ObjType, indexProc)` in `version`
/// (`generic/tcl.h:703-704`).
///
/// That makes the number an external witness for this struct's layout, and it
/// is one that was *measured*: `tk-probe` with `TCLRS_TK_EXERCISE_TYPES=1`
/// reads `version` straight out of Tk's own `color`, `cursor`, `mm` and `pixel`
/// tables and prints 56 for each. If `TclObjType` here grew, shrank or
/// reordered a field, that read would produce something else — so pinning 56
/// pins the whole prefix.
#[test]
fn objtype_v1_version_is_the_offset_of_index_proc() {
    assert_eq!(offset_of!(TclObjType, index_proc), 56);
    assert_eq!(offset_of!(TclObjType, name), 0);
    assert_eq!(offset_of!(TclObjType, free_internal_rep_proc), 8);
    assert_eq!(offset_of!(TclObjType, dup_internal_rep_proc), 16);
    assert_eq!(offset_of!(TclObjType, update_string_proc), 24);
    assert_eq!(offset_of!(TclObjType, set_from_any_proc), 32);
    assert_eq!(offset_of!(TclObjType, version), 40);
    assert_eq!(size_of::<TclObjType>(), 112);
}

/// `Tcl_DStringValue` and `Tcl_DStringLength` are `(dsPtr)->string` and
/// `(dsPtr)->length` (`generic/tcl.h:892-893`) — field reads Tk compiles in, so
/// bytes 0 and 8 have to be those two fields and nothing else.
#[test]
fn the_dstring_value_and_length_macros_are_reads_at_bytes_0_and_8() {
    unsafe {
        let mut ds: TclDString = std::mem::zeroed();
        dstring::init(&mut ds);
        dstring::append(&mut ds, c"hello".as_ptr(), -1);

        let raw = &mut ds as *mut TclDString as *mut u8;
        let value = *(raw as *const *const u8);
        let length = *(raw.add(8) as *const isize);

        assert_eq!(length, 5, "byte 8 is not Tcl_DString::length");
        assert_eq!(
            std::slice::from_raw_parts(value, 5),
            b"hello",
            "byte 0 is not Tcl_DString::string"
        );
        // Still inside the caller's own staticSpace at this size, which is what
        // `Tcl_DStringFree` distinguishes by pointer (`generic/tclUtil.c:2912`).
        assert_eq!(value as usize, ds.static_space.as_ptr() as usize);
        dstring::free(&mut ds);
    }
}

/// `Tcl_FindHashEntry(tablePtr, key)` is `(*((tablePtr)->findProc))(...)`
/// (`generic/tcl.h:2607-2608`): Tk loads a function pointer out of byte 80 of
/// its own struct and calls it.
///
/// This reproduces that load and call rather than going through the Rust field,
/// so a `findProc` that moved is a failed lookup here instead of a jump into
/// whatever a Tk struct happens to hold at byte 80.
#[test]
fn the_find_hash_entry_macro_calls_through_byte_80() {
    unsafe {
        let mut table: TclHashTable = std::mem::zeroed();
        hash::init(&mut table, TCL_STRING_KEYS);

        let mut is_new: c_int = 0;
        let created = (table.create_proc.unwrap())(&mut table, c"widget".as_ptr(), &mut is_new);
        assert_eq!(is_new, 1);
        (*created).client_data = 0x5a5a as *mut c_void;

        type FindProc =
            unsafe extern "C" fn(*mut TclHashTable, *const std::ffi::c_char) -> *mut TclHashEntry;
        let raw = &mut table as *mut TclHashTable as *mut u8;
        let find: FindProc = std::mem::transmute(*(raw.add(80) as *const *const c_void));
        let found = find(&mut table, c"widget".as_ptr());

        assert_eq!(found, created, "byte 80 is not Tcl_HashTable::findProc");
        // `Tcl_GetHashValue(h)` is `(h)->clientData` (`generic/tcl.h:2594`),
        // byte 24 of the entry.
        let entry_bytes = found as *const u8;
        assert_eq!(
            *(entry_bytes.add(24) as *const usize),
            0x5a5a,
            "byte 24 of Tcl_HashEntry is not clientData"
        );
        hash::delete_table(&mut table);
    }
}

// ---------------------------------------------------------------------------
// Ownership
// ---------------------------------------------------------------------------

/// The free path is only for objects this side allocated.
///
/// Tk builds `Tcl_Obj`s on its own C stack — `TkpScanWindowId` at
/// `tk9.0.4/macosx/tkMacOSXEmbed.c:160-165` and `GetTypeCache` at
/// `tk9.0.4/generic/tkObj.c:201-206` — and freeing one of those would be a wild
/// `free` on a stack address. The assertion is what makes that loud.
#[test]
#[should_panic(expected = "this side never allocated")]
fn the_free_path_refuses_a_caller_owned_object() {
    unsafe {
        let mut stack = TclObj {
            ref_count: 0,
            bytes: c"0.0".as_ptr() as *mut std::ffi::c_char,
            length: 3,
            type_ptr: ptr::null(),
            internal_rep: TclObjInternalRep {
                ptr1: ptr::null_mut(),
                ptr2: ptr::null_mut(),
            },
        };
        assert!(!obj::is_host_allocated(&stack));
        obj::free_obj(&mut stack);
    }
}

/// `Tcl_DecrRefCount` is `if (_objPtr->refCount-- <= 1) { TclFreeObj(_objPtr); }`
/// (`generic/tcl.h:2524-2531`): the count is written back *before* the call, so
/// `TclFreeObj` always sees zero or less.
///
/// Reproducing the macro's exact arithmetic is what checks that this side's
/// entry assertion is the right one — an off-by-one in either direction fails.
#[test]
fn the_decr_ref_count_macro_reaches_the_free_path_at_zero() {
    unsafe {
        let o = obj::new_string(b"counted");
        let count = o as *mut isize;
        *count += 1;
        let serial = obj::serial_of(o).expect("a freshly allocated object is live");

        // The macro, verbatim: post-decrement, then free if the old value was
        // 1 or less.
        let old = *count;
        *count -= 1;
        assert_eq!(old, 1);
        obj::free_obj(o);

        // By serial, not by address: an address is only an identity while the
        // object is live, and this assertion is about after.
        assert_ne!(
            obj::serial_of(o),
            Some(serial),
            "the freed object is still in the live set"
        );
    }
}

/// A nested value releases everything it owns, and the accounting says so.
///
/// Every object is followed by its serial number ([`obj::serial_of`]) rather
/// than by its address, because the claim is about *these ten objects* and an
/// address stops naming an object the moment it is freed: the allocator hands
/// the block out again, and this test runs beside others that allocate. Asking
/// `is_host_allocated` after the free asks "is anything live here", which a
/// concurrent allocation answers yes to, for a reason that has nothing to do
/// with what is being tested. Asking for the serial asks "is *that* object
/// still live", which nothing else in the process can make true again.
///
/// The counters are process-wide, so those two assertions stay one-sided.
#[test]
fn freeing_a_nested_value_releases_every_object_under_it() {
    unsafe {
        let (created_before, freed_before, _) = obj::counts();

        let inner: Vec<*mut TclObj> = (0..8)
            .map(|i| obj::new_string(format!("element{i}").as_bytes()))
            .collect();
        let list = objtype::new_list(&inner);
        let dict = objtype::new_dict(&[(inner[0], list)]);
        let owned: Vec<*mut TclObj> = inner.iter().copied().chain([list, dict]).collect();
        // The ten identities this test is about, taken while all ten are live.
        // Distinct by construction: a serial is issued once per process.
        let serials: Vec<u64> = owned
            .iter()
            .map(|p| obj::serial_of(*p).expect("a freshly allocated object is live"))
            .collect();
        let mut unique = serials.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), 10, "two objects were issued the same serial");

        obj::free_obj(dict);

        for (i, (p, serial)) in owned.iter().zip(&serials).enumerate() {
            assert_ne!(
                obj::serial_of(*p),
                Some(*serial),
                "object {i} outlived the dictionary that owned the list that \
                 owned it: it is still live at {p:?} under serial {serial}"
            );
        }

        let (created_after, freed_after, _) = obj::counts();
        assert!(created_after - created_before >= 10);
        assert!(
            freed_after - freed_before >= 10,
            "freed only {} of the 10 objects this test allocated",
            freed_after - freed_before
        );
    }
}

/// A read path may not touch `refCount`, because `GetTypeCache` never
/// initialises it: it sets `length`, `bytes` and `typePtr` on a stack
/// `Tcl_Obj` and passes it straight to `Tcl_GetDoubleFromObj`
/// (`tk9.0.4/generic/tkObj.c:198-207`).
///
/// This reproduces that call, with a poison value in `refCount` that the
/// conversion must leave alone, and then checks the two things Tk goes on to
/// use: a non-NULL `typePtr` to cache, and the double itself.
#[test]
fn the_double_conversion_works_on_tks_stack_object_without_reading_ref_count() {
    unsafe {
        let mut stack = TclObj {
            // Deliberately not what a live object would hold: Tk leaves this
            // field uninitialised, so anything that reads it is wrong.
            ref_count: isize::MIN,
            bytes: c"0.0".as_ptr() as *mut std::ffi::c_char,
            length: 3,
            type_ptr: ptr::null(),
            internal_rep: TclObjInternalRep {
                ptr1: ptr::null_mut(),
                ptr2: ptr::null_mut(),
            },
        };

        let got = objtype::double_of(&mut stack).expect("\"0.0\" is a double");
        assert_eq!(got, 0.0);
        assert_eq!(
            stack.ref_count,
            isize::MIN,
            "the conversion touched refCount on an object that has none"
        );
        assert!(
            !stack.type_ptr.is_null(),
            "GetTypeCache caches whatever typePtr this leaves; NULL would make \
             Tk read every untyped value as a double \
             (tk9.0.4/generic/tkObj.c:206)"
        );
        assert!(objtype::is_double(stack.type_ptr));
        // The conversion stores the value in the internal rep and never frees
        // `bytes`, which here points at a string literal this side does not own.
        assert_eq!(objtype::double_bits(&mut stack), 0.0);
        assert_eq!(stack.bytes as usize, c"0.0".as_ptr() as usize);
    }
}

// ---------------------------------------------------------------------------
// Shimmering
// ---------------------------------------------------------------------------

/// The conversion Tk's own initialisation depends on: `Initialize` builds the
/// command that creates the main window with
/// `Tcl_NewStringObj("toplevel . -class", TCL_INDEX_NONE)` and then calls
/// `Tcl_ListObjAppendElement` on it (`tk9.0.4/generic/tkWindow.c:3382-3384`).
#[test]
fn a_value_built_as_a_string_is_consumed_as_a_list() {
    unsafe {
        let command = obj::new_string(b"toplevel . -class");
        let class = obj::new_string(b"Tclrs");

        let list = objtype::list_of(command);
        list.elems.push(class);
        obj::incr_ref(class);
        objtype::invalidate(command);

        assert_eq!(objtype::list_of(command).elems.len(), 4);
        assert_eq!(obj::text_of(objtype::list_of(command).elems[0]), "toplevel");
        assert_eq!(obj::text_of(objtype::list_of(command).elems[3]), "Tclrs");

        // And back the other way: the string rep was dropped when the list
        // changed, so reading it now runs the list type's updateStringProc.
        assert!(!obj::has_string_rep(command));
        assert_eq!(obj::text_of(command), "toplevel . -class Tclrs");

        obj::free_obj(command);
    }
}

/// The other direction, on its own: a list built through the object API has no
/// string rep until one is asked for, and the one it produces is a canonical
/// Tcl list — braces and all.
#[test]
fn a_list_regenerates_its_string_through_its_update_string_proc() {
    unsafe {
        let a = obj::new_string(b"plain");
        let b = obj::new_string(b"needs quoting");
        let c = obj::new_string(b"");
        let list = objtype::new_list(&[a, b, c]);

        assert!(
            !obj::has_string_rep(list),
            "a list built from elements starts with no string rep, as \
             Tcl_NewListObj's does"
        );
        assert_eq!(obj::text_of(list), "plain {needs quoting} {}");

        // Round trip: that text read back as a list is the same three elements.
        let reparsed = obj::new_string(obj::text_of(list).as_bytes());
        let elems = &objtype::list_of(reparsed).elems;
        assert_eq!(elems.len(), 3);
        assert_eq!(obj::text_of(elems[1]), "needs quoting");
        assert_eq!(obj::text_of(elems[2]), "");

        obj::free_obj(list);
        obj::free_obj(reparsed);
    }
}

/// A dictionary shimmers the same way, which is what `Tcl_DictObjPut` on a
/// value that started as `Tcl_NewObj()` needs
/// (`tk9.0.4/generic/tkUtil.c:1215-1223`).
#[test]
fn a_dictionary_shimmers_in_both_directions() {
    unsafe {
        let from_string = obj::new_string(b"a 1 b 2");
        let pairs = &objtype::dict_of(from_string).pairs;
        assert_eq!(pairs.len(), 2);
        assert_eq!(obj::text_of(pairs[0].0), "a");
        assert_eq!(obj::text_of(pairs[1].1), "2");

        objtype::invalidate(from_string);
        assert_eq!(obj::text_of(from_string), "a 1 b 2");
        obj::free_obj(from_string);
    }
}

// ---------------------------------------------------------------------------
// The Tcl_ObjType contract
// ---------------------------------------------------------------------------

/// How many times [`FOREIGN_TYPE`]'s procs have run. A stand-in for the
/// resource refcounts Tk's own procs maintain.
static FOREIGN_FREES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static FOREIGN_DUPS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

unsafe extern "C" fn foreign_free(o: *mut TclObj) {
    FOREIGN_FREES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // Exactly what `FreeBorderObjProc` does (`tk9.0.4/generic/tk3d.c:518-523`).
    (*o).internal_rep.ptr1 = ptr::null_mut();
    (*o).type_ptr = ptr::null();
}

unsafe extern "C" fn foreign_dup(src: *mut TclObj, dup: *mut TclObj) {
    FOREIGN_DUPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // `DupBorderObjProc` writes typePtr and ptr1 and leaves ptr2 alone
    // (`tk9.0.4/generic/tk3d.c:560-572`).
    (*dup).type_ptr = (*src).type_ptr;
    (*dup).internal_rep.ptr1 = (*src).internal_rep.ptr1;
}

/// A type shaped exactly like Tk's ten: a free proc, a dup proc, and NULL for
/// the other two (`tk9.0.4/generic/tk3d.c:49-57`).
static FOREIGN_TYPE: TclObjType = TclObjType {
    name: c"border".as_ptr(),
    free_internal_rep_proc: foreign_free as *const c_void,
    dup_internal_rep_proc: foreign_dup as *const c_void,
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

/// The constraint that decides the whole design of the host types: when Tk
/// converts a value to one of its own, it calls the *current* type's
/// `freeIntRepProc` directly and then overwrites `typePtr` itself.
///
/// `InitBorderObj` (`tk9.0.4/generic/tk3d.c:1331-1348`) is reproduced here step
/// for step against a value this side made a list. If the host list type had
/// the NULL `freeIntRepProc` a first sketch would give it, the elements'
/// references would never be dropped and the `HostList` box would leak — every
/// time, silently.
#[test]
fn tk_shimmering_a_host_list_runs_the_list_types_free_proc() {
    unsafe {
        let element = obj::new_string(b"element");
        let list = objtype::new_list(&[element]);
        let serial = obj::serial_of(element).expect("a freshly allocated object is live");

        // InitBorderObj, verbatim.
        obj::string_of(list); // Tcl_GetString(objPtr)
        let ty = (*list).type_ptr;
        assert!(!ty.is_null() && !(*ty).free_internal_rep_proc.is_null());
        let free: unsafe extern "C" fn(*mut TclObj) =
            std::mem::transmute((*ty).free_internal_rep_proc);
        free(list);
        (*list).type_ptr = &FOREIGN_TYPE;
        (*list).internal_rep.ptr1 = ptr::null_mut();

        // By serial, not by address, for the reason
        // `freeing_a_nested_value_releases_every_object_under_it` gives: after
        // the free the address may belong to a concurrent test's object, and
        // that says nothing about this element.
        assert_ne!(
            obj::serial_of(element),
            Some(serial),
            "the list's freeIntRepProc did not drop its element's reference, so \
             shimmering to a Tk type leaks"
        );
        obj::free_obj(list);
    }
}

/// `Tcl_DuplicateObj` with a type whose `dupIntRepProc` writes only part of the
/// union (`generic/tclObj.c:1548-1555`).
///
/// The destination's `ptr2` has to already be safe, because Tk's dup procs do
/// not all write it. That is what makes zeroing the union in the allocator a
/// correctness requirement rather than tidiness.
#[test]
fn a_foreign_dup_proc_finds_a_destination_whose_union_is_already_safe() {
    unsafe {
        FOREIGN_DUPS.store(0, std::sync::atomic::Ordering::Relaxed);
        let src = obj::new_string(b"#d9d9d9");
        (*src).type_ptr = &FOREIGN_TYPE;
        (*src).internal_rep.ptr1 = 0x1111 as *mut c_void;
        (*src).internal_rep.ptr2 = 0x2222 as *mut c_void;

        let dup = obj::duplicate(src);
        assert_eq!(FOREIGN_DUPS.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert!(std::ptr::eq((*dup).type_ptr, &FOREIGN_TYPE));
        assert_eq!((*dup).internal_rep.ptr1 as usize, 0x1111);
        assert!(
            (*dup).internal_rep.ptr2.is_null(),
            "the duplicate's ptr2 held stale bytes the dup proc never wrote"
        );
        assert_eq!(obj::string_of(dup), b"#d9d9d9");

        (*src).type_ptr = ptr::null();
        obj::free_obj(src);
        obj::free_obj(dup);
    }
}

/// Not one of Tk's twelve object types defines a `setFromAnyProc`, so
/// `Tcl_ConvertToType` to any of them is an error by construction
/// (`generic/tclObj.c:947-954`). The host types do define one, and convert.
#[test]
fn convert_to_type_refuses_a_type_with_no_set_from_any_proc() {
    unsafe {
        let o = obj::new_string(b"1 2 3");
        assert_eq!(
            objtype::convert_to_type(ptr::null_mut(), o, &FOREIGN_TYPE),
            TCL_ERROR,
            "a type with no setFromAnyProc cannot be converted to"
        );
        assert!((*o).type_ptr.is_null());

        assert_eq!(
            objtype::convert_to_type(ptr::null_mut(), o, &objtype::LIST_TYPE),
            TCL_OK
        );
        assert!(objtype::is_list((*o).type_ptr));
        assert_eq!(objtype::list_of(o).elems.len(), 3);

        // Converting to the type it already has is a no-op that succeeds
        // (`generic/tclObj.c:937-939`).
        assert_eq!(
            objtype::convert_to_type(ptr::null_mut(), o, &objtype::LIST_TYPE),
            TCL_OK
        );
        obj::free_obj(o);
    }
}

/// `Tcl_RegisterObjType` replaces an entry of the same name
/// (`generic/tclObj.c:799-801, 814-817`), and `Tcl_GetObjType` answers by name
/// (`generic/tclObj.c:895-909`).
#[test]
fn the_type_registry_answers_by_name_and_replaces_by_name() {
    unsafe {
        objtype::register_host_types();
        assert!(objtype::is_double(objtype::lookup("double")));
        assert!(objtype::is_list(objtype::lookup("list")));
        assert!(objtype::lookup("no-such-type").is_null());

        let before = objtype::registered_count();
        // Same name as a host type: a replacement, not an addition.
        objtype::register(&FOREIGN_TYPE);
        assert_eq!(objtype::registered_count(), before + 1);
        assert!(std::ptr::eq(objtype::lookup("border"), &FOREIGN_TYPE));
        objtype::register(&FOREIGN_TYPE);
        assert_eq!(objtype::registered_count(), before + 1);
    }
}

/// A type with no `updateStringProc` must never let `bytes` go NULL; Tcl panics
/// otherwise (`generic/tclObj.c:1723-1731`), and this side asserts.
#[test]
#[should_panic(expected = "UpdateStringProc should not be invoked")]
fn a_value_with_no_string_rep_and_no_update_proc_is_a_named_failure() {
    unsafe {
        let o = obj::new_string(b"whatever");
        (*o).type_ptr = &FOREIGN_TYPE;
        obj::invalidate_string_rep(o);
        obj::string_of(o);
    }
}

// ---------------------------------------------------------------------------
// The bridge to this crate's values
// ---------------------------------------------------------------------------

/// Every arm of the bridge, out and back.
#[test]
fn values_round_trip_through_the_shadow_object() {
    unsafe {
        let cases = [
            Value::Int(-7),
            Value::Float(1.5),
            Value::Bool(true),
            Value::Bool(false),
            Value::Str(std::sync::Arc::new("hello world".to_string())),
            Value::Array(std::sync::Arc::new(vec![
                Value::Int(1),
                Value::Str(std::sync::Arc::new("two".into())),
            ])),
        ];
        for want in cases {
            let o = obj::from_value(&want);
            let got = obj::to_value(o);
            assert_eq!(got, want, "round trip changed the value");
            obj::free_obj(o);
        }
    }
}

/// The string rep each arm carries, which is what Tk reads when it does not
/// care about the type.
#[test]
fn the_bridge_gives_every_value_a_string_rep_tk_can_read() {
    unsafe {
        for (value, text) in [
            (Value::Undef, ""),
            (Value::Int(42), "42"),
            (Value::Float(1.0), "1.0"),
            (Value::Bool(true), "1"),
            (Value::Status(3), "3"),
        ] {
            let o = obj::from_value(&value);
            assert_eq!(obj::text_of(o), text, "{value:?} stringified wrongly");
            obj::free_obj(o);
        }
        // A list's string rep is built on demand, and is a canonical Tcl list.
        let o = obj::from_value(&Value::Array(std::sync::Arc::new(vec![
            Value::Str(std::sync::Arc::new("a b".to_string())),
            Value::Int(2),
        ])));
        assert_eq!(obj::text_of(o), "{a b} 2");
        obj::free_obj(o);
    }
}

// ---------------------------------------------------------------------------
// Tcl_DString
// ---------------------------------------------------------------------------

/// The append path has to survive a source that points into the dstring's own
/// buffer across a reallocation — Tcl ticket 16896d49fd, fixed at
/// `generic/tclUtil.c:2664-2676`. The obvious implementation frees the old
/// buffer and then copies from it.
#[test]
fn appending_a_dstring_to_itself_survives_the_growth() {
    unsafe {
        let mut ds: TclDString = std::mem::zeroed();
        dstring::init(&mut ds);
        // 150 bytes, so that one self-append crosses TCL_DSTRING_STATIC_SIZE.
        let seed = "x".repeat(150);
        dstring::append(&mut ds, seed.as_ptr() as *const std::ffi::c_char, 150);
        assert_eq!(ds.length, 150);
        assert_eq!(ds.string as usize, ds.static_space.as_ptr() as usize);

        dstring::append(&mut ds, ds.string, ds.length);

        assert_eq!(ds.length, 300);
        assert_ne!(
            ds.string as usize,
            ds.static_space.as_ptr() as usize,
            "300 bytes cannot still be in the 200-byte static space"
        );
        let got = std::slice::from_raw_parts(ds.string as *const u8, 300);
        assert!(
            got.iter().all(|b| *b == b'x'),
            "the self-append copied freed bytes"
        );
        dstring::free(&mut ds);
        assert_eq!(ds.length, 0);
        assert_eq!(ds.string as usize, ds.static_space.as_ptr() as usize);
    }
}

/// `Tcl_DStringAppendElement` quotes what it appends and separates elements
/// with a space (`generic/tclUtil.c:2740-2825`), and the sublist calls put
/// braces around a group (`generic/tclUtil.c:3061-3095`).
#[test]
fn a_dstring_builds_a_quoted_list() {
    unsafe {
        let mut ds: TclDString = std::mem::zeroed();
        dstring::init(&mut ds);
        dstring::append_element(&mut ds, c"first".as_ptr());
        dstring::append_element(&mut ds, c"has space".as_ptr());
        dstring::start_sublist(&mut ds);
        dstring::append_element(&mut ds, c"a".as_ptr());
        dstring::append_element(&mut ds, c"b".as_ptr());
        dstring::end_sublist(&mut ds);

        let text = std::str::from_utf8(std::slice::from_raw_parts(
            ds.string as *const u8,
            ds.length as usize,
        ))
        .expect("utf8");
        assert_eq!(text, "first {has space} {a b}");

        // And what it built reads back as the four-element list it looks like.
        let parsed = tclrs::list::split(text).expect("a well formed list");
        assert_eq!(parsed, vec!["first", "has space", "a b"]);
        dstring::free(&mut ds);
    }
}

/// `Tcl_DStringToObj` moves a dynamic buffer into the value rather than copying
/// it, and leaves the dstring empty and safe to reuse
/// (`generic/tclUtil.c:3005-3041`).
#[test]
fn dstring_to_obj_moves_the_buffer_and_resets_the_dstring() {
    unsafe {
        let mut ds: TclDString = std::mem::zeroed();
        dstring::init(&mut ds);
        let long = "y".repeat(250);
        dstring::append(&mut ds, long.as_ptr() as *const std::ffi::c_char, 250);
        let moved = ds.string;
        assert_ne!(moved as usize, ds.static_space.as_ptr() as usize);

        let o = dstring::to_obj(&mut ds);
        assert_eq!(
            (*o).bytes as usize,
            moved as usize,
            "a dynamic buffer should be transferred, not copied"
        );
        assert_eq!((*o).length, 250);
        assert_eq!(ds.length, 0);
        assert_eq!(ds.string as usize, ds.static_space.as_ptr() as usize);

        // Reusing the dstring afterwards must not touch the moved buffer.
        dstring::append(&mut ds, c"reused".as_ptr(), -1);
        assert_eq!(obj::string_of(o).len(), 250);
        dstring::free(&mut ds);
        obj::free_obj(o);
    }
}

/// A small dstring is still in the caller's own `staticSpace`, and that one
/// must be copied out, not moved.
#[test]
fn dstring_to_obj_copies_out_of_the_callers_static_space() {
    unsafe {
        let mut ds: TclDString = std::mem::zeroed();
        dstring::init(&mut ds);
        dstring::append(&mut ds, c"short".as_ptr(), -1);
        let o = dstring::to_obj(&mut ds);
        assert_ne!(
            (*o).bytes as usize,
            ds.static_space.as_ptr() as usize,
            "the value would point into a Tk stack frame"
        );
        assert_eq!(obj::string_of(o), b"short");
        obj::free_obj(o);
        dstring::free(&mut ds);
    }
}

// ---------------------------------------------------------------------------
// Tcl_HashTable
// ---------------------------------------------------------------------------

/// `RebuildTable` (`generic/tclHash.c:952-1031`) keeps every entry findable
/// across a growth, and moves the table off the caller's four static buckets.
///
/// Tk's window-name table is one `Tcl_InitHashTable(&mainPtr->nameTable,
/// TCL_STRING_KEYS)` (`tk9.0.4/generic/tkWindow.c:887`) that reaches into the
/// thousands, so a table that never grew would degrade to a linear scan of four
/// chains.
#[test]
fn a_string_keyed_table_grows_and_keeps_every_entry() {
    unsafe {
        let mut table: TclHashTable = std::mem::zeroed();
        hash::init(&mut table, TCL_STRING_KEYS);
        assert_eq!(table.num_buckets, TCL_SMALL_HASH_TABLE as isize);
        assert_eq!(table.rebuild_size, 12);
        assert_eq!(table.mask, 3);
        assert_eq!(table.down_shift, 28);

        let names: Vec<std::ffi::CString> = (0..500)
            .map(|i| std::ffi::CString::new(format!(".frame{i}.button")).unwrap())
            .collect();
        for (i, name) in names.iter().enumerate() {
            let mut is_new: c_int = 0;
            let e = (table.create_proc.unwrap())(&mut table, name.as_ptr(), &mut is_new);
            assert_eq!(is_new, 1);
            (*e).client_data = (i + 1) as *mut c_void;
        }
        assert_eq!(table.num_entries, 500);
        assert!(
            table.num_buckets > TCL_SMALL_HASH_TABLE as isize,
            "500 entries in 4 buckets means the table never rebuilt"
        );

        for (i, name) in names.iter().enumerate() {
            let e = (table.find_proc.unwrap())(&mut table, name.as_ptr());
            assert!(!e.is_null(), "{name:?} was lost in a rebuild");
            assert_eq!((*e).client_data as usize, i + 1);
        }

        // A walk sees each entry exactly once.
        let mut search: TclHashSearch = std::mem::zeroed();
        let mut seen = 0;
        let mut e = hash::first_entry(&mut table, &mut search);
        while !e.is_null() {
            seen += 1;
            e = hash::next_entry(&mut search);
        }
        assert_eq!(seen, 500);

        hash::delete_table(&mut table);
        assert_eq!(table.num_entries, 0);
    }
}

/// The array-key discipline — `keyType > 1`, "the key is an array of `keyType`
/// ints" (`generic/tclHash.c:253-262`) — which is what Tk's font, colour and
/// cursor caches use.
#[test]
fn an_array_keyed_table_matches_on_the_whole_key() {
    unsafe {
        let mut table: TclHashTable = std::mem::zeroed();
        hash::init(&mut table, 3);

        let a: [c_int; 3] = [1, 2, 3];
        let b: [c_int; 3] = [1, 2, 4];
        let mut is_new: c_int = 0;
        let ea = (table.create_proc.unwrap())(
            &mut table,
            a.as_ptr() as *const std::ffi::c_char,
            &mut is_new,
        );
        assert_eq!(is_new, 1);
        let eb = (table.create_proc.unwrap())(
            &mut table,
            b.as_ptr() as *const std::ffi::c_char,
            &mut is_new,
        );
        assert_eq!(is_new, 1);
        assert_ne!(ea, eb, "keys differing in the last int collided");

        let again = (table.find_proc.unwrap())(&mut table, a.as_ptr() as *const std::ffi::c_char);
        assert_eq!(again, ea);
        hash::delete_entry(ea);
        assert!(
            (table.find_proc.unwrap())(&mut table, a.as_ptr() as *const std::ffi::c_char).is_null()
        );
        assert_eq!(table.num_entries, 1);
        hash::delete_table(&mut table);
    }
}

/// After `Tcl_DeleteHashTable` the procs are replaced with ones that name the
/// mistake instead of dereferencing a freed bucket array
/// (`generic/tclHash.c:500-506`).
///
/// The replacements are not *called* here: they are `extern "C"`, so the panic
/// they raise cannot unwind and aborts the process — which is what Tcl's own
/// `Tcl_Panic` does too, and is not something a test can catch in-process. What
/// is checked is that the table was armed at all, which is the part a
/// `delete_table` that forgot would get wrong.
#[test]
fn a_deleted_table_is_armed_against_reuse() {
    unsafe {
        let mut table: TclHashTable = std::mem::zeroed();
        hash::init(&mut table, TCL_STRING_KEYS);
        let live_find = table.find_proc.unwrap() as usize;
        let live_create = table.create_proc.unwrap() as usize;

        hash::delete_table(&mut table);

        assert_ne!(
            table.find_proc.unwrap() as usize,
            live_find,
            "findProc still points at the live implementation"
        );
        assert_ne!(table.create_proc.unwrap() as usize, live_create);
        assert!(table.buckets.is_null());
    }
}

// ---------------------------------------------------------------------------
// Against the real Tk
// ---------------------------------------------------------------------------

/// Tk's ten registered object types, put through their own procs.
///
/// `TkRegisterObjTypes` (`tk9.0.4/generic/tkObj.c:1220-1233`) hands this side
/// ten `Tcl_ObjType *` during `Tk_Init`, and `tk-probe` with
/// `TCLRS_TK_EXERCISE_TYPES=1` reports what each one looks like and what
/// happened when its procs were called. The run ends in `abort` at the first
/// slot with no implementation, which is the probe working as designed, so the
/// output is read rather than the exit status.
///
/// Skipped when there is no Tk to load.
#[test]
fn tk_registers_ten_object_types_and_their_procs_run() {
    let probe = std::path::Path::new(env!("CARGO_BIN_EXE_tk-probe"));
    if tclrs::tk::load::Libtk::open().is_err() {
        eprintln!("skipping: no Tk dylib to load");
        return;
    }
    let out = std::process::Command::new(probe)
        .env("TCLRS_TK_DEGRADED", "1")
        .env("TCLRS_TK_EXERCISE_TYPES", "1")
        .output()
        .expect("run tk-probe");
    let log = String::from_utf8_lossy(&out.stderr);
    let lines: Vec<&str> = log
        .lines()
        .filter(|l| l.starts_with("tkobjtype "))
        .collect();

    let want = [
        "border",
        "bitmap",
        "color",
        "cursor",
        "font",
        "mm",
        "pixel",
        "statekey",
        "window",
        "textindex",
    ];
    assert_eq!(
        lines.len(),
        want.len(),
        "expected one report per registered type, got:\n{}",
        lines.join("\n")
    );
    for (line, name) in lines.iter().zip(want) {
        assert!(
            line.starts_with(&format!("tkobjtype {name} ")),
            "registration order changed: {line}"
        );
    }

    // Eight of the ten had procs called. `statekey` has none to call
    // (`tk9.0.4/generic/tkUtil.c:26-34`) and `textindex` is skipped on purpose.
    let ran = lines
        .iter()
        .filter(|l| l.contains("exercised=dup") || l.contains("exercised=free"))
        .count();
    assert_eq!(ran, 8, "in:\n{}", lines.join("\n"));

    // `mm` is Tk's only updateStringProc (`tk9.0.4/generic/tkObj.c:130`), and
    // it wrote its string into a Tcl_Obj this side allocated, through this
    // side's Tcl_Alloc and Tcl_PrintDouble.
    let mm = lines
        .iter()
        .find(|l| l.starts_with("tkobjtype mm "))
        .expect("mm");
    assert!(mm.contains("exercised=dup+updateString+free"), "{mm}");
    assert!(mm.contains(r#"string="42.0""#), "{mm}");

    // The version field read out of Tk's own tables: TCL_OBJTYPE_V1 stores
    // offsetof(Tcl_ObjType, indexProc) there (`generic/tcl.h:703-704`), which
    // is 56 for this layout, and TCL_OBJTYPE_V0 stores 0.
    for line in &lines {
        let v = line
            .split_whitespace()
            .find_map(|w| w.strip_prefix("version="))
            .expect("a version field");
        assert!(
            v == "0" || v == "56",
            "version {v} is neither TCL_OBJTYPE_V0 nor offsetof(indexProc): {line}"
        );
    }
}
