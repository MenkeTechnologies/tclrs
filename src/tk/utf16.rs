//! `Tcl_UtfToChar16DString` and the two functions under it, ported from
//! `generic/tclUtf.c`.
//!
//! macOS Tk converts every string it hands to Cocoa through this pair: an
//! `NSString` is UTF-16, and Tcl's strings are Tcl's own dialect of UTF-8, so
//! the boundary is exactly here.
//!
//! "Tcl's own dialect" is the reason this is a port rather than a call to
//! `str::encode_utf16`. Tcl's decoder never fails. A lone continuation byte in
//! `0x80..=0x9F` decodes as the *cp1252* character at that position — the table
//! at `generic/tclUtf.c:434-439`, with a citation to Wikipedia in the source —
//! and any other malformed byte decodes as itself. A Rust decoder rejects both,
//! and a host that rejected them would turn a byte string Tk considers
//! printable into an error Tk has no path for.
//!
//! Non-BMP characters are the other subtlety. `Tcl_UtfToChar16` returns *one
//! UTF-16 code unit at a time*: given a four-byte sequence it produces the high
//! surrogate and reports that it consumed **one** byte
//! (`generic/tclUtf.c:520-524`), so the next call re-reads the same sequence
//! from its second byte and, recognising the surrogate it produced last time,
//! emits the low surrogate and consumes three (`generic/tclUtf.c:475-480`).
//! That is why the port keeps `ch` across iterations exactly as the C does.

use std::ffi::c_char;

use super::abi::{RawStub, TclDString, TclStubs};
use super::dstring;
use super::generated::TCL_NAMES;
use super::trace::{record, Table};

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

/// `cp1252` (`generic/tclUtf.c:434-439`): what a naked continuation byte in
/// `0x80..=0x9F` decodes to.
const CP1252: [u16; 32] = [
    0x20AC, 0x0081, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039,
    0x0152, 0x008D, 0x017D, 0x008F, 0x0090, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014,
    0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x009D, 0x017E, 0x0178,
];

/// `UNICODE_SELF` (`generic/tclUtf.c:57`).
const UNICODE_SELF: u16 = 0x80;

/// `complete[]` (`generic/tclUtf.c:87-97`): how many bytes `Tcl_UtfCharComplete`
/// needs to see before the character starting at this byte is whole.
///
/// The two surprises are the C's own, both commented there: a continuation byte
/// asks for 3, because the caller may be pointing at the second byte of a
/// four-byte sequence; and `0xC1` asks for 1, because it can never begin a
/// valid sequence.
fn complete(byte: u8) -> usize {
    match byte {
        0x00..=0x7F => 1,
        0x80..=0xBF => 3,
        0xC1 => 1,
        0xC0 | 0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => 1,
    }
}

/// `Tcl_UtfCharComplete` (`generic/tclUtf.c:1176-1183`).
fn utf_char_complete(src: &[u8]) -> bool {
    !src.is_empty() && src.len() >= complete(src[0])
}

/// `Tcl_UtfToChar16` (`generic/tclUtf.c:449-542`): decode one UTF-16 code unit,
/// returning how many bytes it consumed.
///
/// `ch` is in/out: on entry it holds whatever the previous call produced, which
/// is what lets the second half of a surrogate pair be recognised.
///
/// Reads up to three bytes past `src[0]`; the caller guarantees they exist,
/// which is what `optPtr` and `Tcl_UtfCharComplete` are for at the call site.
fn utf_to_char16(src: &[u8], ch: &mut u16) -> usize {
    let byte = src[0];
    let at = |i: usize| -> u8 { *src.get(i).unwrap_or(&0) };

    if byte < 0xC0 {
        // `generic/tclUtf.c:470-480`: the continuation of a surrogate pair this
        // decoder split in two on the previous call.
        if (byte & 0xC0) == 0x80
            && (at(1) & 0xC0) == 0x80
            && (at(2) & 0xC0) == 0x80
            && ((((byte as u16).wrapping_sub(0x10) << 2) & 0xFC) | 0xD800) == (*ch & 0xFCFC)
            && (at(1) & 0xF0) == (((*ch << 4) & 0x30) as u8 | 0x80)
        {
            *ch = (((at(1) & 0x0F) as u16) << 6) + (at(2) & 0x3F) as u16 + 0xDC00;
            return 3;
        }
        // `generic/tclUtf.c:482-486`.
        *ch = if (byte as u16).wrapping_sub(0x80) < 0x20 {
            CP1252[(byte - 0x80) as usize]
        } else {
            byte as u16
        };
        return 1;
    }
    if byte < 0xE0 {
        // `generic/tclUtf.c:489-499`.
        if byte != 0xC1 && (at(1) & 0xC0) == 0x80 {
            let v = (((byte & 0x1F) as u16) << 6) | (at(1) & 0x3F) as u16;
            if v.wrapping_sub(1) >= UNICODE_SELF - 1 {
                *ch = v;
                return 2;
            }
        }
    } else if byte < 0xF0 {
        // `generic/tclUtf.c:505-516`.
        if (at(1) & 0xC0) == 0x80 && (at(2) & 0xC0) == 0x80 {
            let v = (((byte & 0x0F) as u16) << 12)
                | (((at(1) & 0x3F) as u16) << 6)
                | (at(2) & 0x3F) as u16;
            if v > 0x7FF {
                *ch = v;
                return 3;
            }
        }
    } else if byte < 0xF5 {
        // `generic/tclUtf.c:518-529`. The third trail byte is deliberately not
        // validated — the C cites ticket [ed29806ba] for that.
        if (at(1) & 0xC0) == 0x80 && (at(2) & 0xC0) == 0x80 {
            let high = ((((byte & 0x07) as u16) << 8)
                | (((at(1) & 0x3F) as u16) << 2)
                | ((at(2) & 0x3F) >> 4) as u16)
                .wrapping_sub(0x40);
            if high < 0x400 {
                // One byte consumed, not four: the low surrogate comes from the
                // next call, re-reading from `src[1]`.
                *ch = 0xD800 + high;
                return 1;
            }
        }
    }
    // Anything else represents itself (`generic/tclUtf.c:540-541`).
    *ch = byte as u16;
    1
}

