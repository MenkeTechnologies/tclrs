//! The layout claims in `src/tk/abi.rs`, checked.
//!
//! Every one of these numbers came out of a C program compiled against the Tcl
//! 9.0.4 headers with `offsetof`, not out of reading the field list. Asserting
//! them here is what stops a silent ABI drift — a wrong offset in an FFI struct
//! does not fail to compile, it corrupts memory at run time, and the failure
//! surfaces somewhere unrelated.
//!
//! These need no Tk installed. The slot-name check re-derives the table from a
//! Tcl source tree when `conformance/fetch-suite.sh` has been run, and is
//! skipped otherwise so the suite still passes on a machine without it.

#![cfg(feature = "tk")]

use std::mem::{offset_of, size_of};
use tclrs::tk::abi::*;

#[test]
fn tcl_obj_matches_the_c_layout() {
    // generic/tcl.h:744-765, measured with offsetof against the 9.0.4 headers.
    assert_eq!(size_of::<TclObj>(), 48);
    assert_eq!(offset_of!(TclObj, ref_count), 0);
    assert_eq!(offset_of!(TclObj, bytes), 8);
    assert_eq!(offset_of!(TclObj, length), 16);
    assert_eq!(offset_of!(TclObj, type_ptr), 24);
    assert_eq!(offset_of!(TclObj, internal_rep), 32);
    assert_eq!(size_of::<TclObjInternalRep>(), 16);
    // generic/tcl.h:657-698.
    assert_eq!(size_of::<TclObjType>(), 112);
}

#[test]
fn interp_prefix_puts_the_stub_table_where_tcl_initstubs_looks() {
    // generic/tclInt.h:2002-2007. `Tcl_InitStubs` reads offset 24 and, on its
    // failure path, writes 0 and 8 (generic/tclStubLib.c:62, 95-96). Getting
    // this wrong means Tk reads a stub table out of the wrong eight bytes.
    assert_eq!(offset_of!(InterpPrefix, legacy_result), 0);
    assert_eq!(offset_of!(InterpPrefix, legacy_free_proc), 8);
    assert_eq!(offset_of!(InterpPrefix, error_line), 16);
    assert_eq!(offset_of!(InterpPrefix, stub_table), 24);
}

#[test]
fn stub_tables_have_the_measured_shape() {
    // generic/tclDecls.h:1887. sizeof 5544 with slot 0 at offset 16 is what
    // makes the slot count 691; the count is the ABI, so it is asserted rather
    // than trusted.
    assert_eq!(TCL_STUBS_SLOTS, 691);
    assert_eq!(size_of::<TclStubs>(), 5544);
    assert_eq!(offset_of!(TclStubs, magic), 0);
    assert_eq!(offset_of!(TclStubs, hooks), 8);
    assert_eq!(offset_of!(TclStubs, slots), 16);
    assert_eq!(size_of::<TclStubHooks>(), 24);

    // The three secondary tables, macOS arms.
    assert_eq!(TCL_INT_STUBS_SLOTS, 262);
    assert_eq!(TCL_PLAT_STUBS_SLOTS, 4);
    assert_eq!(TCL_INT_PLAT_STUBS_SLOTS, 31);
    assert_eq!(size_of::<TclIntStubs>(), 16 + 262 * 8);
    assert_eq!(size_of::<TclPlatStubs>(), 16 + 4 * 8);
    assert_eq!(size_of::<TclIntPlatStubs>(), 16 + 31 * 8);
}

#[test]
fn stub_magic_is_the_tcl_9_value() {
    // generic/tcl.h:2309-2310: (int)0xFCA3BACB + sizeof(void *). Anything else
    // and Tcl_InitStubs rejects the interpreter outright
    // (generic/tclStubLib.c:75), which is the one failure that looks like Tk
    // simply not working rather than like an ABI mistake.
    assert_eq!(TCL_STUB_MAGIC as u32, 0xFCA3_BAD3);
}

