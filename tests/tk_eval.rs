//! The evaluation bridge and the foreign command table, exercised through the
//! stub table itself.
//!
//! Nothing here loads Tk. That is deliberate: the contract phase 2 implements
//! is "a caller reaching this crate through `TclStubs` gets an interpreter",
//! and Tk is one such caller, not the definition of one. Every call below goes
//! through the same array of function pointers Tk is handed, transmuted back to
//! the signature `tclDecls.h` gives that slot — so a slot installed at the
//! wrong index, or with the wrong signature, fails here rather than inside a
//! toolkit.
//!
//! The one global fact these share is that a process may build one host: the
//! stub tables and the primary interpreter are process-wide
//! ([`host::build_hosting`] says so). So the host is built once, in a
//! [`OnceLock`], and the tests are written not to collide over names.

#![cfg(feature = "tk")]

use std::ffi::{c_char, c_int, c_void, CStr};
use std::ptr;
use std::sync::{Mutex, MutexGuard, OnceLock};

use tclrs::tk::abi::{TclObj, TclStubs, TCL_ERROR, TCL_OK};
use tclrs::tk::{eval, host};

/// One test at a time.
///
/// The interpreter result is a single field of a single `Host`
/// (`Tcl_GetObjResult`, slot 166), so "evaluate, then read the result" is two
/// steps over shared state and the harness runs tests on threads. Without this
/// the tests read each other's answers — measured: two of them failed under
/// `cargo test` and passed under `--test-threads=1`.
fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// The one host this test process builds, as a `Tcl_Interp *`.
fn interp() -> *mut c_void {
    static INTERP: OnceLock<usize> = OnceLock::new();
    *INTERP.get_or_init(|| host::build_hosting() as usize) as *mut c_void
}

/// The stub table the host installed — the same array Tk reads out of the
/// interpreter at offset 24.
fn table() -> &'static TclStubs {
    unsafe {
        &*(*(interp() as *mut tclrs::tk::host::HostInterp))
            .prefix
            .stub_table
    }
}

/// The function at the named slot, as `f`'s type.
///
/// # Safety
/// `F` must be the signature `tclDecls.h` gives that slot; each caller quotes
/// the declaration it read.
unsafe fn slot<F: Copy>(name: &str) -> F {
    let raw = table().slots[host::slot_index(name)];
    assert_eq!(
        std::mem::size_of::<F>(),
        std::mem::size_of::<*const c_void>(),
        "a slot is a single function pointer"
    );
    *(&raw as *const _ as *const F)
}

// ── the slots these tests call, with the header lines they came from ──────

/// `int Tcl_EvalEx(Tcl_Interp *, const char *, Tcl_Size, int)` —
/// `generic/tclDecls.h:778-779`.
type EvalEx = unsafe extern "C" fn(*mut c_void, *const c_char, isize, c_int) -> c_int;
/// `int Tcl_EvalObjv(Tcl_Interp *, Tcl_Size, Tcl_Obj *const [], int)` —
/// `generic/tclDecls.h:781-782`.
type EvalObjv = unsafe extern "C" fn(*mut c_void, isize, *const *mut TclObj, c_int) -> c_int;
/// `Tcl_Interp *Tcl_CreateInterp(void)` — `generic/tclDecls.h`, slot 94.
type CreateInterp = unsafe extern "C" fn() -> *mut c_void;
/// `void Tcl_DeleteInterp(Tcl_Interp *)` — slot 110.
type DeleteInterp = unsafe extern "C" fn(*mut c_void);
/// `Tcl_Obj *Tcl_GetObjResult(Tcl_Interp *)` — slot 166.
type GetObjResult = unsafe extern "C" fn(*mut c_void) -> *mut TclObj;
/// `Tcl_Obj *Tcl_NewStringObj(const char *, Tcl_Size)` — slot 56.
type NewStringObj = unsafe extern "C" fn(*const c_char, isize) -> *mut TclObj;
/// `void Tcl_SetObjResult(Tcl_Interp *, Tcl_Obj *)` — slot 235.
type SetObjResult = unsafe extern "C" fn(*mut c_void, *mut TclObj);
/// `char *Tcl_GetStringFromObj(Tcl_Obj *, Tcl_Size *)` — slot 651.
type GetStringFromObj = unsafe extern "C" fn(*mut TclObj, *mut isize) -> *mut c_char;
/// `Tcl_Command Tcl_CreateObjCommand(Tcl_Interp *, const char *,
/// Tcl_ObjCmdProc *, void *, Tcl_CmdDeleteProc *)` — slot 96.
type CreateObjCommand = unsafe extern "C" fn(
    *mut c_void,
    *const c_char,
    *mut c_void,
    *mut c_void,
    *mut c_void,
) -> *mut c_void;

