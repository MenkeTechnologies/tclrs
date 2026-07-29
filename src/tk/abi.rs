//! The Tcl 9.0.4 C ABI, as far as Tk depends on it.
//!
//! Every layout here was taken from the Tcl 9.0.4 source that
//! `conformance/fetch-suite.sh` unpacks under `conformance/vendor/tcl9.0.4/`,
//! and every offset was confirmed by compiling `offsetof` against those same
//! headers rather than inferred from the field list. The constants that record
//! those measurements are asserted in `tests/tk_abi.rs`, so a wrong guess here
//! is a test failure and not a segfault at run time.
//!
//! Two things about this ABI are not visible in the header's function list and
//! shape everything else:
//!
//! 1. Tk reaches Tcl *only* through a table of function pointers. The dylib has
//!    no undefined `Tcl_*` symbols at all; `Tcl_InitStubs` is statically linked
//!    into Tk from `libtclstub.a` and pulls the table out of the interpreter it
//!    is handed (`generic/tclStubLib.c:59-62`). Slot order is therefore the
//!    entire contract.
//!
//! 2. Three of the operations Tk performs on a `Tcl_Obj` are not table calls at
//!    all. `Tcl_IncrRefCount`, `Tcl_DecrRefCount` and `Tcl_IsShared` are macros
//!    that read and write `objPtr->refCount` directly (`generic/tcl.h:2517-2534`),
//!    so a value handed to Tk has to be writable memory in this exact shape.
//!    See [`TclObj`].

use std::ffi::{c_char, c_int, c_void};

/// A slot in a stub table, with its arguments and return value erased.
///
/// The tables are arrays of function pointers of many different signatures. A
/// single erased type is what lets them be built and patched uniformly; each
/// implementation is transmuted back to its real signature at the point of
/// installation, next to the header line that declares it.
pub type RawStub = unsafe extern "C" fn();

/// `TCL_STUB_MAGIC` for a 64-bit Tcl 9 (`generic/tcl.h:2309-2310`:
/// `((int) 0xFCA3BACB + (int) sizeof(void *))`).
///
/// `Tcl_InitStubs` rejects the interpreter outright if the table's `magic` is
/// anything else (`generic/tclStubLib.c:75`).
pub const TCL_STUB_MAGIC: c_int =
    0xFCA3_BACB_u32 as c_int + std::mem::size_of::<*const c_void>() as c_int;

/// `TCL_OK` (`generic/tcl.h`).
pub const TCL_OK: c_int = 0;
/// `TCL_ERROR` (`generic/tcl.h`).
pub const TCL_ERROR: c_int = 1;

/// Number of slots in `TclStubs` on this platform.
pub const TCL_STUBS_SLOTS: usize = super::generated::TCL_NAMES.len();
/// Number of slots in `TclIntStubs`.
pub const TCL_INT_STUBS_SLOTS: usize = super::generated::TCL_INT_NAMES.len();
/// Number of slots in `TclPlatStubs` on macOS.
pub const TCL_PLAT_STUBS_SLOTS: usize = super::generated::TCL_PLAT_NAMES.len();
/// Number of slots in `TclIntPlatStubs` on macOS.
pub const TCL_INT_PLAT_STUBS_SLOTS: usize = super::generated::TCL_INT_PLAT_NAMES.len();

/// `TclStubHooks` (`generic/tclDecls.h:1881-1885`): the three secondary tables,
/// which `Tcl_InitStubs` copies into the extension's globals
/// (`generic/tclStubLib.c:143-146`).
#[repr(C)]
pub struct TclStubHooks {
    pub tcl_plat_stubs: *const TclPlatStubs,
    pub tcl_int_stubs: *const TclIntStubs,
    pub tcl_int_plat_stubs: *const TclIntPlatStubs,
}

/// `TclStubs` (`generic/tclDecls.h:1887-2582`).
///
/// Measured: `sizeof` 5544, `magic` at 0, `hooks` at 8, slot 0 at 16, so
/// `(5544 - 16) / 8 = 691` slots.
#[repr(C)]
pub struct TclStubs {
    pub magic: c_int,
    pub hooks: *const TclStubHooks,
    pub slots: [RawStub; TCL_STUBS_SLOTS],
}

