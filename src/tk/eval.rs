//! The evaluation slots: Tk hands over a script, tclrs compiles and runs it.
//!
//! This is the boundary phase 1 measured its way to. Everything Tk asked for
//! before it could be answered with a data structure; `Tcl_EvalEx(interp, "file
//! tildeexpand ~/.Xdefaults", TCL_INDEX_NONE, TCL_EVAL_GLOBAL)`
//! (`tk9.0.4/generic/tkOption.c:1592`) cannot be, and it is where the honest
//! run stopped.
//!
//! # How a script re-enters tclrs
//!
//! There is no second evaluator here and no interpretation of Tcl text by this
//! module. A script arrives as bytes, and goes through the same three stages a
//! script from the command line goes through:
//!
//! 1. [`crate::parser::parse`], through the interpreter's chunk cache, so a
//!    script Tk evaluates twice is compiled once;
//! 2. [`crate::compiler`], which lowers it to a `fusevm::Chunk`;
//! 3. `crate::runtime::run_source`, which runs that chunk on a fusevm VM with
//!    the same numeric hook, extension handler and JIT arming every other
//!    evaluation gets.
//!
//! The value of the last command becomes the interpreter result, readable by Tk
//! through `Tcl_GetObjResult` (slot 166); a failure becomes `TCL_ERROR` with
//! the message as the result, which is Tcl's contract
//! (`generic/tclBasic.c`: the result holds the error message after a failed
//! evaluation).
//!
//! # `TCL_EVAL_GLOBAL`
//!
//! `TCL_EVAL_GLOBAL` is `0x020000` (`generic/tcl.h:985`) and means "evaluate in
//! the global namespace rather than the caller's frame". Every evaluation
//! started here *is* global: `crate::runtime::run_source` runs a chunk whose
//! variables are the interpreter's globals, and a procedure's locals live in
//! fusevm frame slots that a chunk addresses by index, so they are not
//! reachable by name from outside the chunk that declared them. The flag is
//! therefore honoured by construction and the bit is recorded rather than
//! branched on — and a script Tk evaluates without the flag from inside a tclrs
//! procedure would see the globals too, which is the one place this differs
//! from Tcl and is stated here rather than hidden.
//!
//! # The other three variadic slots
//!
//! Four of the seven are marshalled by `trampoline.c` because their variadic
//! arguments carry the payload: `Tcl_AppendStringsToObj` (15), `Tcl_Panic` (2),
//! `Tcl_ObjPrintf` (578) and `Tcl_AppendPrintfToObj` (579). The rest are not,
//! and the reason is the same for each: Tk calls them only to build text that a
//! *script* would read, and no script reads it during initialisation.
//!
//! * `Tcl_SetErrorCode` (slot 228) — sets `::errorCode`, read by `catch`
//!   handlers. Tk sets one where it is about to fail
//!   (`tk9.0.4/generic/tkWindow.c:2795`).
//! * `Tcl_AppendResult` (slot 70) — appends to the interpreter result, and Tk
//!   uses it for error text only.
//! * `Tcl_VarEval` (slot 260) — Tk never calls it. `grep -r Tcl_VarEval` over
//!   `tk9.0.4/{generic,macosx,unix,ttk}` finds no call site.
//!
//! Ignoring the variadic arguments of a function one *defines* is well formed
//! on both calling conventions this targets, because the caller lays them out
//! and tears them down; under AAPCS64 in particular they go on the stack while
//! the fixed arguments stay in registers, so reading the fixed ones is
//! unaffected. What is lost is only the text — never the control flow, and
//! never the validity of a returned object.
//!
//! The two printf slots were once on that list, on the argument that Tk's 443
//! and 93 call sites are all error wording. They are not: `wm geometry .`
//! answers with `Tcl_ObjPrintf("%dx%d+%d+%d", …)`, and `bind Button` rebuilds
//! every pattern it reports through `Tcl_AppendPrintfToObj`
//! (`tk9.0.4/generic/tkBind.c:5190,5212`). Both are ordinary results a script
//! reads, so both are marshalled.

