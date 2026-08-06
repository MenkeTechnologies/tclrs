//! The three slots that decide an answer rather than move bytes:
//! `Tcl_GetIndexFromObjStruct`, `Tcl_PkgProvideEx`'s version arithmetic, and
//! the deferred-free table behind `Tcl_Preserve`.
//!
//! Each of these has a behaviour that a plausible-looking implementation gets
//! wrong in a way nothing crashes on:
//!
//! * the destination width of `Tcl_GetIndexFromObjStruct` arrives inside the
//!   flag word, so writing an `int` into a caller's `short` corrupts two bytes
//!   past it and the run continues;
//! * `9.0` and `9.0.0` are the same version but not the same string, and a
//!   host that compared strings would refuse Tk's second `Tcl_PkgProvideEx`
//!   and turn `Tk_Init` into a version conflict;
//! * `Tcl_EventuallyFree` on a preserved block has to *defer*, and a host that
//!   freed immediately would hand Tk a dangling widget record.
//!
//! None of these needs Tk installed.

#![cfg(feature = "tk")]

use std::ffi::{c_char, c_int, c_void};
use std::ptr;

use tclrs::tk::abi::{TCL_ERROR, TCL_OK};
use tclrs::tk::{index, obj, preserve};

/// `TCL_EXACT` (`generic/tcl.h:948`).
const TCL_EXACT: c_int = 1;
/// `TCL_NULL_OK` (`generic/tcl.h:949`).
const TCL_NULL_OK: c_int = 32;

/// The flag bits a caller's macro contributes for a destination of `T`:
/// `sizeof(*(indexPtr))<<1` (`generic/tclIndexObj.c:365-366`).
const fn width_bits<T>() -> c_int {
    (size_of::<T>() as c_int) << 1
}