/// `TclIntStubs` (`generic/tclIntDecls.h:580`), same `magic`/`hooks` prefix.
#[repr(C)]
pub struct TclIntStubs {
    pub magic: c_int,
    pub hooks: *const c_void,
    pub slots: [RawStub; TCL_INT_STUBS_SLOTS],
}

/// `TclPlatStubs` (`generic/tclPlatDecls.h:162`), the `MAC_OSX_TCL` arm.
#[repr(C)]
pub struct TclPlatStubs {
    pub magic: c_int,
    pub hooks: *const c_void,
    pub slots: [RawStub; TCL_PLAT_STUBS_SLOTS],
}

/// `TclIntPlatStubs` (`generic/tclIntPlatDecls.h:589`), the `MAC_OSX_TCL` arm.
#[repr(C)]
pub struct TclIntPlatStubs {
    pub magic: c_int,
    pub hooks: *const c_void,
    pub slots: [RawStub; TCL_INT_PLAT_STUBS_SLOTS],
}

/// The head of Tcl's private `Interp` struct (`generic/tclInt.h:1990-2015`).
///
/// Tk itself never sees this: no Tk source file includes `tclInt.h`, and to Tk
/// a `Tcl_Interp *` is opaque. The one piece of code that does look inside is
/// `Tcl_InitStubs`, which Tk links statically out of `libtclstub.a` and which
/// starts with `Interp *iPtr = (Interp *)interp; ... iPtr->stubTable`
/// (`generic/tclStubLib.c:59-62`). It reads `stubTable`, and on the failure
/// path writes `legacyResult` and `legacyFreeProc`
/// (`generic/tclStubLib.c:95-96`) — offsets 24, 0 and 8.
///
/// The comment above the struct says outright that the position of `stubTable`
/// is frozen for exactly this reason (`generic/tclInt.h:1991-1999`), which is
/// what makes reproducing only the first 32 bytes safe rather than a bet on
/// the current release.
///
/// Measured against the 9.0.4 headers: `legacyResult` 0, `legacyFreeProc` 8,
/// `errorLine` 16, `stubTable` 24. (Real `Interp` is 1104 bytes; the rest is
/// Tcl's own state and is not reproduced.)
#[repr(C)]
pub struct InterpPrefix {
    /// Offset 0. Set by `Tcl_InitStubs` when it rejects the table.
    pub legacy_result: *const c_char,
    /// Offset 8. Set to 0 (`TCL_STATIC`) on that same path.
    pub legacy_free_proc: *const c_void,
    /// Offset 16, `int`.
    pub error_line: c_int,
    /// The 4 bytes the C compiler inserts to align `stub_table` to 8.
    pub _pad: c_int,
    /// Offset 24. The only field Tk's copy of `Tcl_InitStubs` reads.
    pub stub_table: *const TclStubs,
}

/// `Tcl_Obj` (`generic/tcl.h:744-765`).
///
/// Measured: `sizeof` 48; `refCount` 0, `bytes` 8, `length` 16, `typePtr` 24,
/// `internalRep` 32 (16 bytes, a union — `generic/tcl.h:716-736`).
///
/// This layout is not negotiable the way the stub table's is, because Tk does
/// not go through the table to touch it:
///
/// * `Tcl_IncrRefCount(objPtr)` expands to `((void)++(objPtr)->refCount)`
///   (`generic/tcl.h:2517-2519`) — a direct increment of offset 0.
/// * `Tcl_DecrRefCount(objPtr)` expands to a direct decrement, and only calls
///   the table (slot 30, `TclFreeObj`) once the count has already been written
///   back (`generic/tcl.h:2524-2531`).
/// * `Tcl_IsShared(objPtr)` expands to `((objPtr)->refCount > 1)`
///   (`generic/tcl.h:2532-2534`) — a direct read.
/// * Tk's own `Tcl_ObjType` implementations in `generic/tkObj.c` and ten other
///   files read and write `objPtr->typePtr` and `objPtr->internalRep` in place.
///
/// `Tcl_Size` is `ptrdiff_t` in Tcl 9 (`generic/tcl.h:332`), hence `isize`.
#[repr(C)]
pub struct TclObj {
    pub ref_count: isize,
    pub bytes: *mut c_char,
    pub length: isize,
    pub type_ptr: *const TclObjType,
    pub internal_rep: TclObjInternalRep,
}

