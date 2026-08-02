//! The variable bridge, exercised through the stub table.
//!
//! Everything here goes through `table().slots[…]`, transmuted back to the
//! signature `tclDecls.h` gives the slot, for the reason `tests/tk_eval.rs`
//! gives: a body installed at the wrong index or with the wrong signature fails
//! here rather than inside Tk, where the symptom would be a crash several frames
//! from the cause.
//!
//! None of it needs libtk. A variable trace is a C function pointer and a
//! `clientData`, and a test can supply both — which is what makes the trace
//! machinery testable without a window server, a display, or Tk at all. The one
//! thing these cannot show is a *widget* reacting, and that is demonstrated
//! separately in `tk-conformance/REPORT.md`.
//!
//! Own file rather than more cases in `tk_eval.rs`, because the host is built
//! once per process and the trace registry is process-wide: a test that leaves a
//! trace behind would be visible to every later one. Each test removes what it
//! added, and `serial()` keeps them from overlapping.

#![cfg(feature = "tk")]

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::ptr;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use tclrs::tk::abi::{TclObj, TclStubs, TCL_ERROR, TCL_OK};
use tclrs::tk::linkvar::{TCL_GLOBAL_ONLY, TCL_TRACE_READS, TCL_TRACE_UNSETS, TCL_TRACE_WRITES};
use tclrs::tk::{eval, host};

/// One test at a time: the interpreter result and the trace registry are both
/// process-wide, and cargo runs tests on threads.
fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// The one host this test process builds, as a `Tcl_Interp *`.
fn interp() -> *mut c_void {
    static INTERP: OnceLock<usize> = OnceLock::new();
    *INTERP.get_or_init(|| host::build_hosting() as usize) as *mut c_void
}

fn table() -> &'static TclStubs {
    unsafe {
        &*(*(interp() as *mut tclrs::tk::host::HostInterp))
            .prefix
            .stub_table
    }
}

/// The function at the named slot, as `F`'s type.
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

/// `int Tcl_EvalEx(Tcl_Interp *, const char *, Tcl_Size, int)` — slot 291.
type EvalEx = unsafe extern "C" fn(*mut c_void, *const c_char, isize, c_int) -> c_int;
/// `Tcl_Obj *Tcl_GetObjResult(Tcl_Interp *)` — slot 166.
type GetObjResult = unsafe extern "C" fn(*mut c_void) -> *mut TclObj;
/// `char *Tcl_GetStringFromObj(Tcl_Obj *, Tcl_Size *)` — slot 651.
type GetStringFromObj = unsafe extern "C" fn(*mut TclObj, *mut isize) -> *mut c_char;
/// `Tcl_Obj *Tcl_NewStringObj(const char *, Tcl_Size)` — slot 56.
type NewStringObj = unsafe extern "C" fn(*const c_char, isize) -> *mut TclObj;
/// `int Tcl_TraceVar2(Tcl_Interp *, const char *, const char *, int,
/// Tcl_VarTraceProc *, void *)` — `generic/tclDecls.h:2139`.
type TraceVar2 = unsafe extern "C" fn(
    *mut c_void,
    *const c_char,
    *const c_char,
    c_int,
    *mut c_void,
    *mut c_void,
) -> c_int;
/// `void Tcl_UntraceVar2(Tcl_Interp *, const char *, const char *, int,
/// Tcl_VarTraceProc *, void *)` — `generic/tclDecls.h:2147`.
type UntraceVar2 = unsafe extern "C" fn(
    *mut c_void,
    *const c_char,
    *const c_char,
    c_int,
    *mut c_void,
    *mut c_void,
);
/// `void *Tcl_VarTraceInfo2(Tcl_Interp *, const char *, const char *, int,
/// Tcl_VarTraceProc *, void *)` — `generic/tclDecls.h:2153`.
type VarTraceInfo2 = unsafe extern "C" fn(
    *mut c_void,
    *const c_char,
    *const c_char,
    c_int,
    *mut c_void,
    *mut c_void,
) -> *mut c_void;
/// `int Tcl_LinkVar(Tcl_Interp *, const char *, void *, int)` —
/// `generic/tclDecls.h:2078`.
type LinkVar = unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void, c_int) -> c_int;
/// `void Tcl_UnlinkVar(Tcl_Interp *, const char *)` — `generic/tclDecls.h:2142`.
type UnlinkVar = unsafe extern "C" fn(*mut c_void, *const c_char);
/// `void Tcl_UpdateLinkedVar(Tcl_Interp *, const char *)` —
/// `generic/tclDecls.h:2148`.
type UpdateLinkedVar = unsafe extern "C" fn(*mut c_void, *const c_char);
/// `Tcl_Obj *Tcl_ObjGetVar2(Tcl_Interp *, Tcl_Obj *, Tcl_Obj *, int)` —
/// `generic/tclDecls.h:2086`.
type ObjGetVar2 = unsafe extern "C" fn(*mut c_void, *mut TclObj, *mut TclObj, c_int) -> *mut TclObj;
/// `Tcl_Obj *Tcl_ObjSetVar2(Tcl_Interp *, Tcl_Obj *, Tcl_Obj *, Tcl_Obj *,
/// int)` — `generic/tclDecls.h:2087`.
type ObjSetVar2 =
    unsafe extern "C" fn(*mut c_void, *mut TclObj, *mut TclObj, *mut TclObj, c_int) -> *mut TclObj;