use std::ffi::{c_char, c_int, c_void};

use super::abi::{TclObj, TCL_ERROR, TCL_OK};
use super::host;
use super::interp;

/// `TCL_EVAL_GLOBAL` (`generic/tcl.h:985`).
pub const TCL_EVAL_GLOBAL: c_int = 0x0002_0000;

/// Compile and run `src` in the interpreter behind `interp`, leaving the value
/// or the error message as the interpreter result.
///
/// # Safety
/// `interp` is a `Tcl_Interp *` this crate handed to Tk.
pub unsafe fn eval_script(interp_ptr: *mut c_void, src: &str) -> c_int {
    let host_ptr = interp::host_of(interp_ptr);
    if host_ptr.is_null() {
        // Tcl treats a NULL interpreter as "nowhere to report to" in the slots
        // that accept one, but there is nothing to evaluate *in*.
        return TCL_ERROR;
    }
    let shared = interp::shared_for(host_ptr);
    // Scoped, so a command the script calls knows which interpreter it belongs
    // to, and so the entry is removed however this returns.
    let _scope = interp::Scope::enter(interp_ptr as *mut super::host::HostInterp);

    match crate::runtime::run_source(&shared, src) {
        Ok(value) => {
            let text = crate::runtime::to_tcl_string(&value);
            host::set_result_bytes(interp_ptr, text.as_bytes());
            TCL_OK
        }
        Err(e) => {
            host::set_result_bytes(interp_ptr, e.msg.as_bytes());
            TCL_ERROR
        }
    }
}

/// Slot 291. `int Tcl_EvalEx(Tcl_Interp *, const char *script, Tcl_Size
/// numBytes, int flags)` — `generic/tclDecls.h:778-779`.
///
/// `numBytes` is `TCL_INDEX_NONE` (`(Tcl_Size)-1`, `generic/tcl.h:2292`) for
/// "measure it", which is what both of Tk's call sites pass
/// (`tk9.0.4/generic/tkOption.c:1592`, `tk9.0.4/generic/tkWindow.c:3508`).
///
/// `Tcl_Eval` and `Tcl_GlobalEval` are macros over this slot, with `0` and
/// `TCL_EVAL_GLOBAL` respectively (`generic/tclDecls.h:3966-3969`), so it also
/// serves both of those.
/// # Safety
/// `interp_ptr` is a `Tcl_Interp *` this crate handed to Tk, and `script` is
/// `num_bytes` readable bytes — or a NUL-terminated string when `num_bytes` is
/// negative, which is `TCL_INDEX_NONE`.
pub unsafe extern "C" fn eval_ex(
    interp_ptr: *mut c_void,
    script: *const c_char,
    num_bytes: isize,
    flags: c_int,
) -> c_int {
    super::trace::record(super::trace::Table::Tcl, host::slot_index("tcl_EvalEx"));
    let bytes = host::c_bytes_of(script, num_bytes);
    let src = String::from_utf8_lossy(bytes).into_owned();
    super::trace::note("EvalEx", &src);
    let _ = flags & TCL_EVAL_GLOBAL;
    eval_script(interp_ptr, &src)
}

/// Slot 293. `int Tcl_EvalObjEx(Tcl_Interp *, Tcl_Obj *objPtr, int flags)` —
/// `generic/tclDecls.h:784-785`. `Tcl_EvalObj` and `Tcl_GlobalEvalObj` are
/// macros over it (`generic/tclDecls.h:4172-4174`).
///
/// Tk builds the script as a *list* — `Tcl_NewStringObj("wm geometry .")` then
/// `Tcl_ListObjAppendElement` (`tk9.0.4/generic/tkWindow.c:3446-3449`) — so the
/// value's string rep is the script, which is what this evaluates.
/// # Safety
/// `interp_ptr` is a `Tcl_Interp *` this crate handed to Tk, and `obj` is null
/// or a live `Tcl_Obj`.
pub unsafe extern "C" fn eval_obj_ex(
    interp_ptr: *mut c_void,
    obj: *mut TclObj,
    flags: c_int,
) -> c_int {
    super::trace::record(super::trace::Table::Tcl, host::slot_index("tcl_EvalObjEx"));
    if obj.is_null() {
        return TCL_ERROR;
    }
    let src = String::from_utf8_lossy(host::obj_bytes_of(obj)).into_owned();
    super::trace::note("EvalObjEx", &src);
    let _ = flags & TCL_EVAL_GLOBAL;
    eval_script(interp_ptr, &src)
}