#[test]
fn hash_table_matches_the_c_layout() {
    // generic/tcl.h:1088-1215. This one is load-bearing in an unusual way:
    // Tk allocates the table itself and calls findProc/createProc out of it
    // directly (generic/tcl.h:2607-2610), so a wrong offset here is a call
    // through whatever happens to be at offset 80 of a Tk struct.
    assert_eq!(size_of::<TclHashEntry>(), 40);
    assert_eq!(offset_of!(TclHashEntry, next_ptr), 0);
    assert_eq!(offset_of!(TclHashEntry, table_ptr), 8);
    assert_eq!(offset_of!(TclHashEntry, hash), 16);
    assert_eq!(offset_of!(TclHashEntry, client_data), 24);
    assert_eq!(offset_of!(TclHashEntry, key), 32);

    assert_eq!(size_of::<TclHashTable>(), 104);
    assert_eq!(offset_of!(TclHashTable, buckets), 0);
    assert_eq!(offset_of!(TclHashTable, static_buckets), 8);
    assert_eq!(offset_of!(TclHashTable, num_buckets), 40);
    assert_eq!(offset_of!(TclHashTable, num_entries), 48);
    assert_eq!(offset_of!(TclHashTable, rebuild_size), 56);
    assert_eq!(offset_of!(TclHashTable, mask), 64);
    assert_eq!(offset_of!(TclHashTable, down_shift), 72);
    assert_eq!(offset_of!(TclHashTable, key_type), 76);
    assert_eq!(offset_of!(TclHashTable, find_proc), 80);
    assert_eq!(offset_of!(TclHashTable, create_proc), 88);
    assert_eq!(offset_of!(TclHashTable, type_ptr), 96);

    assert_eq!(size_of::<TclHashSearch>(), 24);
}

#[test]
fn caller_owned_structs_match_the_c_layout() {
    // Tk declares each of these on its own stack and hands over a pointer, so
    // the shape is shared even though the memory is not.
    // generic/tcl.h:880-890.
    assert_eq!(size_of::<TclDString>(), 224);
    assert_eq!(offset_of!(TclDString, string), 0);
    assert_eq!(offset_of!(TclDString, length), 8);
    assert_eq!(offset_of!(TclDString, space_avl), 16);
    assert_eq!(offset_of!(TclDString, static_space), 24);
    // generic/tcl.h:847-870.
    assert_eq!(size_of::<TclCmdInfo>(), 80);
    assert_eq!(offset_of!(TclCmdInfo, is_native_object_proc), 0);
    assert_eq!(offset_of!(TclCmdInfo, obj_proc), 8);
    assert_eq!(offset_of!(TclCmdInfo, namespace_ptr), 56);
    assert_eq!(offset_of!(TclCmdInfo, obj_proc2), 64);
    // generic/tcl.h:1320-1331.
    assert_eq!(size_of::<TclTime>(), 16);
    assert_eq!(offset_of!(TclTime, sec), 0);
    assert_eq!(offset_of!(TclTime, usec), 8);
}

#[test]
fn every_slot_has_its_own_trap() {
    // The whole method rests on a call to slot N landing on a function that
    // knows it is N. Two slots sharing a trap would make one of them
    // unattributable, and the generator is the only thing stopping that.
    let traps = tclrs::tk::generated::TCL_TRAPS;
    let mut seen: Vec<usize> = traps.iter().map(|f| *f as usize).collect();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), TCL_STUBS_SLOTS);
    assert_eq!(tclrs::tk::generated::TCL_NAMES.len(), TCL_STUBS_SLOTS);
}

/// The committed slot names, against the header they were generated from.
///
/// Slot order is the ABI: one name out of place and every implementation after
/// it is installed at the wrong index, which no amount of testing the Rust side
/// would catch. Skipped when the Tcl source tree is absent, since there is then
/// nothing to compare against.
#[test]
fn committed_slot_names_match_the_header() {
    let header = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("conformance/vendor/tcl9.0.4/generic/tclDecls.h");
    if !header.exists() {
        eprintln!("skipping: run conformance/fetch-suite.sh to compare against the header");
        return;
    }
    let text = std::fs::read_to_string(&header).expect("read tclDecls.h");
    let start = text.find("typedef struct TclStubs {").expect("TclStubs");
    let end = text[start..]
        .find("\n} TclStubs;")
        .expect("end of TclStubs")
        + start;
    let body = &text[start..end];

    // Each slot declares exactly one `(*name)`; the order they appear in is the
    // order of the table.
    let mut names = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.find("(*") {
        rest = &rest[i + 2..];
        let j = rest.find(')').expect("unterminated slot name");
        names.push(&rest[..j]);
        rest = &rest[j..];
    }

    assert_eq!(names.len(), TCL_STUBS_SLOTS, "slot count drifted");
    for (i, want) in names.iter().enumerate() {
        assert_eq!(
            &tclrs::tk::generated::TCL_NAMES[i],
            want,
            "slot {i} is {} in the committed table but {want} in tclDecls.h",
            tclrs::tk::generated::TCL_NAMES[i]
        );
    }
}