/// `Tcl_Obj *Tcl_SetVar2Ex(Tcl_Interp *, const char *, const char *, Tcl_Obj *,
/// int)` — `generic/tclDecls.h:2208`.
type SetVar2Ex = unsafe extern "C" fn(
    *mut c_void,
    *const c_char,
    *const c_char,
    *mut TclObj,
    c_int,
) -> *mut TclObj;
/// `int Tcl_UnsetVar2(Tcl_Interp *, const char *, const char *, int)` —
/// `generic/tclDecls.h:2145`.
type UnsetVar2 = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char, c_int) -> c_int;
/// `const char *Tcl_GetVar2(Tcl_Interp *, const char *, const char *, int)` —
/// slot 176.
type GetVar2 =
    unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char, c_int) -> *const c_char;

// ── helpers ──────────────────────────────────────────────────────────────

unsafe fn result_of(i: *mut c_void) -> String {
    let get: GetObjResult = slot("tcl_GetObjResult");
    let get_string: GetStringFromObj = slot("tcl_GetStringFromObj");
    CStr::from_ptr(get_string(get(i), ptr::null_mut()))
        .to_string_lossy()
        .into_owned()
}

unsafe fn eval_ex(i: *mut c_void, src: &str) -> (c_int, String) {
    let f: EvalEx = slot("tcl_EvalEx");
    let c = CString::new(src).expect("no NUL in a test script");
    (f(i, c.as_ptr(), -1, eval::TCL_EVAL_GLOBAL), result_of(i))
}

unsafe fn new_obj(text: &str) -> *mut TclObj {
    let f: NewStringObj = slot("tcl_NewStringObj");
    let c = CString::new(text).expect("no NUL in a test value");
    f(c.as_ptr(), -1)
}

unsafe fn text_of(o: *mut TclObj) -> String {
    let get_string: GetStringFromObj = slot("tcl_GetStringFromObj");
    CStr::from_ptr(get_string(o, ptr::null_mut()))
        .to_string_lossy()
        .into_owned()
}

/// Everything one trace procedure saw, in call order.
static SEEN: Mutex<Vec<(String, c_int, usize)>> = Mutex::new(Vec::new());

fn seen() -> Vec<(String, c_int, usize)> {
    SEEN.lock().expect("seen").clone()
}

fn forget_what_was_seen() {
    SEEN.lock().expect("seen").clear();
}

/// A `Tcl_VarTraceProc` that records and consents.
unsafe extern "C" fn recorder(
    client_data: *mut c_void,
    _interp: *mut c_void,
    name1: *const c_char,
    _name2: *const c_char,
    flags: c_int,
) -> *mut c_char {
    let name = CStr::from_ptr(name1).to_string_lossy().into_owned();
    SEEN.lock()
        .expect("seen")
        .push((name, flags, client_data as usize));
    ptr::null_mut()
}

/// A second one, so a test can tell two traces apart by their procedure.
unsafe extern "C" fn other_recorder(
    client_data: *mut c_void,
    interp: *mut c_void,
    name1: *const c_char,
    name2: *const c_char,
    flags: c_int,
) -> *mut c_char {
    recorder(client_data, interp, name1, name2, flags)
}

/// A `Tcl_VarTraceProc` that refuses every write.
unsafe extern "C" fn refuser(
    _client_data: *mut c_void,
    _interp: *mut c_void,
    _name1: *const c_char,
    _name2: *const c_char,
    _flags: c_int,
) -> *mut c_char {
    c"this variable may not be written".as_ptr() as *mut c_char
}

