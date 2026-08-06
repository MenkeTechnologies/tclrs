//! Tcl's hash table, reimplemented because Tk cannot be given anything else.
//!
//! `Tcl_HashTable` is not opaque and not owned by Tcl. Tk embeds tables
//! directly in its own structs and initialises them in place —
//! `Tcl_InitHashTable(&mainPtr->nameTable, TCL_STRING_KEYS)`
//! (`tk9.0.4/generic/tkWindow.c:887`) — and every lookup afterwards goes
//! through `Tcl_FindHashEntry` / `Tcl_CreateHashEntry`, which are macros that
//! call function pointers stored *in that struct*
//! (`generic/tcl.h:2607-2610`). `Tcl_GetHashValue` and `Tcl_SetHashValue` are
//! macros over `hPtr->clientData` (`generic/tcl.h:2594-2595`), and
//! `Tcl_GetHashKey` reads `hPtr->key` and the table's `keyType`
//! (`generic/tcl.h:2596-2600`).
//!
//! So none of this can be answered from behind the stub table. A host has to
//! lay out `Tcl_HashTable` and `Tcl_HashEntry` byte for byte and supply real
//! `findProc` / `createProc` implementations that Tk will call directly.
//!
//! What is here is `generic/tclHash.c`, ported: the same initial state, the
//! same per-discipline hash and bucket-index rules, and the same growth step.
//! Three of Tcl's four key disciplines are supported, following
//! `generic/tclHash.c:253-262`: `TCL_STRING_KEYS` (0), `TCL_ONE_WORD_KEYS` (1),
//! and any `keyType > 1`, which means "the key is an array of `keyType` ints"
//! and is what Tk uses for its font, colour and cursor caches. The two
//! custom-key disciplines (`TCL_CUSTOM_TYPE_KEYS` = -2 and `TCL_CUSTOM_PTR_KEYS`
//! = -1, `generic/tcl.h:1253-1254`) stop the run rather than guess, and Tk asks
//! for neither: it names only `Tcl_InitHashTable` in its whole source.
//!
//! # Ownership
//!
//! * The `Tcl_HashTable` is the caller's memory. `init` fills it in and
//!   `delete_table` empties it; neither frees the struct.
//! * The bucket array is this side's, and starts as the caller's own
//!   `staticBuckets` — an array inside their struct — exactly as Tcl's does
//!   (`generic/tclHash.c:164-167`). `delete_table` and [`rebuild`] must
//!   therefore compare against `staticBuckets` before freeing, never free
//!   blindly.
//! * A `Tcl_HashEntry` is one allocation this side made, with a string or array
//!   key living in its trailing bytes. `clientData` is the caller's and is never
//!   touched: Tcl says as much (`generic/tclHash.c:374-376`).

use std::ffi::{c_char, c_int, c_void, CStr};
use std::ptr;

use super::abi::*;

/// `REBUILD_MULTIPLIER` (`generic/tclHash.c:27`).
const REBUILD_MULTIPLIER: isize = 3;

/// The multiplier in `RANDOM_INDEX` (`generic/tclHash.c:36-37`).
const RANDOM_MULTIPLIER: usize = 1103515245;

/// Set up `table` in the caller's memory: the caller's own static buckets, and
/// the two function pointers Tk will call through without ever consulting the
/// stub table again.
///
/// Every constant is `Tcl_InitCustomHashTable`'s
/// (`generic/tclHash.c:164-174`). They are not arbitrary: `downShift` 28 with
/// `mask` 3 is what `RANDOM_INDEX` needs to select 2 bits out of the top of a
/// 32-bit product, and [`rebuild`] moves both together.
///
/// # Safety
/// `table` must point at `Tcl_HashTable`-shaped memory owned by the caller.
pub unsafe fn init(table: *mut TclHashTable, key_type: c_int) {
    assert!(
        key_type >= TCL_STRING_KEYS,
        "hash key type {key_type} is one of Tcl's custom-key disciplines \
         (generic/tcl.h:1253-1254), which is not implemented"
    );
    (*table).static_buckets = [ptr::null_mut(); TCL_SMALL_HASH_TABLE];
    (*table).buckets = ptr::addr_of_mut!((*table).static_buckets) as *mut *mut TclHashEntry;
    (*table).num_buckets = TCL_SMALL_HASH_TABLE as isize;
    (*table).num_entries = 0;
    (*table).rebuild_size = TCL_SMALL_HASH_TABLE as isize * REBUILD_MULTIPLIER;
    (*table).mask = 3;
    (*table).down_shift = 28;
    (*table).key_type = key_type;
    (*table).find_proc = Some(find);
    (*table).create_proc = Some(create);
    (*table).type_ptr = ptr::null();
}