/// A plain `const char *const []`, NULL-terminated, as most callers pass.
fn flat_table(entries: &[&'static std::ffi::CStr]) -> Vec<*const c_char> {
    let mut v: Vec<*const c_char> = entries.iter().map(|s| s.as_ptr()).collect();
    v.push(ptr::null());
    v
}

/// Look a name up in a flat table, into an `int`.
unsafe fn lookup(table: &[*const c_char], name: &str, flags: c_int) -> (c_int, c_int) {
    let o = obj::new_string(name.as_bytes());
    let mut out: c_int = -12345;
    let rc = index::get_index_from_obj_struct(
        ptr::null_mut(),
        o,
        table.as_ptr() as *const c_void,
        size_of::<*const c_char>() as isize,
        c"option".as_ptr(),
        flags | width_bits::<c_int>(),
        &mut out as *mut c_int as *mut c_void,
    );
    obj::free_obj(o);
    (rc, out)
}

#[test]
fn an_exact_match_wins_and_a_unique_abbreviation_is_accepted() {
    let table = flat_table(&[c"-background", c"-borderwidth", c"-cursor"]);
    unsafe {
        assert_eq!(lookup(&table, "-cursor", 0), (TCL_OK, 2));
        // "-bo" is a prefix of exactly one entry.
        assert_eq!(lookup(&table, "-bo", 0), (TCL_OK, 1));
        // "-b" is a prefix of two, so it is ambiguous
        // (`generic/tclIndexObj.c:275-277`).
        assert_eq!(lookup(&table, "-b", 0).0, TCL_ERROR);
        // TCL_EXACT refuses the abbreviation that was accepted above.
        assert_eq!(lookup(&table, "-bo", TCL_EXACT).0, TCL_ERROR);
        // …but not an exact hit, because the C breaks out of the scan before
        // it ever reaches the TCL_EXACT test (`generic/tclIndexObj.c:252-255`).
        assert_eq!(lookup(&table, "-cursor", TCL_EXACT), (TCL_OK, 2));
    }
}

/// The one behaviour that silently corrupts the caller's stack when it is
/// missing. `Tcl_GetIndexFromObjStruct` is reached through a macro that ORs
/// `sizeof(*indexPtr)<<1` into `flags`, and the body writes through a pointer
/// of exactly that width (`generic/tclIndexObj.c:299-315`).
///
/// The test writes into a struct with a sentinel field immediately after the
/// destination, so a body that wrote four bytes into a two-byte destination
/// would be caught by the sentinel rather than by luck.
#[test]
fn the_destination_width_comes_from_the_flag_word() {
    #[repr(C)]
    struct Narrow {
        idx: u16,
        guard: u16,
    }
    let table = flat_table(&[c"alpha", c"beta", c"gamma"]);
    let mut narrow = Narrow {
        idx: 0,
        guard: 0xBEEF,
    };
    unsafe {
        let o = obj::new_string(b"gamma");
        let rc = index::get_index_from_obj_struct(
            ptr::null_mut(),
            o,
            table.as_ptr() as *const c_void,
            size_of::<*const c_char>() as isize,
            c"option".as_ptr(),
            width_bits::<u16>(),
            ptr::addr_of_mut!(narrow.idx) as *mut c_void,
        );
        obj::free_obj(o);
        assert_eq!(rc, TCL_OK);
        assert_eq!(narrow.idx, 2);
        assert_eq!(
            narrow.guard, 0xBEEF,
            "a wider store than the flag word asked for ran off the destination"
        );

        // The 8-bit and 64-bit destinations the same mask selects.
        let o = obj::new_string(b"beta");
        let mut byte: u8 = 0;
        assert_eq!(
            index::get_index_from_obj_struct(
                ptr::null_mut(),
                o,
                table.as_ptr() as *const c_void,
                size_of::<*const c_char>() as isize,
                c"option".as_ptr(),
                width_bits::<u8>(),
                ptr::addr_of_mut!(byte) as *mut c_void,
            ),
            TCL_OK
        );
        assert_eq!(byte, 1);

        let mut wide: i64 = 0;
        assert_eq!(
            index::get_index_from_obj_struct(
                ptr::null_mut(),
                o,
                table.as_ptr() as *const c_void,
                size_of::<*const c_char>() as isize,
                c"option".as_ptr(),
                width_bits::<i64>(),
                ptr::addr_of_mut!(wide) as *mut c_void,
            ),
            TCL_OK
        );
        assert_eq!(wide, 1);
        obj::free_obj(o);
    }
}

/// The table is an array of *structures* whose first member is a `char *`
/// (`generic/tclIndexObj.c:66-71`). A host that assumed `sizeof(char *)` would
/// read the wrong field of every entry after the first.
#[test]
fn the_table_is_walked_with_the_callers_stride() {
    #[repr(C)]
    struct Entry {
        name: *const c_char,
        payload: u64,
    }
    let entries = [
        Entry {
            name: c"first".as_ptr(),
            payload: 10,
        },
        Entry {
            name: c"second".as_ptr(),
            payload: 20,
        },
        Entry {
            name: ptr::null(),
            payload: 0,
        },
    ];
    unsafe {
        let o = obj::new_string(b"second");
        let mut out: c_int = -1;
        let rc = index::get_index_from_obj_struct(
            ptr::null_mut(),
            o,
            entries.as_ptr() as *const c_void,
            size_of::<Entry>() as isize,
            c"option".as_ptr(),
            width_bits::<c_int>(),
            &mut out as *mut c_int as *mut c_void,
        );
        obj::free_obj(o);
        assert_eq!((rc, out), (TCL_OK, 1));
    }
}

/// An offset that cannot step over a pointer is rejected before the table is
/// touched (`generic/tclIndexObj.c:203-210`), which is what stops a mistaken
/// `0` from turning into an infinite scan of one entry.
#[test]
fn a_stride_smaller_than_a_pointer_is_refused() {
    unsafe {
        let o = obj::new_string(b"anything");
        let mut out: c_int = 0;
        let rc = index::get_index_from_obj_struct(
            ptr::null_mut(),
            o,
            ptr::null(),
            4,
            c"option".as_ptr(),
            width_bits::<c_int>(),
            &mut out as *mut c_int as *mut c_void,
        );
        obj::free_obj(o);
        assert_eq!(rc, TCL_ERROR);
    }
}

/// `TCL_NULL_OK` turns the empty string into a successful lookup with no index
/// (`generic/tclIndexObj.c:236-238`), which is how an option that may be unset
/// is spelled.
#[test]
fn the_empty_string_is_a_value_under_tcl_null_ok() {
    let table = flat_table(&[c"alpha", c"beta"]);
    unsafe {
        assert_eq!(lookup(&table, "", TCL_NULL_OK).0, TCL_OK);
        assert_eq!(lookup(&table, "", 0).0, TCL_ERROR);
    }
}

/// The lookup caches itself on the value, and the cached rep has to survive a
/// `Tcl_GetString` — `UpdateStringOfIndex` regenerates the *full* entry name,
/// never the abbreviation that was typed (`generic/tclIndexObj.c:373-374`).
#[test]
fn a_cached_lookup_regenerates_the_full_entry_name() {
    let table = flat_table(&[c"-background", c"-borderwidth"]);
    unsafe {
        let o = obj::new_string(b"-bo");
        let mut out: c_int = -1;
        assert_eq!(
            index::get_index_from_obj_struct(
                ptr::null_mut(),
                o,
                table.as_ptr() as *const c_void,
                size_of::<*const c_char>() as isize,
                c"option".as_ptr(),
                width_bits::<c_int>(),
                &mut out as *mut c_int as *mut c_void,
            ),
            TCL_OK
        );
        assert_eq!(out, 1);

        // Drop the string rep the way a shimmer would, then ask for it back:
        // it comes from the type, not from what was typed.
        obj::invalidate_string_rep(o);
        assert_eq!(obj::string_of(o), b"-borderwidth");

        // And the second lookup answers from the cache rather than the table.
        let mut again: c_int = -1;
        assert_eq!(
            index::get_index_from_obj_struct(
                ptr::null_mut(),
                o,
                table.as_ptr() as *const c_void,
                size_of::<*const c_char>() as isize,
                c"option".as_ptr(),
                width_bits::<c_int>(),
                &mut again as *mut c_int as *mut c_void,
            ),
            TCL_OK
        );
        assert_eq!(again, 1);
        obj::free_obj(o);
    }
}

// ---------------------------------------------------------------------------
// Tcl_Preserve
// ---------------------------------------------------------------------------

/// How many times the test's free proc has run, and on what.
static mut FREED: [usize; 4] = [0; 4];

unsafe extern "C" fn count_free(p: *mut c_void) {
    let i = (p as usize) & 3;
    FREED[i] += 1;
}

/// `Tcl_EventuallyFree` on a preserved block defers until the last
/// `Tcl_Release` (`generic/tclPreserve.c:211-241`, `:166-195`); on an
/// unpreserved one it frees immediately.
///
/// The addresses here are never dereferenced — the free proc only counts — so
/// they are deliberately fake, which is also what proves the table is keyed by
/// address and nothing else.
#[test]
fn eventually_free_defers_exactly_while_a_block_is_preserved() {
    let a = 0x1000usize as *mut c_void;
    let b = 0x2001usize as *mut c_void;
    unsafe {
        FREED = [0; 4];

        // Unpreserved: freed on the spot.
        preserve::eventually_free(b, count_free as *mut c_void);
        assert_eq!(FREED[1], 1);

        // Preserved twice, so the first release must not free it.
        preserve::preserve(a);
        preserve::preserve(a);
        preserve::eventually_free(a, count_free as *mut c_void);
        assert_eq!(FREED[0], 0, "freed while still preserved");
        preserve::release(a);
        assert_eq!(FREED[0], 0, "freed with one Tcl_Preserve still outstanding");
        preserve::release(a);
        assert_eq!(FREED[0], 1, "the last Tcl_Release did not free");

        // And the entry is gone, so a later preserve starts from scratch.
        preserve::preserve(a);
        preserve::release(a);
        assert_eq!(FREED[0], 1, "a stale mustFree flag survived the free");
    }
}

// ---------------------------------------------------------------------------
// Tcl_PkgProvideEx's version arithmetic
// ---------------------------------------------------------------------------

/// Tk provides itself twice at the same version
/// (`tk9.0.4/generic/tkWindow.c:3461-3466`), so "same version" has to be
/// decided the way Tcl decides it, not by comparing strings. The normalisation
/// (`generic/tclPkg.c:1654-1747`) turns a version into a list of signed
/// integers, and the comparison (`generic/tclPkg.c:1777-1929`) walks those.
#[test]
fn versions_compare_the_way_tcl_normalises_them() {
    use tclrs::tk::pkg::compare;

    // Trailing zero components are not significant.
    assert_eq!(compare("9.0", "9.0.0"), Some(0));
    assert_eq!(compare("9.0.4", "9.0.4"), Some(0));
    // Leading zeros in a component are skipped, not read as octal or as text.
    assert_eq!(compare("09.0", "9.0"), Some(0));
    assert_eq!(compare("1.10", "1.9"), Some(1));
    assert_eq!(compare("1.9", "1.10"), Some(-1));
    // The comparison is by length then by text, which is what lifts the 32-bit
    // ceiling the C's comment calls out (`generic/tclPkg.c:1799-1802`).
    assert_eq!(
        compare("1.99999999999999999999", "1.99999999999999999998"),
        Some(1)
    );
    // TIP 268: an alpha sorts below a beta sorts below the release.
    assert_eq!(compare("1.2a1", "1.2b1"), Some(-1));
    assert_eq!(compare("1.2b1", "1.2"), Some(-1));
    assert_eq!(compare("1.2a1", "1.2a2"), Some(-1));
    // Everything from a `+` is ignored (`generic/tclPkg.c:1692`).
    assert_eq!(compare("1.2+build9", "1.2"), Some(0));
}

/// The syntax rules, which are what stop a package name or a path from being
/// accepted as a version (`generic/tclPkg.c:1676-1687`).
#[test]
fn a_malformed_version_is_refused_rather_than_guessed_at() {
    use tclrs::tk::pkg::compare;

    for bad in ["", "a1", ".1", "1.", "1..2", "1.a", "1a.2", "1a2b3", "1.2c"] {
        assert_eq!(compare(bad, "1.0"), None, "{bad:?} was accepted");
    }
    for good in ["1", "1.0", "1.2a3", "9.0.4", "0.1"] {
        assert!(compare(good, "1.0").is_some(), "{good:?} was refused");
    }
}