unsafe fn trace_scalar(name: &str, flags: c_int, proc_: *const (), data: usize) -> c_int {
    let f: TraceVar2 = slot("tcl_TraceVar2");
    let c = CString::new(name).expect("no NUL in a variable name");
    f(
        interp(),
        c.as_ptr(),
        ptr::null(),
        flags,
        proc_ as *mut c_void,
        data as *mut c_void,
    )
}

unsafe fn untrace_scalar(name: &str, flags: c_int, proc_: *const (), data: usize) {
    let f: UntraceVar2 = slot("tcl_UntraceVar2");
    let c = CString::new(name).expect("no NUL in a variable name");
    f(
        interp(),
        c.as_ptr(),
        ptr::null(),
        flags,
        proc_ as *mut c_void,
        data as *mut c_void,
    );
}

const WATCH_ALL: c_int = TCL_GLOBAL_ONLY | TCL_TRACE_READS | TCL_TRACE_WRITES | TCL_TRACE_UNSETS;

// ── traces ───────────────────────────────────────────────────────────────

/// The `-textvariable` contract in miniature: a script's `set` has to reach the
/// trace, or no widget option that names a variable does anything at all.
#[test]
fn a_scripts_write_reaches_a_write_trace() {
    let _serial = serial();
    unsafe {
        forget_what_was_seen();
        assert_eq!(
            trace_scalar(
                "tv_written",
                TCL_GLOBAL_ONLY | TCL_TRACE_WRITES,
                recorder as *const (),
                7
            ),
            TCL_OK
        );
        let (code, _) = eval_ex(interp(), "set tv_written hello");
        assert_eq!(code, TCL_OK);

        let calls = seen();
        assert_eq!(calls.len(), 1, "exactly one write trace call: {calls:?}");
        assert_eq!(calls[0].0, "tv_written");
        assert_eq!(calls[0].2, 7, "the clientData comes back unchanged");
        assert_ne!(
            calls[0].1 & TCL_TRACE_WRITES,
            0,
            "the trace is told this was a write"
        );

        // What the trace saw is what the variable holds.
        let get: GetVar2 = slot("tcl_GetVar2");
        let name = CString::new("tv_written").expect("name");
        let value = get(interp(), name.as_ptr(), ptr::null(), TCL_GLOBAL_ONLY);
        assert!(!value.is_null());
        assert_eq!(CStr::from_ptr(value).to_string_lossy(), "hello");

        untrace_scalar(
            "tv_written",
            TCL_GLOBAL_ONLY | TCL_TRACE_WRITES,
            recorder as *const (),
            7,
        );
    }
}

/// A write that does not change the value is still a write in Tcl, but the
/// projection this frontend uses can only see a change. The test states which
/// of the two this is, so the limit is measured rather than assumed.
#[test]
fn a_write_of_the_same_value_is_not_seen_as_a_write() {
    let _serial = serial();
    unsafe {
        let (code, _) = eval_ex(interp(), "set tv_same first");
        assert_eq!(code, TCL_OK);
        forget_what_was_seen();
        assert_eq!(
            trace_scalar(
                "tv_same",
                TCL_GLOBAL_ONLY | TCL_TRACE_WRITES,
                recorder as *const (),
                0
            ),
            TCL_OK
        );
        eval_ex(interp(), "set tv_same first");
        assert_eq!(
            seen().len(),
            0,
            "the value did not move, so this frontend has nothing to notice"
        );
        eval_ex(interp(), "set tv_same second");
        assert_eq!(seen().len(), 1, "a value that did move is noticed");
        untrace_scalar(
            "tv_same",
            TCL_GLOBAL_ONLY | TCL_TRACE_WRITES,
            recorder as *const (),
            0,
        );
    }
}

/// A read trace fires at the read, once per read — the property the blanking in
/// `runtime::TracedIn::blank_reads` exists to give, and the one a boundary-only
/// implementation would not have.
#[test]
fn a_read_trace_fires_once_for_every_read() {
    let _serial = serial();
    unsafe {
        eval_ex(interp(), "set tv_read seven");
        forget_what_was_seen();
        assert_eq!(
            trace_scalar(
                "tv_read",
                TCL_GLOBAL_ONLY | TCL_TRACE_READS,
                recorder as *const (),
                0
            ),
            TCL_OK
        );
        let (code, result) = eval_ex(interp(), "string cat $tv_read $tv_read $tv_read");
        assert_eq!((code, result.as_str()), (TCL_OK, "sevensevenseven"));
        assert_eq!(
            seen().len(),
            3,
            "three reads, three trace calls: {:?}",
            seen()
        );
        untrace_scalar(
            "tv_read",
            TCL_GLOBAL_ONLY | TCL_TRACE_READS,
            recorder as *const (),
            0,
        );
    }
}