/// Slot 355. `unsigned short *Tcl_UtfToChar16DString(const char *src,
/// Tcl_Size length, Tcl_DString *dsPtr)` (`generic/tclUtf.c:1698-1755`).
///
/// Appends to `dsPtr` and returns the start of what it appended. The two-pass
/// length handling is the C's: over-allocate at one code unit per input byte,
/// which is always enough, then shrink to what was written
/// (`generic/tclUtf.c:1724-1727`, `:1751-1752`).
///
/// # Safety
/// `src` is null or points at `length` readable bytes (or a NUL-terminated
/// string when `length` is negative); `ds` is an initialised `Tcl_DString`.
pub unsafe extern "C" fn utf_to_char16_dstring(
    src: *const c_char,
    length: isize,
    ds: *mut TclDString,
) -> *mut u16 {
    entered!("tcl_UtfToChar16DString");
    if src.is_null() {
        return std::ptr::null_mut();
    }
    let len = if length < 0 {
        libc::strlen(src)
    } else {
        length as usize
    };
    let bytes = std::slice::from_raw_parts(src as *const u8, len);

    // `Tcl_DStringLength` and `Tcl_DStringValue` are field reads
    // (`generic/tcl.h:892-893`), not slots, so they are field reads here too.
    let old_length = (*ds).length as usize;
    dstring::set_length(ds, (old_length + (len + 1) * size_of::<u16>()) as isize);
    let w_start = (*ds).string.add(old_length) as *mut u16;

    let mut w = w_start;
    let mut ch: u16 = 0;
    let mut p = 0usize;
    // The two loops are the C's split at `optPtr = endPtr - 3`
    // (`generic/tclUtf.c:1737-1749`): while at least four bytes remain the
    // decoder may read ahead freely; after that every character has to be
    // checked for completeness, and an incomplete tail is copied byte for byte.
    let opt = len.saturating_sub(3);
    while p <= opt && p < len {
        p += utf_to_char16(&bytes[p..], &mut ch);
        *w = ch;
        w = w.add(1);
    }
    while p < len {
        if utf_char_complete(&bytes[p..]) {
            p += utf_to_char16(&bytes[p..], &mut ch);
            *w = ch;
        } else {
            *w = bytes[p] as u16;
            p += 1;
        }
        w = w.add(1);
    }
    *w = 0;
    let written = w as usize - w_start as usize;
    dstring::set_length(ds, (old_length + written) as isize);
    w_start
}

