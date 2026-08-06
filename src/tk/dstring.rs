//! `Tcl_DString`, which Tk allocates itself and reads two fields of directly.
//!
//! The struct is the caller's: Tk declares one on its own stack —
//! `Tcl_DString nameDS;` in `Initialize` (`tk9.0.4/generic/tkWindow.c:3352`) —
//! and hands over a pointer. Every mutation goes through the stub table, but
//! `Tcl_DStringValue` and `Tcl_DStringLength` are macros over the fields
//! (`generic/tcl.h:892-893`), so the layout is shared even though the storage is
//! not. [`super::abi::TclDString`] is that layout, measured at 224 bytes with
//! `string` 0, `length` 8, `spaceAvl` 16 and `staticSpace` 24.
//!
//! # Ownership
//!
//! * The `Tcl_DString` itself belongs to Tk. Nothing here frees it, and nothing
//!   here keeps a pointer to it past the call.
//! * `string` is either `staticSpace` — inside Tk's own struct — or a
//!   `Tcl_Alloc` block this side made. `Tcl_DStringFree` distinguishes them by
//!   comparing the pointers (`generic/tclUtil.c:2912`), which is why the growth
//!   path may not blindly `free`.
//! * `Tcl_DStringToObj` *moves* a dynamic buffer into a `Tcl_Obj`'s `bytes`
//!   rather than copying it (`generic/tclUtil.c:3021-3029`), so after it the
//!   dstring no longer owns that block and is reset to its static space.
//!
//! # The aliasing case
//!
//! `Tcl_DStringAppend` handles a source that points *into* the dstring's own
//! buffer by recording the offset before reallocating and recomputing the
//! pointer afterwards (`generic/tclUtil.c:2664-2676`, Tcl ticket 16896d49fd).
//! Tk does exactly that: `Tcl_DStringAppend(&ds, Tcl_DStringValue(&ds), -1)` is
//! not a shape one has to go looking for, and getting it wrong is a
//! use-after-free that only shows up once the buffer happens to need growing.
//! Both append paths here reproduce the fix.

use std::ffi::{c_char, c_void};
use std::ptr;

use super::abi::{TclDString, TclObj, TCL_DSTRING_STATIC_SIZE};
use super::obj;

/// `Tcl_DStringInit` (`generic/tclUtil.c:2606-2614`).
///
/// `string` may never be left NULL, because `Tcl_DStringValue` reads it without
/// a check (`generic/tcl.h:893`).
///
/// # Safety
/// `ds` must point at `Tcl_DString`-shaped memory owned by the caller.
pub unsafe fn init(ds: *mut TclDString) {
    (*ds).string = (*ds).static_space.as_mut_ptr();
    (*ds).length = 0;
    (*ds).space_avl = TCL_DSTRING_STATIC_SIZE as isize;
    (*ds).static_space[0] = 0;
}

/// Whether the buffer is still the caller's inline space.
///
/// # Safety
/// `ds` must have been set up by [`init`].
unsafe fn is_static(ds: *mut TclDString) -> bool {
    ptr::eq((*ds).string, (*ds).static_space.as_ptr())
}

/// Make room for `need` bytes in total, moving off the static space when it is
/// no longer big enough, and keeping `alias` — a pointer that may point into
/// the old buffer — valid across the move.
///
/// Returns the possibly-updated `alias`.
///
/// # Safety
/// `ds` must have been set up by [`init`]; `alias` must be either null or a
/// readable pointer.
unsafe fn reserve(ds: *mut TclDString, need: isize, alias: *const c_char) -> *const c_char {
    if need <= (*ds).space_avl {
        return alias;
    }
    // Tcl doubles through TclAllocEx / TclReallocEx; the growth factor is not
    // part of the contract, only that spaceAvl ends up at least `need`.
    let cap = (need * 2).max(TCL_DSTRING_STATIC_SIZE as isize);
    // See [16896d49fd] (`generic/tclUtil.c:2666-2676`): a source inside this
    // buffer has to be re-derived after the move.
    let offset = if !alias.is_null()
        && alias >= (*ds).string
        && alias <= (*ds).string.offset((*ds).length)
    {
        Some(alias.offset_from((*ds).string))
    } else {
        None
    };
    let fresh = if is_static(ds) {
        let p = libc::malloc(cap as usize) as *mut c_char;
        assert!(!p.is_null(), "out of memory growing a Tcl_DString");
        ptr::copy_nonoverlapping((*ds).string, p, (*ds).length as usize);
        p
    } else {
        let p = libc::realloc((*ds).string as *mut c_void, cap as usize) as *mut c_char;
        assert!(!p.is_null(), "out of memory growing a Tcl_DString");
        p
    };
    (*ds).string = fresh;
    (*ds).space_avl = cap;
    match offset {
        Some(n) => fresh.offset(n),
        None => alias,
    }
}

/// `Tcl_DStringAppend` (`generic/tclUtil.c:2634-2688`).
///
/// # Safety
/// `ds` must have been set up by [`init`]; `bytes` must address `len` readable
/// bytes, or be NUL-terminated when `len` is negative.
pub unsafe fn append(ds: *mut TclDString, bytes: *const c_char, len: isize) -> *mut c_char {
    let n = if len < 0 {
        if bytes.is_null() {
            0
        } else {
            libc::strlen(bytes) as isize
        }
    } else {
        len
    };
    let src = reserve(ds, (*ds).length + n + 1, bytes);
    if n > 0 {
        ptr::copy_nonoverlapping(src, (*ds).string.offset((*ds).length), n as usize);
    }
    (*ds).length += n;
    *(*ds).string.offset((*ds).length) = 0;
    (*ds).string
}