/// `Tcl_UnsetVar2` fires the unset traces itself, which is what lets a widget
/// re-create the variable it was watching
/// (`tk9.0.4/generic/tkButton.c:1785-1789`).
#[test]
fn unsetting_a_variable_from_c_fires_its_unset_traces() {
    let _serial = serial();
    unsafe {
        eval_ex(interp(), "set tv_gone here");
        forget_what_was_seen();
        assert_eq!(
            trace_scalar(
                "tv_gone",
                TCL_GLOBAL_ONLY | TCL_TRACE_UNSETS,
                recorder as *const (),
                0
            ),
            TCL_OK
        );
        let f: UnsetVar2 = slot("tcl_UnsetVar2");
        let name = CString::new("tv_gone").expect("name");
        assert_eq!(
            f(interp(), name.as_ptr(), ptr::null(), TCL_GLOBAL_ONLY),
            TCL_OK
        );
        let calls = seen();
        assert_eq!(calls.len(), 1, "one unset trace call: {calls:?}");
        assert_ne!(calls[0].1 & TCL_TRACE_UNSETS, 0);

        // Unsetting one that is not set is an error, as it is in Tcl.
        assert_eq!(
            f(interp(), name.as_ptr(), ptr::null(), TCL_GLOBAL_ONLY),
            TCL_ERROR
        );
        untrace_scalar(
            "tv_gone",
            TCL_GLOBAL_ONLY | TCL_TRACE_UNSETS,
            recorder as *const (),
            0,
        );
    }
}

/// The refusal a trace procedure returns becomes the refusal of the access
/// (`generic/tclTrace.c:2663-2700`). `Tcl_ObjSetVar2` reports it by answering
/// NULL, which is what `tkButton.c:1257-1261` acts on.
#[test]
fn a_trace_that_refuses_a_write_makes_the_write_fail() {
    let _serial = serial();
    unsafe {
        let flags = TCL_GLOBAL_ONLY | TCL_TRACE_WRITES;
        assert_eq!(
            trace_scalar("tv_ro", flags, refuser as *const (), 0),
            TCL_OK
        );

        let set: ObjSetVar2 = slot("tcl_ObjSetVar2");
        let name = new_obj("tv_ro");
        // `TCL_LEAVE_ERR_MSG` — `generic/tcl.h:1015`.
        let answer = set(
            interp(),
            name,
            ptr::null_mut(),
            new_obj("x"),
            TCL_GLOBAL_ONLY | 0x200,
        );
        assert_eq!(answer, ptr::null_mut(), "the refusal is reported as NULL");
        assert_eq!(result_of(interp()), "this variable may not be written");

        untrace_scalar("tv_ro", flags, refuser as *const (), 0);
    }
}

/// A refusal from a write a *script* made has nowhere to go once the command
/// that made it is over, so it is dropped rather than turned into a failure of
/// something else. Stated as a test because it is the one place the projection
/// is weaker than Tcl, and a silent difference is the kind that rots.
#[test]
fn a_refusal_from_a_scripts_write_is_dropped_at_the_end_of_the_evaluation() {
    let _serial = serial();
    unsafe {
        let flags = TCL_GLOBAL_ONLY | TCL_TRACE_WRITES;
        assert_eq!(
            trace_scalar("tv_ro2", flags, refuser as *const (), 0),
            TCL_OK
        );
        let (code, result) = eval_ex(interp(), "set tv_ro2 x");
        assert_eq!(
            (code, result.as_str()),
            (TCL_OK, "x"),
            "the write stands; Tcl would have failed the `set`"
        );
        untrace_scalar("tv_ro2", flags, refuser as *const (), 0);
    }
}

/// A removed trace stops firing, and only the matching one is removed:
/// `Tcl_UntraceVar2` compares the procedure, the flags and the clientData
/// (`generic/tclTrace.c:2828-2831`).
#[test]
fn untracing_removes_one_trace_and_leaves_the_others() {
    let _serial = serial();
    unsafe {
        let flags = TCL_GLOBAL_ONLY | TCL_TRACE_WRITES;
        assert_eq!(
            trace_scalar("tv_two", flags, recorder as *const (), 1),
            TCL_OK
        );
        assert_eq!(
            trace_scalar("tv_two", flags, recorder as *const (), 2),
            TCL_OK
        );
        forget_what_was_seen();
        eval_ex(interp(), "set tv_two a");
        assert_eq!(seen().len(), 2, "both fire: {:?}", seen());

        untrace_scalar("tv_two", flags, recorder as *const (), 1);
        forget_what_was_seen();
        eval_ex(interp(), "set tv_two b");
        let calls = seen();
        assert_eq!(calls.len(), 1, "one left: {calls:?}");
        assert_eq!(calls[0].2, 2, "the one that was not removed");

        untrace_scalar("tv_two", flags, recorder as *const (), 2);
        forget_what_was_seen();
        eval_ex(interp(), "set tv_two c");
        assert_eq!(seen().len(), 0, "no trace left");
    }
}