/// `Tcl_UtfToUniChar` (`generic/tclUtf.c:551-625`): decode one *code point*,
/// returning how many bytes it consumed.
///
/// The 32-bit sibling of [`utf_to_char16`], and the only difference that
/// matters is the four-byte case: this one produces the whole character and
/// consumes four bytes, where the 16-bit decoder produces a high surrogate and
/// consumes one. Everything else — the cp1252 table, the overlong rejections,
/// "represents itself" — is the same, and is the same code path in the C too.
fn utf_to_uni_char(src: &[u8], ch: &mut i32) -> usize {
    let byte = src[0];
    let at = |i: usize| -> u8 { *src.get(i).unwrap_or(&0) };

    if byte < 0xC0 {
        // `generic/tclUtf.c:563-573`.
        *ch = if (byte as u32).wrapping_sub(0x80) < 0x20 {
            CP1252[(byte - 0x80) as usize] as i32
        } else {
            byte as i32
        };
        return 1;
    }
    if byte < 0xE0 {
        // `generic/tclUtf.c:576-586`.
        if byte != 0xC1 && (at(1) & 0xC0) == 0x80 {
            let v = (((byte & 0x1F) as i32) << 6) | (at(1) & 0x3F) as i32;
            if (v - 1) as u32 >= (UNICODE_SELF as u32) - 1 {
                *ch = v;
                return 2;
            }
        }
    } else if byte < 0xF0 {
        // `generic/tclUtf.c:592-603`.
        if (at(1) & 0xC0) == 0x80 && (at(2) & 0xC0) == 0x80 {
            let v = (((byte & 0x0F) as i32) << 12)
                | (((at(1) & 0x3F) as i32) << 6)
                | (at(2) & 0x3F) as i32;
            if v > 0x7FF {
                *ch = v;
                return 3;
            }
        }
    } else if byte < 0xF5 {
        // `generic/tclUtf.c:609-620`. All three trail bytes are checked here,
        // unlike the 16-bit decoder.
        if (at(1) & 0xC0) == 0x80 && (at(2) & 0xC0) == 0x80 && (at(3) & 0xC0) == 0x80 {
            let v = (((byte & 0x07) as i32) << 18)
                | (((at(1) & 0x3F) as i32) << 12)
                | (((at(2) & 0x3F) as i32) << 6)
                | (at(3) & 0x3F) as i32;
            if (v - 0x10000) as u32 <= 0xFFFFF {
                *ch = v;
                return 4;
            }
        }
    }
    *ch = byte as i32;
    1
}

/// Slot 646. `Tcl_Size Tcl_UtfToUniChar(const char *src, int *chPtr)`
/// (`generic/tclDecls.h:2537`).
///
/// # Safety
/// `src` must have at least one readable byte, and up to three more when the
/// first is a lead byte.
pub unsafe extern "C" fn utf_to_uni_char_slot(src: *const c_char, ch: *mut i32) -> isize {
    entered!("tcl_UtfToUniChar");
    let bytes = std::slice::from_raw_parts(src as *const u8, 4.min(libc::strlen(src) + 1));
    let mut v: i32 = 0;
    let n = utf_to_uni_char(bytes, &mut v);
    if !ch.is_null() {
        *ch = v;
    }
    n as isize
}

/// Slot 669. `Tcl_Size Tcl_NumUtfChars(const char *src, Tcl_Size length)`
/// (`generic/tclUtf.c:1013-1044`).
///
/// Not the same function as slot 312. This one counts **code points**, because
/// it walks with `TclUtfToUniChar`; slot 312 counts UTF-16 code units. Tk uses
/// both, for different purposes, and installing one body at both slots would
/// miscount every non-BMP string by exactly the number of such characters in
/// it.
///
/// # Safety
/// As [`num_utf_chars`].
pub unsafe extern "C" fn num_utf_chars_cp(src: *const c_char, length: isize) -> isize {
    entered!("tcl_NumUtfChars");
    if src.is_null() {
        return 0;
    }
    let len = if length < 0 {
        libc::strlen(src)
    } else {
        length as usize
    };
    let bytes = std::slice::from_raw_parts(src as *const u8, len);
    let mut ch: i32 = 0;
    let mut p = 0usize;
    let mut count: isize = 0;
    let opt = len.saturating_sub(4);
    while p <= opt && p < len {
        p += utf_to_uni_char(&bytes[p..], &mut ch);
        count += 1;
    }
    while p < len {
        if utf_char_complete(&bytes[p..]) {
            p += utf_to_uni_char(&bytes[p..], &mut ch);
        } else {
            p += 1;
        }
        count += 1;
    }
    count
}

/// Slot 671. `const char *Tcl_UtfAtIndex(const char *src, Tcl_Size index)`
/// (`generic/tclUtf.c:1150-1160`).
///
/// The code-point sibling of [`utf_at_index`], and simpler for it: with no
/// surrogates to produce there is no trailing correction to make.
///
/// # Safety
/// As [`utf_at_index`].
pub unsafe extern "C" fn utf_at_index_cp(src: *const c_char, index: isize) -> *const c_char {
    entered!("tcl_UtfAtIndex");
    if src.is_null() {
        return src;
    }
    let mut at = 0usize;
    let mut ch: i32 = 0;
    let mut remaining = index;
    while remaining > 0 {
        let rest =
            std::slice::from_raw_parts(src.add(at) as *const u8, libc::strlen(src.add(at)) + 1);
        at += utf_to_uni_char(rest, &mut ch);
        remaining -= 1;
    }
    src.add(at)
}