/// `Tcl_ObjInternalRep` (`generic/tcl.h:716-736`): a union of 16 bytes, the
/// widest arm being the two-pointer one. Tk stores its own reps here.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct TclObjInternalRep {
    pub ptr1: *mut c_void,
    pub ptr2: *mut c_void,
}

/// `Tcl_ObjType` (`generic/tcl.h:657-698`), the Tcl 9 shape with the abstract
/// list slots. Measured `sizeof` 112. Only ever handled by pointer here: Tk
/// registers ten of its own with `Tcl_RegisterObjType`
/// (`tk9.0.4/generic/tkObj.c:1223-1232`) and this side just has to keep them.
#[repr(C)]
pub struct TclObjType {
    pub name: *const c_char,
    pub free_internal_rep_proc: *const c_void,
    pub dup_internal_rep_proc: *const c_void,
    pub update_string_proc: *const c_void,
    pub set_from_any_proc: *const c_void,
    pub version: usize,
    pub length_proc: *const c_void,
    pub index_proc: *const c_void,
    pub slice_proc: *const c_void,
    pub reverse_proc: *const c_void,
    pub get_elements_proc: *const c_void,
    pub set_element_proc: *const c_void,
    pub replace_proc: *const c_void,
    pub in_oper_proc: *const c_void,
}

/// A `Tcl_ObjType` is a table of code pointers and a name, written once and
/// read for the life of the process — Tcl's own are `static const` for exactly
/// that reason (`generic/tclObj.c`). Nothing mutates one, so sharing it across
/// threads is sound; the raw pointers inside are what makes the compiler ask.
unsafe impl Sync for TclObjType {}

/// `Tcl_CmdInfo` (`generic/tcl.h:847-870`). Measured `sizeof` 80;
/// `isNativeObjectProc` 0, `objProc` 8, `objClientData` 16, `proc` 24,
/// `clientData` 32, `deleteProc` 40, `deleteData` 48, `namespacePtr` 56,
/// `objProc2` 64, `objClientData2` 72.
///
/// Caller-allocated: Tk declares one on its stack and hands over a pointer for
/// `Tcl_GetCommandInfo` to fill (`tk9.0.4/generic/tkWindow.c:961`), then reads
/// four of the fields back.
#[repr(C)]
pub struct TclCmdInfo {
    /// 1 for `Tcl_CreateObjCommand`, 2 for `Tcl_CreateObjCommand2`, 0 for the
    /// string-based `Tcl_CreateCommand`.
    pub is_native_object_proc: c_int,
    pub obj_proc: *mut c_void,
    pub obj_client_data: *mut c_void,
    pub proc: *mut c_void,
    pub client_data: *mut c_void,
    pub delete_proc: *mut c_void,
    pub delete_data: *mut c_void,
    pub namespace_ptr: *mut TclNamespace,
    pub obj_proc2: *mut c_void,
    pub obj_client_data2: *mut c_void,
}

/// `Tcl_Namespace` (`generic/tcl.h:774-790`), the public view of a namespace.
///
/// The header notes that its first five fields must match Tcl's private
/// `Namespace` exactly (`generic/tcl.h:769-771`), which is what makes the
/// public struct safe to hand out. Tk asks for `::tk` and `::tk::mac` during
/// initialisation and only ever checks the result against NULL
/// (`tk9.0.4/generic/tkWindow.c:904`, `tk9.0.4/macosx/tkMacOSXDraw.c:85`).
#[repr(C)]
pub struct TclNamespace {
    pub name: *mut c_char,
    pub full_name: *mut c_char,
    pub client_data: *mut c_void,
    pub delete_proc: *const c_void,
    pub parent_ptr: *mut TclNamespace,
}

/// `Tcl_Time` (`generic/tcl.h:1320-1331`). Measured `sizeof` 16, `sec` 0
/// (`long long` in Tcl 9), `usec` 8 (`long`).
#[repr(C)]
pub struct TclTime {
    pub sec: i64,
    pub usec: std::ffi::c_long,
}

/// `TCL_SMALL_HASH_TABLE` (`generic/tcl.h`), measured as 4.
pub const TCL_SMALL_HASH_TABLE: usize = 4;

/// `TCL_STRING_KEYS` (`generic/tcl.h`), measured as 0.
pub const TCL_STRING_KEYS: c_int = 0;
/// `TCL_ONE_WORD_KEYS` (`generic/tcl.h`), measured as 1.
pub const TCL_ONE_WORD_KEYS: c_int = 1;