/// The cursor walk `ButtonTextVarProc` depends on
/// (`tk9.0.4/generic/tkButton.c:1764-1783`): NULL gives the first trace with
/// this procedure, and handing an answer back gives the next.
#[test]
fn var_trace_info_walks_the_traces_of_one_variable() {
    let _serial = serial();
    unsafe {
        let flags = TCL_GLOBAL_ONLY | TCL_TRACE_WRITES;
        assert_eq!(
            trace_scalar("tv_walk", flags, recorder as *const (), 11),
            TCL_OK
        );
        assert_eq!(
            trace_scalar("tv_walk", flags, other_recorder as *const (), 22),
            TCL_OK
        );
        assert_eq!(
            trace_scalar("tv_walk", flags, recorder as *const (), 33),
            TCL_OK
        );

        let info: VarTraceInfo2 = slot("tcl_VarTraceInfo2");
        let name = CString::new("tv_walk").expect("name");
        let ask = |prev: usize| {
            info(
                interp(),
                name.as_ptr(),
                ptr::null(),
                flags,
                recorder as *mut c_void,
                prev as *mut c_void,
            ) as usize
        };
        // Newest first, which is the order Tcl's list is in
        // (`generic/tclTrace.c:3086-3092`), and only the traces whose procedure
        // matches.
        assert_eq!(ask(0), 33, "the first trace with this procedure");
        assert_eq!(ask(33), 11, "the next one, skipping the other procedure");
        assert_eq!(ask(11), 0, "and then there are none");
        // A variable with no trace at all answers NULL, which is how
        // `Tcl_LinkVar` decides a variable is not already linked
        // (`generic/tclLink.c:167-173`).
        let absent = CString::new("tv_never_traced").expect("name");
        assert_eq!(
            info(
                interp(),
                absent.as_ptr(),
                ptr::null(),
                flags,
                recorder as *mut c_void,
                ptr::null_mut()
            ),
            ptr::null_mut()
        );

        untrace_scalar("tv_walk", flags, recorder as *const (), 11);
        untrace_scalar("tv_walk", flags, other_recorder as *const (), 22);
        untrace_scalar("tv_walk", flags, recorder as *const (), 33);
    }
}

/// A trace that writes the variable it is tracing must not call itself. Tcl's
/// guard is `TclIsVarTraceActive` (`generic/tclTrace.c:2514-2517`).
#[test]
fn a_trace_that_writes_its_own_variable_does_not_recur() {
    static DEPTH: AtomicUsize = AtomicUsize::new(0);
    static MAX: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn writer(
        _client_data: *mut c_void,
        interp: *mut c_void,
        name1: *const c_char,
        _name2: *const c_char,
        _flags: c_int,
    ) -> *mut c_char {
        let depth = DEPTH.fetch_add(1, Ordering::SeqCst) + 1;
        MAX.fetch_max(depth, Ordering::SeqCst);
        let set: SetVar2Ex = slot("tcl_SetVar2Ex");
        let value = new_obj("rewritten by the trace");
        set(interp, name1, ptr::null(), value, TCL_GLOBAL_ONLY);
        DEPTH.fetch_sub(1, Ordering::SeqCst);
        ptr::null_mut()
    }

    let _serial = serial();
    unsafe {
        let flags = TCL_GLOBAL_ONLY | TCL_TRACE_WRITES;
        assert_eq!(
            trace_scalar("tv_loop", flags, writer as *const (), 0),
            TCL_OK
        );
        let (code, _) = eval_ex(interp(), "set tv_loop start");
        assert_eq!(code, TCL_OK);
        assert_eq!(
            MAX.load(Ordering::SeqCst),
            1,
            "the trace ran, and ran once, rather than recurring"
        );
        let get: GetVar2 = slot("tcl_GetVar2");
        let name = CString::new("tv_loop").expect("name");
        let value = get(interp(), name.as_ptr(), ptr::null(), TCL_GLOBAL_ONLY);
        assert_eq!(
            CStr::from_ptr(value).to_string_lossy(),
            "rewritten by the trace",
            "what the trace stored is what the variable holds"
        );
        untrace_scalar("tv_loop", flags, writer as *const (), 0);
    }
}