/// Slot 312. `Tcl_Size TclNumUtfChars(const char *src, Tcl_Size length)`
/// (`generic/tclUtf.c:1050-1101`), reached through the `Tcl_NumUtfChars` macro
/// (`generic/tclDecls.h:3873`).
///
/// It counts UTF-16 *code units*, not code points: the decoder it walks with is
/// `Tcl_UtfToChar16`, so a non-BMP character counts as two. That is the number
/// Tk wants — every index it computes with it is an index into an `NSString`.
///
/// An incomplete sequence at the end counts as one character and advances one
/// byte (`generic/tclUtf.c:1090-1096`), which is not what the decoder would do
/// with it, and is deliberate in the C.
///
/// # Safety
/// `src` points at `length` readable bytes, or at a NUL-terminated string when
/// `length` is negative.
pub unsafe extern "C" fn num_utf_chars(src: *const c_char, length: isize) -> isize {
    entered!("tclNumUtfChars");
    if src.is_null() {
        return 0;
    }
    let len = if length < 0 {
        libc::strlen(src)
    } else {
        length as usize
    };
    let bytes = std::slice::from_raw_parts(src as *const u8, len);
    let mut ch: u16 = 0;
    let mut p = 0usize;
    let mut count: isize = 0;
    // The C's split at `optPtr = endPtr - 4` (`generic/tclUtf.c:1074-1086`).
    let opt = len.saturating_sub(4);
    while p <= opt && p < len {
        p += utf_to_char16(&bytes[p..], &mut ch);
        count += 1;
    }
    while p < len {
        if utf_char_complete(&bytes[p..]) {
            p += utf_to_char16(&bytes[p..], &mut ch);
        } else {
            p += 1;
        }
        count += 1;
    }
    count
}

/// Slot 325. `const char *TclUtfAtIndex(const char *src, Tcl_Size index)`
/// (`generic/tclUtf.c:1116-1140`), reached through the `Tcl_UtfAtIndex` macro.
///
/// `index` counts UTF-16 code units, matching [`num_utf_chars`]. The trailing
/// correction is what makes that consistent: if the walk stopped on a high
/// surrogate that the decoder produced from a single byte, the position asked
/// for is the *second* unit of that pair, and the answer is one more decode on
/// (`generic/tclUtf.c:1130-1135`).
///
/// The string is walked as NUL-terminated — the C has no length to stop at —
/// so a caller asking for an index past the end walks off it, there and here.
///
/// # Safety
/// `src` must be NUL-terminated and `index` must be within its length in code
/// units.
pub unsafe extern "C" fn utf_at_index(src: *const c_char, index: isize) -> *const c_char {
    entered!("tclUtfAtIndex");
    if src.is_null() {
        return src;
    }
    let mut at = 0usize;
    let mut ch: u16 = 0;
    let mut len = 0usize;
    let rest = |p: usize| -> &'static [u8] {
        std::slice::from_raw_parts(src.add(p) as *const u8, libc::strlen(src.add(p)) + 1)
    };
    let mut remaining = index;
    while remaining > 0 {
        len = utf_to_char16(rest(at), &mut ch);
        at += len;
        remaining -= 1;
    }
    if index > 0 && ch >= 0xD800 && len < 3 {
        at += utf_to_char16(rest(at), &mut ch);
    }
    src.add(at)
}

/// Patch this module's slots into `t`, returning their indices.
///
/// # Safety
/// Each erased signature is the one `tclDecls.h` gives the slot, quoted on the
/// line above it.
pub unsafe fn install_impls(t: &mut TclStubs) -> Vec<usize> {
    vec![
        // unsigned short *(*tcl_UtfToChar16DString)(const char *, Tcl_Size,
        //     Tcl_DString *) /* 355 */
        install(
            t,
            "tcl_UtfToChar16DString",
            utf_to_char16_dstring as *const (),
        ),
        // Tcl_Size (*tclNumUtfChars)(const char *src, Tcl_Size length) /* 312 */
        install(t, "tclNumUtfChars", num_utf_chars as *const ()),
        // const char *(*tclUtfAtIndex)(const char *src, Tcl_Size index) /* 325 */
        install(t, "tclUtfAtIndex", utf_at_index as *const ()),
        // Tcl_Size (*tcl_UtfToUniChar)(const char *src, int *chPtr) /* 646 */
        install(t, "tcl_UtfToUniChar", utf_to_uni_char_slot as *const ()),
        // Tcl_Size (*tcl_NumUtfChars)(const char *src, Tcl_Size length) /* 669 */
        install(t, "tcl_NumUtfChars", num_utf_chars_cp as *const ()),
        // const char *(*tcl_UtfAtIndex)(const char *src, Tcl_Size index) /* 671 */
        install(t, "tcl_UtfAtIndex", utf_at_index_cp as *const ()),
    ]
}

/// As [`super::host`]'s own installer: by name, never by literal index.
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