/// `Tcl_HashEntry` (`generic/tcl.h:1088-1104`). Measured `sizeof` 40;
/// `nextPtr` 0, `tablePtr` 8, `hash` 16, `clientData` 24, `key` 32.
///
/// `key` is a union whose last arm is a flexible `char string[1]`, so a
/// string-keyed entry is allocated larger than `sizeof` — the header's own
/// comment is "MUST BE LAST FIELD IN RECORD".
///
/// The fields are not private in practice. `Tcl_GetHashValue` and
/// `Tcl_SetHashValue` are macros over `clientData` (`generic/tcl.h:2594-2595`)
/// and `Tcl_GetHashKey` reads `key` and the table's `keyType`
/// (`generic/tcl.h:2596-2600`), all without touching the stub table.
#[repr(C)]
pub struct TclHashEntry {
    pub next_ptr: *mut TclHashEntry,
    pub table_ptr: *mut TclHashTable,
    pub hash: usize,
    pub client_data: *mut c_void,
    /// `key.oneWordValue` / `key.string[0]`; the storage after it belongs to
    /// the same allocation for a string key.
    pub key: *mut c_char,
}

/// `Tcl_HashTable` (`generic/tcl.h:1182-1215`). Measured `sizeof` 104;
/// `buckets` 0, `staticBuckets` 8, `numBuckets` 40, `numEntries` 48,
/// `rebuildSize` 56, `mask` 64, `downShift` 72, `keyType` 76, `findProc` 80,
/// `createProc` 88, `typePtr` 96.
///
/// This one is the sharpest case of ABI that is not in the stub table.
/// `Tcl_FindHashEntry` and `Tcl_CreateHashEntry` are macros that call
/// `tablePtr->findProc` and `tablePtr->createProc` (`generic/tcl.h:2607-2610`),
/// which live *inside the caller's own memory*. Tk declares its tables inline
/// in its own structs — `Tcl_InitHashTable(&mainPtr->nameTable,
/// TCL_STRING_KEYS)` (`tk9.0.4/generic/tkWindow.c:887`) — so a host has to fill
/// in a struct of exactly this shape, including two working function pointers,
/// and every subsequent lookup bypasses the stub table entirely.
#[repr(C)]
pub struct TclHashTable {
    pub buckets: *mut *mut TclHashEntry,
    pub static_buckets: [*mut TclHashEntry; TCL_SMALL_HASH_TABLE],
    pub num_buckets: isize,
    pub num_entries: isize,
    pub rebuild_size: isize,
    pub mask: usize,
    pub down_shift: c_int,
    pub key_type: c_int,
    pub find_proc:
        Option<unsafe extern "C" fn(*mut TclHashTable, *const c_char) -> *mut TclHashEntry>,
    pub create_proc: Option<
        unsafe extern "C" fn(*mut TclHashTable, *const c_char, *mut c_int) -> *mut TclHashEntry,
    >,
    pub type_ptr: *const c_void,
}

/// `Tcl_HashSearch` (`generic/tcl.h:1222-1228`). Measured `sizeof` 24.
#[repr(C)]
pub struct TclHashSearch {
    pub table_ptr: *mut TclHashTable,
    pub next_index: isize,
    pub next_entry_ptr: *mut TclHashEntry,
}

/// `TCL_DSTRING_STATIC_SIZE` (`generic/tcl.h:879`).
pub const TCL_DSTRING_STATIC_SIZE: usize = 200;

/// `Tcl_DString` (`generic/tcl.h:880-890`). Measured `sizeof` 224; `string` 0,
/// `length` 8, `spaceAvl` 16, `staticSpace` 24.
///
/// Like `Tcl_Obj`, this one is partly bypassed: `Tcl_DStringValue` and
/// `Tcl_DStringLength` are macros over the fields (`generic/tcl.h:892-893`),
/// so the layout has to be right even though every mutation goes through the
/// table. Tk declares these on its own stack — `Tcl_DString nameDS;` in
/// `Initialize` (`tk9.0.4/generic/tkWindow.c:3352`) — and hands over a pointer,
/// so the memory is Tk's and only the shape is shared.
#[repr(C)]
pub struct TclDString {
    pub string: *mut c_char,
    pub length: isize,
    pub space_avl: isize,
    pub static_space: [c_char; TCL_DSTRING_STATIC_SIZE],
}