/// One trace watching all three operations sees all three, and is told which
/// one it is looking at each time. `Tcl_LinkVar` registers exactly this
/// combination (`generic/tclLink.c:201-203`), so nothing about linking works
/// without it.
#[test]
fn one_trace_can_watch_reads_writes_and_unsets_at_once() {
    let _serial = serial();
    unsafe {
        assert_eq!(
            trace_scalar("tv_all", WATCH_ALL, recorder as *const (), 0),
            TCL_OK
        );
        forget_what_was_seen();

        eval_ex(interp(), "set tv_all first");
        eval_ex(interp(), "string length $tv_all");
        let unset: UnsetVar2 = slot("tcl_UnsetVar2");
        let name = CString::new("tv_all").expect("name");
        assert_eq!(
            unset(interp(), name.as_ptr(), ptr::null(), TCL_GLOBAL_ONLY),
            TCL_OK
        );

        let kinds: Vec<c_int> = seen()
            .iter()
            .map(|(_, flags, _)| flags & (TCL_TRACE_READS | TCL_TRACE_WRITES | TCL_TRACE_UNSETS))
            .collect();
        assert_eq!(
            kinds,
            vec![TCL_TRACE_WRITES, TCL_TRACE_READS, TCL_TRACE_UNSETS],
            "in the order the script and the host performed them: {:?}",
            seen()
        );
        untrace_scalar("tv_all", WATCH_ALL, recorder as *const (), 0);
    }
}

// ── the object-valued slots ──────────────────────────────────────────────

/// The two halves of the bridge meet: a value Tk stores is a value the script
/// reads, and the other way round. Without this a `-textvariable` would be set
/// in one store and read from another.
#[test]
fn tk_and_a_script_see_one_set_of_variables() {
    let _serial = serial();
    unsafe {
        let set: ObjSetVar2 = slot("tcl_ObjSetVar2");
        let get: ObjGetVar2 = slot("tcl_ObjGetVar2");
        let name = new_obj("tv_shared");

        assert!(!set(
            interp(),
            name,
            ptr::null_mut(),
            new_obj("from C"),
            TCL_GLOBAL_ONLY
        )
        .is_null());
        let (code, result) = eval_ex(interp(), "set tv_shared");
        assert_eq!((code, result.as_str()), (TCL_OK, "from C"));

        let (code, _) = eval_ex(interp(), "set tv_shared from-the-script");
        assert_eq!(code, TCL_OK);
        let value = get(interp(), name, ptr::null_mut(), TCL_GLOBAL_ONLY);
        assert!(!value.is_null());
        assert_eq!(text_of(value), "from-the-script");

        // An unset variable is NULL, not an empty value — the distinction
        // `tkButton.c:1256` acts on when it decides whether to create the
        // variable itself.
        let missing = new_obj("tv_never_set_at_all");
        assert_eq!(
            get(interp(), missing, ptr::null_mut(), TCL_GLOBAL_ONLY),
            ptr::null_mut()
        );
    }
}

/// The setting slots answer with what the variable holds *after* its traces,
/// not with what they were handed. Tcl re-reads for the same reason, and a
/// widget that keeps the object it is given (`tkButton.c:1266-1267`) would
/// otherwise keep a value the interpreter no longer has.
#[test]
fn a_setting_slot_answers_with_what_the_trace_left() {
    unsafe extern "C" fn rewriter(
        _client_data: *mut c_void,
        interp: *mut c_void,
        name1: *const c_char,
        _name2: *const c_char,
        _flags: c_int,
    ) -> *mut c_char {
        let set: SetVar2Ex = slot("tcl_SetVar2Ex");
        set(
            interp,
            name1,
            ptr::null(),
            new_obj("what the trace chose"),
            TCL_GLOBAL_ONLY,
        );
        ptr::null_mut()
    }

    let _serial = serial();
    unsafe {
        let flags = TCL_GLOBAL_ONLY | TCL_TRACE_WRITES;
        assert_eq!(
            trace_scalar("tv_after", flags, rewriter as *const (), 0),
            TCL_OK
        );
        let set: ObjSetVar2 = slot("tcl_ObjSetVar2");
        let answer = set(
            interp(),
            new_obj("tv_after"),
            ptr::null_mut(),
            new_obj("what the caller asked for"),
            TCL_GLOBAL_ONLY,
        );
        assert!(!answer.is_null());
        assert_eq!(text_of(answer), "what the trace chose");
        untrace_scalar("tv_after", flags, rewriter as *const (), 0);
    }
}