/// The bytes currently held, as a slice.
///
/// # Safety
/// `ds` must have been set up by [`init`].
unsafe fn bytes_of(ds: *mut TclDString) -> &'static [u8] {
    std::slice::from_raw_parts((*ds).string as *const u8, (*ds).length as usize)
}

/// `TclNeedSpace` (`generic/tclUtil.c:3232-3306`): whether a separator has to be
/// written before the next element.
///
/// The three cases are the C's, in order: at the start of the string, at the
/// start of a nested element opened by one or more `{`, or already ended by a
/// separator — with the wrinkle that a trailing space preceded by an odd number
/// of backslashes is escaped and so is *not* a separator.
fn need_space(text: &[u8]) -> bool {
    let mut end = text.len();
    while end > 0 && text[end - 1] == b'{' {
        end -= 1;
    }
    if end == 0 {
        return false;
    }
    let last = text[end - 1];
    if crate::list::is_space(last) {
        // Count the backslashes before the space: an odd number escapes it, so
        // it is a literal character and a separator is still needed.
        let mut i = end - 1;
        let mut escaped = false;
        while i > 0 && text[i - 1] == b'\\' {
            escaped = !escaped;
            i -= 1;
        }
        return escaped;
    }
    true
}

/// `Tcl_DStringAppendElement` (`generic/tclUtil.c:2740-2825`): append `element`
/// quoted as a list element, with a separating space if one is needed.
///
/// A leading `#` only has to be quoted when the element could start a list, so
/// the `DONT_QUOTE_HASH` decision is the same `needSpace` answer that decides
/// the separator (`generic/tclUtil.c:2751-2774`). `crate::list::quote` takes it
/// as its `quote_hash` argument.
///
/// # Safety
/// `ds` must have been set up by [`init`]; `element` must be NUL-terminated.
pub unsafe fn append_element(ds: *mut TclDString, element: *const c_char) -> *mut c_char {
    let needs = need_space(bytes_of(ds));
    let text = if element.is_null() {
        String::new()
    } else {
        String::from_utf8_lossy(std::ffi::CStr::from_ptr(element).to_bytes()).into_owned()
    };
    let quoted = crate::list::quote(&text, !needs);
    if needs {
        append(ds, c" ".as_ptr(), 1);
    }
    append(ds, quoted.as_ptr() as *const c_char, quoted.len() as isize)
}

/// `Tcl_DStringStartSublist` (`generic/tclUtil.c:3061-3070`).
///
/// # Safety
/// As [`append_element`].
pub unsafe fn start_sublist(ds: *mut TclDString) {
    if need_space(bytes_of(ds)) {
        append(ds, c" {".as_ptr(), 2);
    } else {
        append(ds, c"{".as_ptr(), 1);
    }
}

/// `Tcl_DStringEndSublist` (`generic/tclUtil.c:3090-3095`).
///
/// # Safety
/// As [`append_element`].
pub unsafe fn end_sublist(ds: *mut TclDString) {
    append(ds, c"}".as_ptr(), 1);
}

/// `Tcl_DStringSetLength` (`generic/tclUtil.c:2846-2888`), which grows as well
/// as shrinks.
///
/// # Safety
/// `ds` must have been set up by [`init`].
pub unsafe fn set_length(ds: *mut TclDString, length: isize) {
    let length = length.max(0);
    reserve(ds, length + 1, ptr::null());
    (*ds).length = length;
    *(*ds).string.offset(length) = 0;
}

/// `Tcl_DStringFree` (`generic/tclUtil.c:2908-2919`): release a dynamic buffer
/// and go back to the static space.
///
/// # Safety
/// `ds` must have been set up by [`init`].
pub unsafe fn free(ds: *mut TclDString) {
    if !is_static(ds) {
        libc::free((*ds).string as *mut c_void);
    }
    init(ds);
}

/// `Tcl_DStringToObj` (`generic/tclUtil.c:3005-3041`): the dstring's contents as
/// a new value, moving the buffer rather than copying it when it is dynamic.
///
/// The moved block came from `libc::malloc` here and `Tcl_Obj::bytes` is freed
/// with `libc::free` there, so the transfer is between two owners of the same
/// allocator — which is the whole reason [`obj`] does not use Rust's.
///
/// # Safety
/// `ds` must have been set up by [`init`]. The result is pinned host storage
/// with count 0.
pub unsafe fn to_obj(ds: *mut TclDString) -> *mut TclObj {
    let result = if is_static(ds) {
        obj::new_string(bytes_of(ds))
    } else {
        let o = obj::alloc();
        obj::invalidate_string_rep(o);
        (*o).bytes = (*ds).string;
        (*o).length = (*ds).length;
        o
    };
    // Re-establish the dstring as empty with no buffer allocated. Not
    // `free`: when the branch above moved the buffer, freeing it here would be
    // a double free, and Tcl says as much (`generic/tclUtil.c:3031-3038`).
    (*ds).string = (*ds).static_space.as_mut_ptr();
    (*ds).space_avl = TCL_DSTRING_STATIC_SIZE as isize;
    (*ds).length = 0;
    (*ds).static_space[0] = 0;
    result
}