/// Slot 292. `int Tcl_EvalObjv(Tcl_Interp *, Tcl_Size objc, Tcl_Obj *const
/// objv[], int flags)` — `generic/tclDecls.h:781-782`.
///
/// Unlike the other two this one carries an *already parsed* command: the words
/// are given, and no substitution is to be performed on them
/// (`generic/tclBasic.c`'s `Tcl_EvalObjv` dispatches on `objv[0]` directly).
/// So it takes the two routes a word list can take, in the order Tcl takes
/// them:
///
/// 1. `objv[0]` names a command Tk registered — call it, with these exact
///    words. No re-parse, so a word containing a space or a `$` stays one
///    word;
/// 2. otherwise, the words are joined as a Tcl list and evaluated as a script.
///    Joining quotes each word, so the split the parser performs gives back the
///    words that went in — which is why this is a faithful reproduction of the
///    invocation and not a second, looser parse.
/// # Safety
/// `interp_ptr` is a `Tcl_Interp *` this crate handed to Tk, and `objv` holds
/// `objc` live `Tcl_Obj` pointers.
pub unsafe extern "C" fn eval_objv(
    interp_ptr: *mut c_void,
    objc: isize,
    objv: *const *mut TclObj,
    _flags: c_int,
) -> c_int {
    super::trace::record(super::trace::Table::Tcl, host::slot_index("tcl_EvalObjv"));
    if objc <= 0 || objv.is_null() {
        return TCL_OK;
    }
    let words: Vec<*mut TclObj> = (0..objc).map(|i| *objv.offset(i)).collect();
    let name = String::from_utf8_lossy(host::obj_bytes_of(words[0])).into_owned();
    super::trace::note("EvalObjv", &name);

    let host_ptr = interp::host_of(interp_ptr);
    if host_ptr.is_null() {
        return TCL_ERROR;
    }
    if host::command_named(host_ptr, &name).is_some() {
        let _scope = interp::Scope::enter(interp_ptr as *mut super::host::HostInterp);
        return match super::dispatch::invoke_objv(interp_ptr, &name, &words) {
            Ok(code) => code,
            Err(msg) => {
                host::set_result_bytes(interp_ptr, msg.as_bytes());
                TCL_ERROR
            }
        };
    }

    let text: Vec<String> = words
        .iter()
        .map(|w| String::from_utf8_lossy(host::obj_bytes_of(*w)).into_owned())
        .collect();
    eval_script(interp_ptr, &crate::list::join(&text))
}

// ---------------------------------------------------------------------------
// The C trampoline's two callbacks
// ---------------------------------------------------------------------------

/// What `tclrs_tk_append_strings_to_obj` in `trampoline.c` hands back: the
/// object and the NULL-terminated argument list it walked, as a counted array.
///
/// # Safety
/// Called only from that function, with `count` valid entries.
#[no_mangle]
pub unsafe extern "C" fn tclrs_tk_append_strings(
    obj: *mut TclObj,
    strings: *const *const c_char,
    count: usize,
) {
    super::trace::record(
        super::trace::Table::Tcl,
        host::slot_index("tcl_AppendStringsToObj"),
    );
    if obj.is_null() {
        return;
    }
    for i in 0..count {
        let p = *strings.add(i);
        host::append_bytes_to_obj(obj, host::c_bytes_of(p, -1));
    }
    super::trace::note(
        "AppendStringsToObj",
        &String::from_utf8_lossy(host::obj_bytes_of(obj)),
    );
}

