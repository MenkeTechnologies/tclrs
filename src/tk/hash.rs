//! Tcl's hash table, reimplemented because Tk cannot be given anything else.
//!
//! `Tcl_HashTable` is not opaque and not owned by Tcl. Tk embeds tables
//! directly in its own structs and initialises them in place —
//! `Tcl_InitHashTable(&mainPtr->nameTable, TCL_STRING_KEYS)`
//! (`tk9.0.4/generic/tkWindow.c:887`) — and every lookup afterwards goes
//! through `Tcl_FindHashEntry` / `Tcl_CreateHashEntry`, which are macros that
//! call function pointers stored *in that struct*
//! (`generic/tcl.h:2607-2610`). `Tcl_GetHashValue` and `Tcl_SetHashValue` are
//! macros over `hPtr->clientData` (`generic/tcl.h:2594-2595`).
//!
//! So none of this can be answered from behind the stub table. A host has to
//! lay out `Tcl_HashTable` and `Tcl_HashEntry` byte for byte and supply real
//! `findProc` / `createProc` implementations that Tk will call directly.
//!
//! What is here is chaining over a fixed bucket array, which is the shape
//! `generic/tclHash.c` uses minus its rebuild step. Three of Tcl's four key
//! disciplines are supported, following `generic/tclHash.c:253-262`:
//! `TCL_STRING_KEYS` (0), `TCL_ONE_WORD_KEYS` (1), and any `keyType > 1`, which
//! means "the key is an array of `keyType` ints" and is what Tk uses for its
//! font, colour and cursor caches. The two custom-key disciplines
//! (`TCL_CUSTOM_TYPE_KEYS`, `TCL_CUSTOM_PTR_KEYS`) stop the run rather than
//! guess, and Tk has not asked for either.

use std::ffi::{c_char, c_int, c_void, CStr};
use std::ptr;

use super::abi::*;

/// Buckets a table gets. `generic/tclHash.c` starts at `TCL_SMALL_HASH_TABLE`
/// and rebuilds; this allocates once and never rebuilds, so it starts wider.
const BUCKETS: usize = 64;

/// Set up `table` in the caller's memory: real bucket storage, and the two
/// function pointers Tk will call through without ever consulting the stub
/// table again.
///
/// # Safety
/// `table` must point at `Tcl_HashTable`-shaped memory owned by the caller.
pub unsafe fn init(table: *mut TclHashTable, key_type: c_int) {
    assert!(
        key_type >= TCL_STRING_KEYS,
        "hash key type {key_type} is one of Tcl's custom-key disciplines \
         (generic/tclHash.c:257-259), which is not implemented"
    );
    let bytes = BUCKETS * std::mem::size_of::<*mut TclHashEntry>();
    let buckets = libc::calloc(1, bytes) as *mut *mut TclHashEntry;
    assert!(!buckets.is_null(), "out of memory allocating hash buckets");
    (*table).buckets = buckets;
    (*table).static_buckets = [ptr::null_mut(); TCL_SMALL_HASH_TABLE];
    (*table).num_buckets = BUCKETS as isize;
    (*table).num_entries = 0;
    (*table).rebuild_size = isize::MAX;
    (*table).mask = BUCKETS - 1;
    (*table).down_shift = 0;
    (*table).key_type = key_type;
    (*table).find_proc = Some(find);
    (*table).create_proc = Some(create);
    (*table).type_ptr = ptr::null();
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

/// FNV-1a. Not Tcl's function — nothing outside this file can observe which
/// one is used, because a hash value only ever picks a bucket and `hash` is
/// never compared across implementations.
fn hash_of(bytes: &[u8]) -> usize {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h as usize
}

/// `tablePtr->findProc`. Called by the `Tcl_FindHashEntry` macro, never
/// through the stub table.
unsafe extern "C" fn find(table: *mut TclHashTable, key: *const c_char) -> *mut TclHashEntry {
    let want = key_bytes(table, key);
    let h = hash_of(&want);
    let mut e = *(*table).buckets.add(h & (*table).mask);
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
    let h = hash_of(&want);
    // A string or array key lives in the trailing bytes of the same allocation,
    // per the `char string[1]` / `int words[1]` arms of the union and the "MUST
    // BE LAST FIELD" comment (`generic/tcl.h:1095-1103`). The size rule is
    // `AllocArrayEntry` / `AllocStringEntry` (`generic/tclHash.c:674-691`):
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

    let b = (*table).buckets.add(h & (*table).mask);
    (*e).next_ptr = *b;
    *b = e;
    (*table).num_entries += 1;
    if !new_ptr.is_null() {
        *new_ptr = 1;
    }
    e
}

/// Unlink and free `entry`, keeping `numEntries` honest.
///
/// # Safety
/// `entry` must have come from this table implementation.
pub unsafe fn delete_entry(entry: *mut TclHashEntry) {
    let table = (*entry).table_ptr;
    let b = (*table).buckets.add((*entry).hash & (*table).mask);
    let mut cur = *b;
    if cur == entry {
        *b = (*entry).next_ptr;
    } else {
        while !cur.is_null() && (*cur).next_ptr != entry {
            cur = (*cur).next_ptr;
        }
        if !cur.is_null() {
            (*cur).next_ptr = (*entry).next_ptr;
        }
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
/// have been rehashed since.
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

/// Free every entry and the bucket array. The `Tcl_HashTable` itself is the
/// caller's memory and is left alone.
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
    libc::free((*table).buckets as *mut c_void);
    (*table).buckets = ptr::null_mut();
    (*table).num_entries = 0;
    (*table).num_buckets = 0;
}