/// The interpreter result of `interp`, as a `String`.
unsafe fn result_of(i: *mut c_void) -> String {
    let get: GetObjResult = slot("tcl_GetObjResult");
    let obj = get(i);
    let get_string: GetStringFromObj = slot("tcl_GetStringFromObj");
    CStr::from_ptr(get_string(obj, ptr::null_mut()))
        .to_string_lossy()
        .into_owned()
}

/// Evaluate `src` through slot 291 and return `(code, result)`.
unsafe fn eval_ex(i: *mut c_void, src: &str) -> (c_int, String) {
    let f: EvalEx = slot("tcl_EvalEx");
    let c = std::ffi::CString::new(src).unwrap();
    // `TCL_INDEX_NONE` (`generic/tcl.h:2292`) and `TCL_EVAL_GLOBAL`
    // (`generic/tcl.h:985`) — exactly what `tkOption.c:1592` passes.
    let code = f(i, c.as_ptr(), -1, eval::TCL_EVAL_GLOBAL);
    (code, result_of(i))
}

#[test]
fn a_script_handed_over_is_compiled_and_run() {
    let _serial = serial();
    unsafe {
        let i = interp();
        // Arithmetic, a variable, and a value that survives into the next
        // evaluation: the three things that separate an evaluator from a
        // placeholder that echoes its argument.
        let (code, result) = eval_ex(i, "set tk_eval_a [expr {6 * 7}]");
        assert_eq!((code, result.as_str()), (TCL_OK, "42"));
        let (code, result) = eval_ex(i, "string length $tk_eval_a");
        assert_eq!((code, result.as_str()), (TCL_OK, "2"));
    }
}

#[test]
fn a_failing_script_is_an_error_with_the_message_as_the_result() {
    let _serial = serial();
    unsafe {
        let i = interp();
        // The script here was `file tildeexpand ~/.Xdefaults` — the exact one
        // Tk asks for at `tk9.0.4/generic/tkOption.c:1592` — until
        // `src/cmd_file.rs` landed `file`, and that script now succeeds. What
        // this test is about is the error path, not that particular script, so
        // it moved to a command the frontend still has no implementation of.
        // `uplevel` is the same stand-in `tests/execution_differential.rs`
        // uses, so the two move together when it is built.
        //
        // The Tk call site is checked below: it reads only the completion code
        // and skips the `.Xdefaults` read when it is not TCL_OK, so either
        // answer keeps `Tk_Init` going, and it is TCL_OK now.
        let (code, result) = eval_ex(i, "uplevel 1 {set x 1}");
        assert_eq!(code, TCL_ERROR);
        assert!(result.contains("invalid command name"), "{result:?}");
        let (code, _) = eval_ex(i, "file tildeexpand ~/.Xdefaults");
        assert_eq!(code, TCL_OK);
    }
}

#[test]
fn the_second_interpreter_is_independent_and_can_be_deleted() {
    let _serial = serial();
    unsafe {
        let primary = interp();
        let create: CreateInterp = slot("tcl_CreateInterp");
        let child = create();
        assert!(!child.is_null());
        assert_ne!(child, primary);

        // Tk creates this one to hold an option database and throws it away
        // (`tk9.0.4/generic/tkOption.c:1496-1499`). If its variables were the
        // primary's, that database would leak into the application's globals.
        let (code, _) = eval_ex(child, "set tk_eval_only_in_child 1");
        assert_eq!(code, TCL_OK);
        // Reading an unset variable is an error, so `catch` answering 1 in the
        // primary and 0 in the child is the two stores being separate.
        let (code, result) = eval_ex(primary, "catch {set tk_eval_only_in_child}");
        assert_eq!((code, result.as_str()), (TCL_OK, "1"));

        let (code, result) = eval_ex(child, "catch {set tk_eval_only_in_child}");
        assert_eq!((code, result.as_str()), (TCL_OK, "0"));

        let delete: DeleteInterp = slot("tcl_DeleteInterp");
        delete(child);
    }
}