// ── linked variables ─────────────────────────────────────────────────────

/// The C storage really is written. Nothing else here can show that: reading
/// the Tcl variable back would answer with the Tcl variable.
#[test]
fn a_script_writing_a_linked_int_changes_the_c_variable() {
    let _serial = serial();
    static CELL: AtomicI32 = AtomicI32::new(41);
    unsafe {
        let link: LinkVar = slot("tcl_LinkVar");
        let unlink: UnlinkVar = slot("tcl_UnlinkVar");
        let name = CString::new("lv_int").expect("name");
        // `TCL_LINK_INT` — `generic/tcl.h:1042`.
        assert_eq!(
            link(interp(), name.as_ptr(), CELL.as_ptr() as *mut c_void, 1),
            TCL_OK
        );
        // Linking sets the Tcl variable from the C storage first
        // (`generic/tclLink.c:189-195`).
        let (code, result) = eval_ex(interp(), "set lv_int");
        assert_eq!((code, result.as_str()), (TCL_OK, "41"));

        let (code, _) = eval_ex(interp(), "set lv_int 42");
        assert_eq!(code, TCL_OK);
        assert_eq!(
            CELL.load(Ordering::SeqCst),
            42,
            "the C variable followed the script"
        );

        // And the other way: the C moves, and a read picks it up because the
        // link's read trace fires at the read.
        CELL.store(99, Ordering::SeqCst);
        let (code, result) = eval_ex(interp(), "set lv_int");
        assert_eq!((code, result.as_str()), (TCL_OK, "99"));

        // Linking the same name twice is refused (`generic/tclLink.c:167-173`).
        assert_eq!(
            link(interp(), name.as_ptr(), CELL.as_ptr() as *mut c_void, 1),
            TCL_ERROR
        );
        assert_eq!(result_of(interp()), "variable 'lv_int' is already linked");

        unlink(interp(), name.as_ptr());
        // Unlinked, the C stops following.
        let (code, _) = eval_ex(interp(), "set lv_int 7");
        assert_eq!(code, TCL_OK);
        assert_eq!(CELL.load(Ordering::SeqCst), 99, "no longer linked");
    }
}

/// A value the C type cannot hold is refused with the C's own message, and the
/// variable is put back (`generic/tclLink.c:900-906`).
#[test]
fn a_linked_int_refuses_a_value_that_is_not_an_integer() {
    let _serial = serial();
    static CELL: AtomicI32 = AtomicI32::new(5);
    unsafe {
        let link: LinkVar = slot("tcl_LinkVar");
        let unlink: UnlinkVar = slot("tcl_UnlinkVar");
        let name = CString::new("lv_bad").expect("name");
        assert_eq!(
            link(interp(), name.as_ptr(), CELL.as_ptr() as *mut c_void, 1),
            TCL_OK
        );
        eval_ex(interp(), "set lv_bad wobble");
        assert_eq!(
            CELL.load(Ordering::SeqCst),
            5,
            "the C variable kept its value"
        );
        let (code, result) = eval_ex(interp(), "set lv_bad");
        assert_eq!(
            (code, result.as_str()),
            (TCL_OK, "5"),
            "and the Tcl variable was put back to it"
        );
        unlink(interp(), name.as_ptr());
    }
}

/// `Tcl_UpdateLinkedVar` is what C calls after changing the storage itself
/// (`generic/tclLink.c:439-463`), and it must fire the variable's *other*
/// traces while suppressing the link's own.
#[test]
fn update_linked_var_pushes_the_c_value_and_fires_other_traces() {
    let _serial = serial();
    static CELL: AtomicI32 = AtomicI32::new(1);
    unsafe {
        let link: LinkVar = slot("tcl_LinkVar");
        let unlink: UnlinkVar = slot("tcl_UnlinkVar");
        let update: UpdateLinkedVar = slot("tcl_UpdateLinkedVar");
        let name = CString::new("lv_push").expect("name");
        assert_eq!(
            link(interp(), name.as_ptr(), CELL.as_ptr() as *mut c_void, 1),
            TCL_OK
        );
        let flags = TCL_GLOBAL_ONLY | TCL_TRACE_WRITES;
        assert_eq!(
            trace_scalar("lv_push", flags, recorder as *const (), 0),
            TCL_OK
        );
        forget_what_was_seen();

        CELL.store(1234, Ordering::SeqCst);
        update(interp(), name.as_ptr());

        let get: GetVar2 = slot("tcl_GetVar2");
        let value = get(interp(), name.as_ptr(), ptr::null(), TCL_GLOBAL_ONLY);
        assert_eq!(CStr::from_ptr(value).to_string_lossy(), "1234");
        assert_eq!(
            seen().len(),
            1,
            "the watcher was told, exactly once: {:?}",
            seen()
        );

        untrace_scalar("lv_push", flags, recorder as *const (), 0);
        unlink(interp(), name.as_ptr());
    }
}

