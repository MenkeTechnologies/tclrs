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
///
/// It moved once, from 2726 calls over 71 slots to 2737 over 75, when
/// `src/cmd_file.rs` landed the `file` command. `tkOption.c:1592` evaluates
/// `file tildeexpand ~/.Xdefaults` and reads only the completion code: while
/// `file` was not a command this frontend compiled that script failed, and Tk
/// skipped the option-file read entirely. It succeeds now, so Tk_Init walks
/// the path it walks under a real Tcl — `Tcl_TranslateFileName`, then
/// `Tcl_OpenFileChannel`, then `Tcl_PosixError` for the reason the open
/// failed — and those three slots had to be filled before it could return at
/// all. The count is still deterministic: this host has no channels, so the
/// open always fails and the option file is never read, whatever the machine
/// has in its home directory.
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
    // Which is TCL_ERROR, and the reason is this crate's frontend rather than
    // anything Tk asked for. See the module docs on `tclrs::tk`.
    //
    // Where that reason sits has moved twice. It was the compiler refusing
    // `proc tkInit {}` inside an `if`; then it was `namespace`, the first
    // command that script evaluates. Both are gone: the `proc` binds its name
    // when the branch runs (`crate::procs`), and `namespace`, `rename` and
    // `tcl_findLibrary` all answer now (`crate::cmd_namespace`,
    // `crate::cmd_source`).
    //
    // `can't read "tk_version"` stood here after that, until `tk-varbridge`
    // bridged the host's `vars` table to the interpreter's globals — Tk writes
    // both `tk_version` and `tk_patchLevel` through `Tcl_SetVar2` (slot 238,
    // `generic/tkWindow.c:1066-1067`) and the script can now read them back.
    //
    // So `tkInit` runs to its last statement, which is `tcl_findLibrary tk
    // $tk_version $tk_patchLevel tk.tcl TK_LIBRARY tk_library`
    // (`generic/tkWindow.c:3513`), and *that* is the refusal now. With no
    // `TK_LIBRARY` in the environment — which is how this test runs — the search
    // finds nothing and the message is the search's own.
    //
    // Pointed at an installed Tk it finds `tk.tcl` and reads it, and what stops
    // it there has moved: `{*}` argument expansion, which `tk.tcl` uses in eleven
    // places, is implemented now (`crate::procs::expand_call_op`), and the first
    // construct still refused is `upvar` with no level — `tk.tcl:145`, `upvar
    // ::tk::FocusGrab($index) data`, in `::tk::SetFocusGrab`. Measured:
    // `TK_LIBRARY=/opt/homebrew/lib/tk9.0 tclrs --tk` reports
    // `"upvar" with no level is not supported` for it.
    //
    // The refusal is asserted rather than the whole message because the search
    // path is absolute and depends on where the binary was built.
    //
    // The call and slot counts below did not move with any of it: the whole
    // failure is still on this side of the stub table, so Tk asked for exactly
    // what it asked for before.
    assert!(
        out.contains("Can't find a usable tk.tcl in the following directories")
            || out.contains("{*} argument expansion is not supported yet"),
        "the failure moved: {out:?}"
    );
    let calls = err.lines().filter(|l| l.starts_with("tkslot ")).count();
    assert_eq!(calls, 2737, "the call count moved");
    let distinct: std::collections::BTreeSet<&str> = err
        .lines()
        .filter(|l| l.starts_with("tkslot "))
        .filter_map(|l| l.split_whitespace().nth(3))
        .collect();
    assert_eq!(distinct.len(), 75, "the distinct-slot count moved");
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

/// A synthetic mouse click reaches a callback this crate compiled.
///
/// This is the whole path, and every step of it is Tk's rather than a
/// simulation: `event generate` builds an `XEvent` and hands it to
/// `Tk_HandleEvent`; `Tk_BindEvent` matches it against the binding table and
/// finds the `Button` class binding; the script is evaluated through
/// `Tcl_EvalEx`, which is this crate's compiler and fusevm; that script calls
/// `.b invoke`, which is Tk's own button command; and *that* evaluates the
/// `-command` body, again through this crate. The `puts` at the end is fusevm
/// writing to the process's stdout.
///
/// Three slots had to exist before any of it could happen, and each stopped the
/// run dead when it did not:
///
/// * `Tcl_SaveInterpState` / `Tcl_RestoreInterpState` (535/536), which
///   `Tk_BindEvent` takes around every binding script it evaluates
///   (`tk9.0.4/generic/tkBind.c:2554`, `:2608`);
/// * `Tcl_AppendObjToErrorInfo` (574), which the same function reaches through
///   `Tcl_AddErrorInfo` when a binding fails (`:2590`);
/// * `Tcl_BackgroundException` (631), where that failure then goes (`:2591`) —
///   a binding script has no caller to return an error to.
///
/// The class binding is written here because `tk.tcl` is what would otherwise
/// have written it, and `tk.tcl` does not load yet — see the module docs on
/// `tclrs::tk`. That is the only part of this a real application would not
/// have to do for itself.
#[test]
fn a_generated_click_reaches_a_callback_through_a_class_binding() {
    let Some((out, err)) = host(&[
        "button .b -text hello -command {puts CALLBACK-FIRED}",
        "pack .b",
        // What tk.tcl's button.tcl binds, reduced to the part under test: a
        // release over the widget invokes it.
        "bind Button <ButtonRelease-1> {.b invoke}",
        "update",
        "event generate .b <Button-1>",
        "event generate .b <ButtonRelease-1>",
        "update",
    ]) else {
        return;
    };
    assert!(
        !err.contains("tktrap "),
        "the click stopped on a slot: {:?}",
        err.lines().find(|l| l.starts_with("tktrap "))
    );
    assert!(
        out.contains("CALLBACK-FIRED"),
        "the generated click did not reach the -command body: {out:?}"
    );
    // `<Button-1>` reports one background error, and it names what is missing
    // rather than something about this host: `::tk::ScreenChanged` is defined
    // by `tk.tcl` (`tk9.0.4/library/tk.tcl`), which is the file that does not
    // load. The click still completes, because Tk reports a failed binding and
    // carries on — which is exactly what `Tcl_BackgroundException` is for.
    assert!(
        err.contains(r#"tkbgerror 1 invalid command name "::tk::ScreenChanged""#),
        "the background error is not the missing tk.tcl one: {err:?}"
    );
}