/// Whether the bucket array is still the caller's inline space.
///
/// # Safety
/// `table` must have been set up by [`init`].
unsafe fn buckets_are_static(table: *mut TclHashTable) -> bool {
    ptr::eq(
        (*table).buckets as *const c_void,
        ptr::addr_of!((*table).static_buckets) as *const c_void,
    )
}

/// The key material a caller passed in, as bytes to hash and compare.
///
/// The three shapes are `generic/tclHash.c:253-262`: a NUL-terminated string, a
/// single word held in the pointer itself, or — for any `keyType > 1` — an
/// array of `keyType` ints that `key` points at.
unsafe fn key_bytes(table: *mut TclHashTable, key: *const c_char) -> Vec<u8> {
    match (*table).key_type {
        TCL_STRING_KEYS => CStr::from_ptr(key).to_bytes().to_vec(),
        TCL_ONE_WORD_KEYS => (key as usize).to_ne_bytes().to_vec(),
        n => std::slice::from_raw_parts(key as *const u8, n as usize * 4).to_vec(),
    }
}

/// The key already stored in `entry`, in the same form as [`key_bytes`].
unsafe fn entry_key_bytes(entry: *mut TclHashEntry) -> Vec<u8> {
    let table = (*entry).table_ptr;
    let inline = ptr::addr_of!((*entry).key) as *const u8;
    match (*table).key_type {
        TCL_STRING_KEYS => CStr::from_ptr(inline as *const c_char).to_bytes().to_vec(),
        TCL_ONE_WORD_KEYS => ((*entry).key as usize).to_ne_bytes().to_vec(),
        n => std::slice::from_raw_parts(inline, n as usize * 4).to_vec(),
    }
}

/// `TclHashStringKey` (`generic/tclHash.c:832-879`): the first byte, then
/// `result += (result << 3) + c` for each byte after it.
///
/// Ported rather than replaced even though nothing outside this file can
/// observe which function is used, because the hash and the bucket-index rule
/// have to agree with each other across [`create`], [`find`], [`delete_entry`]
/// and [`rebuild`], and porting both from the same source is how they stay in
/// agreement.
fn hash_string(bytes: &[u8]) -> usize {
    let mut result: usize = match bytes.first() {
        None | Some(0) => return 0,
        Some(b) => *b as usize,
    };
    for b in &bytes[1..] {
        result = result.wrapping_add(result << 3).wrapping_add(*b as usize);
    }
    result
}

/// `HashArrayKey` (`generic/tclHash.c:738-752`): the sum of the `keyType` ints.
fn hash_array(bytes: &[u8]) -> usize {
    let mut result: usize = 0;
    for word in bytes.chunks_exact(4) {
        let v = i32::from_ne_bytes([word[0], word[1], word[2], word[3]]);
        result = result.wrapping_add(v as usize);
    }
    result
}

/// The hash of a key, per discipline.
unsafe fn hash_of(table: *mut TclHashTable, key: &[u8], raw: *const c_char) -> usize {
    match (*table).key_type {
        TCL_STRING_KEYS => hash_string(key),
        // The one-word discipline has no `hashKeyProc` at all, and the hash is
        // the pointer itself (`generic/tclHash.c:271-274`).
        TCL_ONE_WORD_KEYS => raw as usize,
        _ => hash_array(key),
    }
}

/// The bucket a hash falls in.
///
/// String keys mask the hash directly; the other two disciplines go through
/// `RANDOM_INDEX` — one because it has no hash proc, the array one because its
/// key type sets `TCL_HASH_KEY_RANDOMIZE_HASH` (`generic/tclHash.c:66-73`,
/// `generic/tclHash.c:264-274`). Summed ints would otherwise cluster in the low
/// buckets, which is the whole reason that flag exists.
unsafe fn bucket_of(table: *mut TclHashTable, hash: usize) -> usize {
    if (*table).key_type == TCL_STRING_KEYS {
        hash & (*table).mask
    } else {
        (hash.wrapping_mul(RANDOM_MULTIPLIER) >> (*table).down_shift) & (*table).mask
    }
}