/// `TCL_LINK_READ_ONLY` (`generic/tcl.h:1058`): a script's write is undone and
/// answered with `linked variable is read-only` (`generic/tclLink.c:807-811`).
#[test]
fn a_read_only_link_puts_its_value_back() {
    let _serial = serial();
    static CELL: AtomicI32 = AtomicI32::new(64);
    unsafe {
        let link: LinkVar = slot("tcl_LinkVar");
        let unlink: UnlinkVar = slot("tcl_UnlinkVar");
        let name = CString::new("lv_ro").expect("name");
        // `TCL_LINK_INT | TCL_LINK_READ_ONLY`.
        assert_eq!(
            link(
                interp(),
                name.as_ptr(),
                CELL.as_ptr() as *mut c_void,
                1 | 0x80
            ),
            TCL_OK
        );
        eval_ex(interp(), "set lv_ro 3");
        assert_eq!(CELL.load(Ordering::SeqCst), 64, "the C is untouched");
        let (code, result) = eval_ex(interp(), "set lv_ro");
        assert_eq!(
            (code, result.as_str()),
            (TCL_OK, "64"),
            "and the Tcl variable was put back"
        );
        unlink(interp(), name.as_ptr());
    }
}

/// A boolean link is the one Tk uses most — `tk_strictMotif` and
/// `::tk::AlwaysShowSelection` are both `TCL_LINK_BOOLEAN`
/// (`tk9.0.4/generic/tkWindow.c:900-910`) — and it accepts every spelling of a
/// Tcl boolean.
#[test]
fn a_boolean_link_takes_every_spelling_tcl_accepts() {
    let _serial = serial();
    static CELL: AtomicI32 = AtomicI32::new(0);
    unsafe {
        let link: LinkVar = slot("tcl_LinkVar");
        let unlink: UnlinkVar = slot("tcl_UnlinkVar");
        let name = CString::new("lv_bool").expect("name");
        // `TCL_LINK_BOOLEAN` — `generic/tcl.h:1044`.
        assert_eq!(
            link(interp(), name.as_ptr(), CELL.as_ptr() as *mut c_void, 3),
            TCL_OK
        );
        for (script, want) in [
            ("set lv_bool yes", 1),
            ("set lv_bool 0", 0),
            ("set lv_bool true", 1),
            ("set lv_bool off", 0),
            ("set lv_bool 42", 1),
        ] {
            let (code, _) = eval_ex(interp(), script);
            assert_eq!(code, TCL_OK, "{script}");
            assert_eq!(CELL.load(Ordering::SeqCst), want, "{script}");
        }
        // The variable keeps the text the script wrote until the C value moves
        // away from it: the read trace rewrites only when `lastValue` and the C
        // storage disagree (`generic/tclLink.c:749-753,791-794`). After
        // `set lv_bool 42` both are 1, so `42` is still what is read.
        let (code, result) = eval_ex(interp(), "set lv_bool");
        assert_eq!((code, result.as_str()), (TCL_OK, "42"));
        // Move the C storage, and the next read picks it up — as the boolean
        // `0`/`1` the C holds, not as anything the script wrote.
        CELL.store(0, Ordering::SeqCst);
        let (code, result) = eval_ex(interp(), "set lv_bool");
        assert_eq!((code, result.as_str()), (TCL_OK, "0"));
        unlink(interp(), name.as_ptr());
    }
}

/// A type this host does not link is refused rather than guessed at.
/// `TCL_LINK_CHARS` and `TCL_LINK_BINARY` both need the buffer length that only
/// `Tcl_LinkArray` sets, and there is no `Tcl_LinkArray` here.
#[test]
fn an_unhosted_link_type_is_refused() {
    let _serial = serial();
    static CELL: AtomicI32 = AtomicI32::new(0);
    unsafe {
        let link: LinkVar = slot("tcl_LinkVar");
        let name = CString::new("lv_chars").expect("name");
        // `TCL_LINK_CHARS` — `generic/tcl.h:1056`.
        assert_eq!(
            link(interp(), name.as_ptr(), CELL.as_ptr() as *mut c_void, 15),
            TCL_ERROR
        );
        assert_eq!(result_of(interp()), "bad linked variable type 15");
    }
}
