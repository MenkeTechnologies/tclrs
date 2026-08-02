//! Two things: the UTF-8/UTF-16 boundary Cocoa sits behind, and what the whole
//! merged host reaches when it is pointed at the real Tk.
//!
//! The conversion tests need no Tk. The session test does, and skips itself
//! when the dylib is missing, exactly as `tk_probe_session` does.

#![cfg(feature = "tk")]

use std::ffi::c_char;
use std::process::Command;
use std::ptr;

use tclrs::tk::abi::TclDString;
use tclrs::tk::{dstring, utf16};

// ---------------------------------------------------------------------------
// The UTF-8 to UTF-16 boundary
// ---------------------------------------------------------------------------

/// Convert `src` and return the code units written, without the terminator.
fn to_utf16(src: &[u8]) -> Vec<u16> {
    let mut ds = TclDString {
        string: ptr::null_mut(),
        length: 0,
        space_avl: 0,
        static_space: [0; tclrs::tk::abi::TCL_DSTRING_STATIC_SIZE],
    };
    unsafe {
        dstring::init(&mut ds);
        let w = utf16::utf_to_char16_dstring(
            src.as_ptr() as *const c_char,
            src.len() as isize,
            &mut ds,
        );
        let units = ds.length as usize / size_of::<u16>();
        let out = std::slice::from_raw_parts(w, units).to_vec();
        dstring::free(&mut ds);
        out
    }
}

/// Plain text and a BMP character go through unchanged, and the dstring's
/// length is set to what was written rather than to what was reserved
/// (`generic/tclUtf.c:1751-1752`).
#[test]
fn bmp_text_converts_to_one_code_unit_per_character() {
    assert_eq!(to_utf16(b"hello"), vec![0x68, 0x65, 0x6C, 0x6C, 0x6F]);
    // U+00E9 as two UTF-8 bytes.
    assert_eq!(to_utf16("é".as_bytes()), vec![0x00E9]);
    // U+20AC as three.
    assert_eq!(to_utf16("€".as_bytes()), vec![0x20AC]);
    assert_eq!(to_utf16(b""), Vec::<u16>::new());
}

/// A non-BMP character becomes a surrogate pair, and the decoder produces it
/// in two calls that consume one byte and then three
/// (`generic/tclUtf.c:518-529`, `:470-481`). Getting that handshake wrong
/// produces either one unit or four, and both are silently wrong text.
#[test]
fn a_non_bmp_character_becomes_a_surrogate_pair() {
    // U+1F600, four UTF-8 bytes, surrogates D83D DE00.
    assert_eq!(to_utf16("\u{1F600}".as_bytes()), vec![0xD83D, 0xDE00]);
    // Surrounded by ASCII, so a wrong byte count would shift everything after.
    assert_eq!(
        to_utf16("a\u{1F600}b".as_bytes()),
        vec![0x61, 0xD83D, 0xDE00, 0x62]
    );
    // And the two counting functions disagree about it on purpose: slot 312
    // counts code units, slot 669 counts code points.
    let s = c"a\u{1F600}b";
    unsafe {
        assert_eq!(utf16::num_utf_chars(s.as_ptr(), -1), 4);
        assert_eq!(utf16::num_utf_chars_cp(s.as_ptr(), -1), 3);
    }
}

/// Tcl's decoder never fails. A naked continuation byte in `0x80..=0x9F` is a
/// cp1252 character (`generic/tclUtf.c:434-439`, `:482-486`) and anything else
/// malformed is itself. A host that used a validating decoder would turn text
/// Tk considers printable into an error Tk has nowhere to put.
#[test]
fn malformed_input_decodes_rather_than_failing() {
    // 0x80 is cp1252's euro sign, not a replacement character and not 0x80.
    assert_eq!(to_utf16(&[0x80]), vec![0x20AC]);
    // 0xA0 is outside the cp1252 window, so it represents itself.
    assert_eq!(to_utf16(&[0xA0]), vec![0x00A0]);
    // A lead byte with no trail byte represents itself.
    assert_eq!(to_utf16(&[0xC3]), vec![0x00C3]);
    // An overlong encoding of '/' is refused as a character and its lead byte
    // represents itself (`generic/tclUtf.c:489-499`).
    assert_eq!(to_utf16(&[0xC0, 0xAF]), vec![0x00C0, 0x00AF]);
    // A truncated three-byte sequence at the very end is copied byte for byte
    // by the tail loop (`generic/tclUtf.c:1743-1748`).
    assert_eq!(to_utf16(&[0x61, 0xE2, 0x82]), vec![0x61, 0x00E2, 0x0082]);
}

/// `Tcl_UtfAtIndex` indexes in the same units the matching counter counts, and
/// the two slots differ by exactly the surrogate correction
/// (`generic/tclUtf.c:1130-1135`).
#[test]
fn indexing_agrees_with_the_counter_that_shares_its_units() {
    let s = c"a\u{1F600}b";
    unsafe {
        // Code units: index 0 is 'a', 1 and 2 are the two halves of the pair,
        // 3 is 'b'. Index 2 lands past the whole four-byte sequence.
        assert_eq!(*utf16::utf_at_index(s.as_ptr(), 0), b'a' as c_char);
        assert_eq!(*utf16::utf_at_index(s.as_ptr(), 3), b'b' as c_char);
        // Code points: index 1 is the emoji, index 2 is 'b'.
        assert_eq!(*utf16::utf_at_index_cp(s.as_ptr(), 0), b'a' as c_char);
        assert_eq!(*utf16::utf_at_index_cp(s.as_ptr(), 2), b'b' as c_char);
    }
}