/// The variadic slot, called the way `TkMakeEnsemble` calls it.
///
/// This is the one that cannot be written in Rust at all, so it is also the one
/// where "it compiled" proves nothing. `tk9.0.4/generic/tkUtil.c:1222` is
/// `Tcl_AppendStringsToObj(fqdnObj, "::", map[i].name, (char *)NULL)`, and the
/// name it builds is what the subcommand is registered under: a body that
/// ignored the arguments would register every subcommand of every ensemble
/// under the ensemble's own name.
#[test]
fn the_trampoline_appends_every_variadic_string() {
    let _serial = serial();
    unsafe {
        let new_string: NewStringObj = slot("tcl_NewStringObj");
        let obj = new_string(c"::tk::fontchooser".as_ptr(), -1);

        // The call must be *declared* variadic, not merely passed three extra
        // pointers. Rust can declare a C-variadic function pointer on stable
        // even though it cannot define one, and the distinction is not
        // cosmetic: under AAPCS64 a variadic argument goes on the stack where a
        // fixed one of the same position would go in a register, so calling
        // this slot through a fixed four-argument type hands the trampoline
        // three registers it never reads and an empty stack it does. That
        // mismatch is a segfault, which is how this line was arrived at.
        let append: unsafe extern "C" fn(*mut TclObj, ...) = slot("tcl_AppendStringsToObj");
        append(
            obj,
            c"::".as_ptr(),
            c"configure".as_ptr(),
            ptr::null::<c_char>(),
        );

        let get_string: GetStringFromObj = slot("tcl_GetStringFromObj");
        let text = CStr::from_ptr(get_string(obj, ptr::null_mut()))
            .to_string_lossy()
            .into_owned();
        assert_eq!(text, "::tk::fontchooser::configure");
    }
}

// ── the foreign command table ────────────────────────────────────────────

/// A `Tcl_ObjCmdProc` (`generic/tcl.h:587-588`) that joins its arguments with
/// `+` and answers with `clientData` prefixed, so a test can tell the
/// arguments, their order and the client data apart in one string.
unsafe extern "C" fn joining_command(
    client_data: *mut c_void,
    i: *mut c_void,
    objc: c_int,
    objv: *const *mut TclObj,
) -> c_int {
    let get_string: GetStringFromObj = slot("tcl_GetStringFromObj");
    let mut parts = Vec::new();
    for k in 0..objc {
        let w = *objv.offset(k as isize);
        parts.push(
            CStr::from_ptr(get_string(w, ptr::null_mut()))
                .to_string_lossy()
                .into_owned(),
        );
    }
    // `clientData` is an opaque `void *` that Tcl hands back untouched
    // (`generic/tcl.h:587`). Tk passes real pointers through it — a `TkWindow *`
    // for every widget command — so these tests pass one too, a static C string,
    // and read it back. An integer cast to a pointer would test a narrower
    // thing and is a dangling pointer besides.
    let tag = if client_data.is_null() {
        "nil".to_string()
    } else {
        CStr::from_ptr(client_data as *const c_char)
            .to_string_lossy()
            .into_owned()
    };
    let text = format!("{tag}|{}", parts.join("+"));
    let new_string: NewStringObj = slot("tcl_NewStringObj");
    let set: SetObjResult = slot("tcl_SetObjResult");
    let c = std::ffi::CString::new(text).unwrap();
    set(i, new_string(c.as_ptr(), -1));
    TCL_OK
}

/// A command that fails, so the error path is measured rather than assumed.
unsafe extern "C" fn failing_command(
    _client_data: *mut c_void,
    i: *mut c_void,
    _objc: c_int,
    _objv: *const *mut TclObj,
) -> c_int {
    let new_string: NewStringObj = slot("tcl_NewStringObj");
    let set: SetObjResult = slot("tcl_SetObjResult");
    set(i, new_string(c"widget is not mapped".as_ptr(), -1));
    TCL_ERROR
}

/// Register `name` through slot 96, exactly as Tk does.
unsafe fn register(name: &CStr, proc_: *mut c_void, client_data: *mut c_void) {
    let create: CreateObjCommand = slot("tcl_CreateObjCommand");
    let token = create(interp(), name.as_ptr(), proc_, client_data, ptr::null_mut());
    assert!(!token.is_null(), "Tcl_CreateObjCommand returned no token");
}