/// `tablePtr->findProc`. Called by the `Tcl_FindHashEntry` macro, never
/// through the stub table.
unsafe extern "C" fn find(table: *mut TclHashTable, key: *const c_char) -> *mut TclHashEntry {
    let want = key_bytes(table, key);
    let h = hash_of(table, &want, key);
    let mut e = *(*table).buckets.add(bucket_of(table, h));
    while !e.is_null() {
        if (*e).hash == h && entry_key_bytes(e) == want {
            return e;
        }
        e = (*e).next_ptr;
    }
    ptr::null_mut()
}

/// `tablePtr->createProc`. `*newPtr` is 1 when an entry was created and 0 when
/// an existing one was returned, which is how every caller distinguishes the
/// two (`generic/tclHash.c`).
unsafe extern "C" fn create(
    table: *mut TclHashTable,
    key: *const c_char,
    new_ptr: *mut c_int,
) -> *mut TclHashEntry {
    let existing = find(table, key);
    if !existing.is_null() {
        if !new_ptr.is_null() {
            *new_ptr = 0;
        }
        return existing;
    }

    let want = key_bytes(table, key);
    let h = hash_of(table, &want, key);
    // A string or array key lives in the trailing bytes of the same allocation,
    // per the `char string[1]` / `int words[1]` arms of the union and the "MUST
    // BE LAST FIELD" comment (`generic/tcl.h:1095-1103`). The size rule is
    // `AllocArrayEntry` / `AllocStringEntry` (`generic/tclHash.c:770-788`):
    // `offsetof(key) + key bytes`, never smaller than the struct itself.
    let size = if (*table).key_type == TCL_ONE_WORD_KEYS {
        std::mem::size_of::<TclHashEntry>()
    } else {
        let terminator = usize::from((*table).key_type == TCL_STRING_KEYS);
        (std::mem::offset_of!(TclHashEntry, key) + want.len() + terminator)
            .max(std::mem::size_of::<TclHashEntry>())
    };
    let e = libc::calloc(1, size) as *mut TclHashEntry;
    assert!(!e.is_null(), "out of memory allocating hash entry");
    (*e).table_ptr = table;
    (*e).hash = h;
    (*e).client_data = ptr::null_mut();
    if (*table).key_type == TCL_ONE_WORD_KEYS {
        (*e).key = key as *mut c_char;
    } else {
        let dst = ptr::addr_of_mut!((*e).key) as *mut u8;
        ptr::copy_nonoverlapping(want.as_ptr(), dst, want.len());
        if (*table).key_type == TCL_STRING_KEYS {
            *dst.add(want.len()) = 0;
        }
    }

    let b = (*table).buckets.add(bucket_of(table, h));
    (*e).next_ptr = *b;
    *b = e;
    (*table).num_entries += 1;
    if !new_ptr.is_null() {
        *new_ptr = 1;
    }
    // `generic/tclHash.c:350-359`: grow once the table is denser than three
    // entries per bucket. Without this the table stays four buckets wide
    // forever, and Tk's window-name table alone reaches into the thousands.
    if (*table).num_entries >= (*table).rebuild_size {
        rebuild(table);
    }
    e
}

/// `RebuildTable` (`generic/tclHash.c:952-1031`): four times the buckets, four
/// times the rebuild threshold, two fewer bits of shift, two more bits of mask,
/// and every entry rehashed into the new array.
///
/// # Safety
/// `table` must have been set up by [`init`].
pub unsafe fn rebuild(table: *mut TclHashTable) {
    let old_size = (*table).num_buckets;
    let old_buckets = (*table).buckets;
    let old_was_static = buckets_are_static(table);

    (*table).num_buckets *= 4;
    let bytes = (*table).num_buckets as usize * std::mem::size_of::<*mut TclHashEntry>();
    let fresh = libc::calloc(1, bytes) as *mut *mut TclHashEntry;
    assert!(!fresh.is_null(), "out of memory rebuilding a hash table");
    (*table).buckets = fresh;
    (*table).rebuild_size *= 4;
    if (*table).down_shift > 1 {
        (*table).down_shift -= 2;
    }
    (*table).mask = ((*table).mask << 2) + 3;

    for i in 0..old_size {
        let mut e = *old_buckets.offset(i);
        while !e.is_null() {
            let next = (*e).next_ptr;
            let b = fresh.add(bucket_of(table, (*e).hash));
            (*e).next_ptr = *b;
            *b = e;
            e = next;
        }
    }

    if !old_was_static {
        libc::free(old_buckets as *mut c_void);
    }
}