// ---------------------------------------------------------------------------
// The session
// ---------------------------------------------------------------------------

/// Run `tk-host` with the given arguments, or `None` if there is no Tk.
///
/// stdin is a pipe rather than the inherited one on purpose: `TkpInit` opens a
/// console window when stdin is a character device with no blocks *and* there
/// is no startup script (`tk9.0.4/macosx/tkMacOSXInit.c:493-494`, `:585`), and
/// under a test harness stdin is `/dev/null`, which is exactly that. A pipe is
/// not a character device, so the session takes the branch a session started
/// from a terminal takes.
fn host(args: &[&str]) -> Option<(String, String)> {
    use std::io::Write;
    let exe = std::path::Path::new(env!("CARGO_BIN_EXE_tk-host"));
    let mut child = Command::new(exe)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn tk-host");
    let _ = child.stdin.take().unwrap().write_all(b"\n");
    let out = child.wait_with_output().expect("wait for tk-host");
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    if err.contains("no Tk dylib at") || err.contains("dlopen(") {
        eprintln!("skipping: {}", err.trim());
        return None;
    }
    Some((String::from_utf8_lossy(&out.stdout).into_owned(), err))
}

/// `Tk_Init` runs to its last statement without asking for a slot that is not
/// there, and returns.
///
/// The number is pinned because it is the measurement: a slot that stops being
/// reached, or one that starts being reached twice, is a change in what Tk was
/// told, and this is where that shows up.
#[test]
fn tk_init_returns_rather_than_stopping_on_a_missing_slot() {
    let Some((out, err)) = host(&[]) else { return };
    assert!(
        !err.contains("tktrap "),
        "the run stopped on a slot: {:?}",
        err.lines().find(|l| l.starts_with("tktrap "))
    );
    assert!(
        out.contains("tkhost Tk_Init returned 1 "),
        "expected an observed return code, got {out:?}"
    );
    // Which is TCL_ERROR, and the reason is this crate's compiler rather than
    // anything Tk asked for. See the module docs on `tclrs::tk`.
    assert!(
        out.contains("is only supported at the top level of a script"),
        "the failure moved: {out:?}"
    );
    let calls = err.lines().filter(|l| l.starts_with("tkslot ")).count();
    assert_eq!(calls, 2726, "the call count moved");
    let distinct: std::collections::BTreeSet<&str> = err
        .lines()
        .filter(|l| l.starts_with("tkslot "))
        .filter_map(|l| l.split_whitespace().nth(3))
        .collect();
    assert_eq!(distinct.len(), 71, "the distinct-slot count moved");
    assert!(
        out.contains("tkhost commands 106"),
        "the command count moved: {out:?}"
    );
}

/// The main window exists and is real: Tk answers `winfo` about it, `wm`
/// reports a geometry, and both answers come back through commands Tk
/// registered and this crate's compiler resolved at run time.
#[test]
fn the_main_window_exists_and_tk_answers_for_it() {
    let Some((out, _)) = host(&[
        r#"puts "exists=[winfo exists .] class=[winfo class .]""#,
        r#"puts "geometry=[wm geometry .]""#,
    ]) else {
        return;
    };
    assert!(out.contains("exists=1 class=Tk"), "{out:?}");
    // `wm geometry` is answered through `Tcl_ObjPrintf`, which is a variadic
    // slot: a body that ignored its arguments would print an empty string here.
    let line = out
        .lines()
        .find(|l| l.starts_with("geometry="))
        .unwrap_or_default();
    let geom = line.trim_start_matches("geometry=");
    assert!(
        geom.contains('x') && geom.contains('+'),
        "expected a WxH+X+Y geometry, got {geom:?}"
    );
}

/// A widget, packed, mapped, and the event loop spun over it — then the
/// callback Tk owns fired back into a script this crate compiled.
#[test]
fn a_packed_widget_is_mapped_and_its_callback_reaches_a_tclrs_script() {
    let Some((out, _)) = host(&[
        "button .b -text hello -command {puts CALLBACK-FIRED}",
        "pack .b",
        "--events",
        "200",
        r#"puts "mapped=[winfo ismapped .b] viewable=[winfo viewable .b] state=[wm state .]""#,
        ".b invoke",
    ]) else {
        return;
    };
    assert!(
        out.contains("mapped=1 viewable=1 state=normal"),
        "the widget did not reach the screen: {out:?}"
    );
    assert!(
        out.contains("passes, ") && out.contains("serviced, process alive"),
        "the event loop did not survive: {out:?}"
    );
    assert!(
        out.contains("CALLBACK-FIRED"),
        "Tk did not evaluate the -command script: {out:?}"
    );
}