/// The whole point of phase 3: a name that did not exist when this crate's
/// compiler started, called from a script this crate compiled.
#[test]
fn a_registered_command_is_callable_from_a_compiled_script() {
    let _serial = serial();
    unsafe {
        register(
            c"tkeval_join",
            joining_command as *mut c_void,
            c"cd".as_ptr() as *mut c_void,
        );
    }
    let mut i = tclrs::Interp::capturing();

    // objv[0] is the command name and objc counts it, which is Tcl's
    // convention (`generic/tclBasic.c` invokes with the words as written), so
    // two arguments arrive as three words.
    assert_eq!(
        i.eval("tkeval_join alpha beta").unwrap(),
        "cd|tkeval_join+alpha+beta"
    );

    // Arguments are substituted before the dispatch, as for any other command.
    assert_eq!(
        i.eval("set w beta; tkeval_join [string toupper alpha] $w")
            .unwrap(),
        "cd|tkeval_join+ALPHA+beta"
    );

    // And the value comes back as an ordinary Tcl value, usable by the rest of
    // the script — it is not a string printed and forgotten.
    assert_eq!(i.eval("string length [tkeval_join a]").unwrap(), "16");
}

#[test]
fn a_failing_registered_command_raises_its_result_as_the_error() {
    let _serial = serial();
    unsafe {
        register(
            c"tkeval_fail",
            failing_command as *mut c_void,
            ptr::null_mut(),
        );
    }
    let mut i = tclrs::Interp::capturing();
    let err = i.eval("tkeval_fail .b").unwrap_err();
    assert_eq!(err.msg, "widget is not mapped");

    // And it is catchable, like any other error a command raises.
    assert_eq!(i.eval("catch {tkeval_fail .b} m").unwrap(), "1");
}

/// Re-registering a name replaces it rather than shadowing it.
///
/// Tcl deletes the old command first (`generic/tclBasic.c`'s
/// `Tcl_CreateObjCommand2`). Appending instead would leave the *first*
/// registration winning every lookup, which is the wrong way round and is the
/// kind of thing that only shows up once a package re-registers on reload.
#[test]
fn re_registering_a_name_replaces_the_command() {
    let _serial = serial();
    unsafe {
        register(
            c"tkeval_twice",
            joining_command as *mut c_void,
            c"first".as_ptr() as *mut c_void,
        );
        register(
            c"tkeval_twice",
            joining_command as *mut c_void,
            c"second".as_ptr() as *mut c_void,
        );
    }
    let mut i = tclrs::Interp::capturing();
    assert_eq!(i.eval("tkeval_twice").unwrap(), "second|tkeval_twice");
}

/// `Tcl_EvalObjv` carries an already-parsed command, so its words must not be
/// re-split. A word holding a space is the test that tells the two apart.
#[test]
fn eval_objv_invokes_a_registered_command_without_re_parsing() {
    let _serial = serial();
    unsafe {
        register(
            c"tkeval_objv",
            joining_command as *mut c_void,
            ptr::null_mut(),
        );
        let new_string: NewStringObj = slot("tcl_NewStringObj");
        let words = [
            new_string(c"tkeval_objv".as_ptr(), -1),
            new_string(c"one two".as_ptr(), -1),
            new_string(c"$notavariable".as_ptr(), -1),
        ];
        let f: EvalObjv = slot("tcl_EvalObjv");
        let code = f(interp(), words.len() as isize, words.as_ptr(), 0);
        assert_eq!(code, TCL_OK);
        assert_eq!(result_of(interp()), "nil|tkeval_objv+one two+$notavariable");
    }
}

/// A name nothing is registered under still fails the way it always failed.
///
/// The dispatch op replaced a refusal the compiler used to defer, so its
/// wording *and* its line have to be the ones that refusal carried — the line
/// is what `tclrs` prints as `(file "…" line N)`.
#[test]
fn an_unregistered_name_raises_the_error_the_compiler_would_have() {
    let _serial = serial();
    // Force the host into existence first, so the lowering is the dynamic one.
    interp();
    let mut i = tclrs::Interp::capturing();
    let err = i.eval("puts hi\nnosuchtkcommand a b").unwrap_err();
    assert_eq!(err.msg, "invalid command name \"nosuchtkcommand\"");
    assert_eq!(err.line, Some(2), "the located line was lost");

    // Tcl substitutes every word before dispatching on the first, so the
    // arguments run even though the command does not exist.
    let mut i = tclrs::Interp::capturing();
    let _ = i.eval("nosuchtkcommand [puts inner]");
    assert_eq!(i.take_output(), "inner\n");
}