/// What `tclrs_tk_panic_trampoline` hands back: the formatted message.
///
/// `Tcl_Panic` is declared `TCL_NORETURN` (`generic/tclDecls.h:62`) and Tcl's
/// own body aborts (`generic/tclPanic.c`), so returning from here would be a
/// contract violation on top of whatever made Tk panic.
///
/// # Safety
/// Called only from that function, with a NUL-terminated buffer.
#[no_mangle]
pub unsafe extern "C" fn tclrs_tk_panic(text: *const c_char) -> ! {
    use std::io::Write;
    let msg = String::from_utf8_lossy(host::c_bytes_of(text, -1)).into_owned();
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "tkpanic {msg}");
    let _ = err.flush();
    std::process::abort()
}

/// What `tclrs_tk_obj_printf` hands back: the finished text, to be wrapped in
/// a value the way `Tcl_ObjPrintf` does (`generic/tclStringObj.c:2937-2943`).
///
/// The value is returned unreferenced, which is `TclNewObj`'s contract
/// (`generic/tclObj.c:1149-1151`) and what every caller of `Tcl_ObjPrintf`
/// expects: Tk hands the result straight to `Tcl_SetObjResult`, which takes the
/// first reference.
///
/// # Safety
/// Called only from that function, with `length` readable bytes at `text`.
#[no_mangle]
pub unsafe extern "C" fn tclrs_tk_new_string_obj(
    text: *const c_char,
    length: usize,
) -> *mut TclObj {
    super::trace::record(super::trace::Table::Tcl, host::slot_index("tcl_ObjPrintf"));
    let bytes = std::slice::from_raw_parts(text as *const u8, length);
    super::trace::note("ObjPrintf", &String::from_utf8_lossy(bytes));
    super::obj::new_string(bytes)
}

/// What `tclrs_tk_append_printf_to_obj` hands back: the finished text, to be
/// appended the way `Tcl_AppendPrintfToObj` appends
/// (`generic/tclStringObj.c:2904-2915`, which is `Tcl_ObjPrintf`'s body over a
/// caller's value rather than a fresh one).
///
/// A NULL value is Tk's own precondition rather than this side's: every one of
/// its 93 call sites passes an object it has just built, and Tcl's body
/// dereferences without a check. It is refused here anyway, because the
/// alternative is a wild write.
///
/// # Safety
/// Called only from that function, with `length` readable bytes at `text`.
#[no_mangle]
pub unsafe extern "C" fn tclrs_tk_append_printf(
    obj: *mut TclObj,
    text: *const c_char,
    length: usize,
) {
    super::trace::record(
        super::trace::Table::Tcl,
        host::slot_index("tcl_AppendPrintfToObj"),
    );
    if obj.is_null() {
        return;
    }
    let bytes = std::slice::from_raw_parts(text as *const u8, length);
    host::append_bytes_to_obj(obj, bytes);
    super::trace::note(
        "AppendPrintfToObj",
        &String::from_utf8_lossy(host::obj_bytes_of(obj)),
    );
}

extern "C" {
    /// `void tclrs_tk_append_strings_to_obj(void *objPtr, ...)`, the body
    /// installed at slot 15.
    pub fn tclrs_tk_append_strings_to_obj(obj: *mut TclObj, ...);
    /// `void tclrs_tk_panic_trampoline(const char *format, ...)`, the body
    /// installed at slot 2.
    pub fn tclrs_tk_panic_trampoline(format: *const c_char, ...);
    /// `void *tclrs_tk_obj_printf(const char *format, ...)`, the body installed
    /// at slot 578.
    pub fn tclrs_tk_obj_printf(format: *const c_char, ...) -> *mut TclObj;
    /// `void tclrs_tk_append_printf_to_obj(void *objPtr, const char *format,
    /// ...)`, the body installed at slot 579.
    pub fn tclrs_tk_append_printf_to_obj(obj: *mut TclObj, format: *const c_char, ...);
}