/// Unlink and free `entry`, keeping `numEntries` honest.
///
/// `clientData` is deliberately left alone: freeing it is the caller's job
/// (`generic/tclHash.c:374-376`).
///
/// # Safety
/// `entry` must have come from this table implementation.
pub unsafe fn delete_entry(entry: *mut TclHashEntry) {
    let table = (*entry).table_ptr;
    let b = (*table).buckets.add(bucket_of(table, (*entry).hash));
    let mut cur = *b;
    if cur == entry {
        *b = (*entry).next_ptr;
    } else {
        while !cur.is_null() && (*cur).next_ptr != entry {
            cur = (*cur).next_ptr;
        }
        assert!(
            !cur.is_null(),
            "malformed bucket chain in Tcl_DeleteHashEntry \
             (generic/tclHash.c:416-419)"
        );
        (*cur).next_ptr = (*entry).next_ptr;
    }
    (*table).num_entries -= 1;
    libc::free(entry as *mut c_void);
}

/// First entry of `table`, seeding `search` for [`next_entry`].
///
/// # Safety
/// `table` must have been set up by [`init`].
pub unsafe fn first_entry(
    table: *mut TclHashTable,
    search: *mut TclHashSearch,
) -> *mut TclHashEntry {
    (*search).table_ptr = table;
    (*search).next_index = 0;
    (*search).next_entry_ptr = ptr::null_mut();
    next_entry(search)
}

/// Next entry of a walk started by [`first_entry`].
///
/// # Safety
/// `search` must have been seeded by [`first_entry`] and the table must not
/// have been rebuilt since.
pub unsafe fn next_entry(search: *mut TclHashSearch) -> *mut TclHashEntry {
    let table = (*search).table_ptr;
    loop {
        if !(*search).next_entry_ptr.is_null() {
            let e = (*search).next_entry_ptr;
            (*search).next_entry_ptr = (*e).next_ptr;
            return e;
        }
        if (*search).next_index >= (*table).num_buckets {
            return ptr::null_mut();
        }
        (*search).next_entry_ptr = *(*table).buckets.offset((*search).next_index);
        (*search).next_index += 1;
    }
}

/// `BogusFind` (`generic/tclHash.c:899`): what a deleted table's `findProc`
/// becomes, so that using one after `Tcl_DeleteHashTable` is a named failure
/// rather than a null dereference (`generic/tclHash.c:500-506`).
unsafe extern "C" fn bogus_find(
    _table: *mut TclHashTable,
    _key: *const c_char,
) -> *mut TclHashEntry {
    panic!("called Tcl_FindHashEntry on a deleted table (generic/tclHash.c:899-907)");
}

/// `BogusCreate` (`generic/tclHash.c:925`).
unsafe extern "C" fn bogus_create(
    _table: *mut TclHashTable,
    _key: *const c_char,
    _new: *mut c_int,
) -> *mut TclHashEntry {
    panic!("called Tcl_CreateHashEntry on a deleted table (generic/tclHash.c:925-931)");
}

/// `Tcl_DeleteHashTable` (`generic/tclHash.c:453-507`): free every entry and any
/// dynamic bucket array, then arm the table so a later use says so.
///
/// The `Tcl_HashTable` itself is the caller's memory and is left in place.
///
/// # Safety
/// `table` must have been set up by [`init`].
pub unsafe fn delete_table(table: *mut TclHashTable) {
    for i in 0..(*table).num_buckets {
        let mut e = *(*table).buckets.offset(i);
        while !e.is_null() {
            let next = (*e).next_ptr;
            libc::free(e as *mut c_void);
            e = next;
        }
    }
    if !buckets_are_static(table) {
        libc::free((*table).buckets as *mut c_void);
    }
    (*table).buckets = ptr::null_mut();
    (*table).num_entries = 0;
    (*table).num_buckets = 0;
    (*table).find_proc = Some(bogus_find);
    (*table).create_proc = Some(bogus_create);
}
