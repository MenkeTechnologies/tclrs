//! Variable traces and linked variables: `Tcl_TraceVar2`, `Tcl_UntraceVar2`,
//! `Tcl_VarTraceInfo2`, `Tcl_LinkVar`, `Tcl_UnlinkVar`, `Tcl_UpdateLinkedVar`,
//! and the object-valued variable slots they are built on.
//!
//! Ported from `generic/tclTrace.c` and `generic/tclLink.c`.
//!
//! # Why this is the module Tk cannot be hosted without
//!
//! Every widget option that names a variable is a variable trace. `-textvariable`
//! on a label, `-variable` on a checkbutton, `-listvariable` on a listbox,
//! `-variable` on a scale — each one is a `Tcl_TraceVar2` with
//! `TCL_TRACE_WRITES|TCL_TRACE_UNSETS` and a C procedure that recomputes the
//! widget (`tk9.0.4/generic/tkButton.c:1322-1330`,
//! `tk9.0.4/generic/tkEntry.c:1437`, `tk9.0.4/generic/tkListbox.c:1677`,
//! `tk9.0.4/generic/tkScale.c:731`). Without the trace firing, setting the
//! variable does nothing at all and the widget keeps its old text forever.
//!
//! `Tcl_LinkVar` is the other half: five variables are linked to C storage while
//! Tk starts (`tk9.0.4/generic/tkWindow.c:900,907`,
//! `tk9.0.4/macosx/tkMacOSXDraw.c:89,99,103`,
//! `tk9.0.4/macosx/tkMacOSXFont.c:1538`), and a script that writes
//! `tk_strictMotif` is expected to change the C `int` behind it.
//!
//! # The three names that are not slots
//!
//! `Tcl_TraceVar`, `Tcl_UntraceVar` and `Tcl_VarTraceInfo` have no stub slots in
//! Tcl 9: they are macros over the two-part forms with a NULL second part
//! (`generic/tclDecls.h:3954-3959`). Implementing the `2` forms serves both
//! spellings, and `install(t, "tcl_TraceVar", …)` would panic — the table has
//! `reserved247` where 8.6 had that name.
//!
//! # Where a trace fires
//!
//! Tcl fires a trace inside the access itself: `TclCallVarTraces`
//! (`generic/tclTrace.c:2481`) runs from `TclPtrSetVar` and its neighbours. A
//! chunk here reaches its globals through fusevm's native `GetVar` and `SetVar`
//! ops, which have no host hook, so the two halves are answered differently:
//!
//! * **reads** fire at the read. [`crate::runtime`] empties the slot of a
//!   read-traced global, so every read of one reaches fusevm's undef hook, and
//!   the hook runs the traces and answers with what they left.
//! * **writes and unsets** fire when the chunk next hands control to something
//!   that could observe the variable — a call to a registered Tk command, a
//!   nested `eval`, or the end of the evaluation. No C code runs between two ops
//!   of a chunk otherwise, so nothing that a trace could talk to can tell the
//!   difference; a second script watching from another thread could.
//! * a write made **by Tk**, through the slots in this file, fires at the call.
//!
//! `crate::runtime::sync_out` and `crate::runtime::sync_in` are that boundary,
//! and the documentation there states the one case the projection loses.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::{Arc, Mutex};

use fusevm::Value;

use super::abi::{RawStub, TclObj, TclStubs, TCL_ERROR, TCL_OK};
use super::generated::TCL_NAMES;
use super::host::{self, HostInterp};
use super::interp;
use super::obj;
use super::trace::{note, record, Table};
use crate::runtime::{
    self, arm_var_traces, set_var_trace_sink, to_tcl_string, TraceOp, Traced, VarTraceSink,
};

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

// ── the flag bits, from `generic/tcl.h:1006-1058` ────────────────────────────

pub const TCL_GLOBAL_ONLY: c_int = 1;
/// `TCL_LEAVE_ERR_MSG` (`generic/tcl.h:1015`): the caller wants the reason for
/// a refusal left in the interpreter result, not just the refusal.
pub const TCL_LEAVE_ERR_MSG: c_int = 0x200;
pub const TCL_TRACE_READS: c_int = 0x10;
pub const TCL_TRACE_WRITES: c_int = 0x20;
pub const TCL_TRACE_UNSETS: c_int = 0x40;
pub const TCL_TRACE_DESTROYED: c_int = 0x80;
pub const TCL_TRACE_ARRAY: c_int = 0x800;
pub const TCL_TRACE_RESULT_DYNAMIC: c_int = 0x8000;
pub const TCL_TRACE_RESULT_OBJECT: c_int = 0x10000;

/// The bits `TraceVarEx` keeps and `Tcl_UntraceVar2` compares on
/// (`generic/tclTrace.c:3082-3084`, `generic/tclTrace.c:2818-2820`). A trace
/// registered with `TCL_GLOBAL_ONLY` set is *stored* without it, so an untrace
/// that passes the same flags matches.
const TRACE_FLAG_MASK: c_int = TCL_TRACE_READS
    | TCL_TRACE_WRITES
    | TCL_TRACE_UNSETS
    | TCL_TRACE_ARRAY
    | TCL_TRACE_RESULT_DYNAMIC
    | TCL_TRACE_RESULT_OBJECT;

const TCL_LINK_INT: c_int = 1;
const TCL_LINK_DOUBLE: c_int = 2;
const TCL_LINK_BOOLEAN: c_int = 3;
const TCL_LINK_STRING: c_int = 4;
const TCL_LINK_WIDE_INT: c_int = 5;
const TCL_LINK_CHAR: c_int = 6;
const TCL_LINK_UCHAR: c_int = 7;
const TCL_LINK_SHORT: c_int = 8;
const TCL_LINK_USHORT: c_int = 9;
const TCL_LINK_UINT: c_int = 10;
const TCL_LINK_FLOAT: c_int = 13;
const TCL_LINK_WIDE_UINT: c_int = 14;
const TCL_LINK_READ_ONLY: c_int = 0x80;

/// `LINK_READ_ONLY` and `LINK_BEING_UPDATED` (`generic/tclLink.c:89-90`).
const LINK_READ_ONLY: c_int = 1;
const LINK_BEING_UPDATED: c_int = 2;

// ── the registry ─────────────────────────────────────────────────────────────

/// `char *(*Tcl_VarTraceProc)(void *clientData, Tcl_Interp *interp,
/// const char *name1, const char *name2, int flags)` — `generic/tcl.h:706-708`.
type VarTraceProc = unsafe extern "C" fn(
    *mut c_void,
    *mut c_void,
    *const c_char,
    *const c_char,
    c_int,
) -> *mut c_char;

/// `VarTrace` (`generic/tclInt.h`), flattened.
///
/// Tcl hangs the list off the variable; here it is one list keyed by the
/// variable's two-part name, because this host has no `Var` structure to hang
/// anything off. The two addresses are `usize` rather than pointers so the
/// registry is `Send`: nothing dereferences them except [`Registry::fire`],
/// which is only ever reached from the thread the interpreter runs on.
#[derive(Clone)]
struct VarTrace {
    part1: String,
    part2: Option<String>,
    /// Already masked by [`TRACE_FLAG_MASK`], as `TraceVarEx` masks
    /// (`generic/tclTrace.c:3084`).
    flags: c_int,
    proc_: usize,
    client_data: usize,
}

/// `Link` (`generic/tclLink.c:28-74`), without the array fields: `Tcl_LinkArray`
/// is a different slot and is not hosted, so `bytes` and `numElems` would be
/// zero everywhere.
struct Link {
    var_name: String,
    addr: usize,
    ty: c_int,
    flags: c_int,
    /// `lastValue`, as the widest thing the linked types need. A read trace
    /// only rewrites the Tcl variable when the C value has moved
    /// (`generic/tclLink.c:739-794`), and this is what "moved" is measured
    /// against.
    last: LastValue,
}

#[derive(Clone, Copy, PartialEq)]
enum LastValue {
    Int(i64),
    Float(f64),
    /// The last string handed to a `TCL_LINK_STRING`, whose `Tcl_Alloc`ed
    /// buffer this side owns.
    Text(usize),
    None,
}

#[derive(Default)]
struct Registry {
    traces: Vec<VarTrace>,
    links: Vec<Link>,
    /// Variables whose traces are running, so a trace that writes the variable
    /// it is tracing does not call itself. Tcl's guard is the `VAR_TRACE_ACTIVE`
    /// bit and the early return at `generic/tclTrace.c:2514-2517`.
    active: Vec<String>,
}

static REGISTRY: Mutex<Option<Registry>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Registry) -> R) -> R {
    let mut guard = REGISTRY.lock().expect("variable trace registry");
    let registry = guard.get_or_insert_with(Registry::default);
    let out = f(registry);
    arm_var_traces(!registry.traces.is_empty());
    out
}

/// How many traces and links are live. The measurement the tests and the
/// `tk-host` binary report, so "the bridge did something" is a number rather
/// than a claim.
pub fn counts() -> (usize, usize) {
    with(|r| (r.traces.len(), r.links.len()))
}

/// Install the sink [`crate::runtime`] answers variable traces through. Called
/// once, when the hosting table is built.
pub fn install_sink() {
    set_var_trace_sink(Arc::new(Sink));
}

struct Sink;

impl VarTraceSink for Sink {
    fn traced(&self, name: &str) -> Traced {
        with(|r| {
            let mut watched = Traced::default();
            for t in r.traces.iter().filter(|t| t.part1 == name) {
                watched.reads |= t.flags & TCL_TRACE_READS != 0;
                watched.writes |= t.flags & TCL_TRACE_WRITES != 0;
                watched.unsets |= t.flags & TCL_TRACE_UNSETS != 0;
            }
            watched
        })
    }

    fn fire(&self, name: &str, op: TraceOp) -> Result<(), String> {
        fire_traces(name, None, op)
    }
}

/// Run every trace on `name` that watches `op`.
///
/// The order is Tcl's: `TraceVarEx` prepends (`generic/tclTrace.c:3086-3092`),
/// so the most recently added trace runs first. The list is copied out before
/// any of it runs, because a trace procedure may add or remove traces —
/// `ButtonTextVarProc` re-establishes its own on an unset
/// (`tk9.0.4/generic/tkButton.c:1787-1789`) — and Tcl's own walk is likewise
/// insulated from that by `ActiveVarTrace`.
fn fire_traces(part1: &str, part2: Option<&str>, op: TraceOp) -> Result<(), String> {
    let bit = match op {
        TraceOp::Read => TCL_TRACE_READS,
        TraceOp::Write => TCL_TRACE_WRITES,
        TraceOp::Unset => TCL_TRACE_UNSETS,
    };
    let due: Vec<VarTrace> = with(|r| {
        if r.active.iter().any(|n| n == part1) {
            return Vec::new();
        }
        let due: Vec<VarTrace> = r
            .traces
            .iter()
            .rev()
            .filter(|t| t.part1 == part1 && t.part2.as_deref() == part2 && t.flags & bit != 0)
            .cloned()
            .collect();
        if !due.is_empty() {
            r.active.push(part1.to_string());
        }
        due
    });
    if due.is_empty() {
        return Ok(());
    }

    // An unset trace is told the variable is going away for good, which is what
    // makes `ButtonTextVarProc` re-create it and re-trace it rather than only
    // note it (`tk9.0.4/generic/tkButton.c:1755-1790`, reached because
    // `TclCallVarTraces` adds the bit at `generic/tclTrace.c:2620-2622`).
    let flags = bit
        | TCL_GLOBAL_ONLY
        | if matches!(op, TraceOp::Unset) {
            TCL_TRACE_DESTROYED
        } else {
            0
        };
    let outcome = run_procs(&due, part1, part2, flags);
    with(|r| r.active.retain(|n| n != part1));
    outcome
}

fn run_procs(
    due: &[VarTrace],
    part1: &str,
    part2: Option<&str>,
    flags: c_int,
) -> Result<(), String> {
    let interp = interp::current() as *mut c_void;
    if interp.is_null() {
        return Ok(());
    }
    let name1 = CString::new(part1).map_err(|_| "a variable name with a NUL in it".to_string())?;
    let name2 = part2.and_then(|p| CString::new(p).ok());
    let name2_ptr = name2
        .as_ref()
        .map_or(std::ptr::null(), |c| c.as_ptr() as *const c_char);

    for t in due {
        let message = unsafe {
            let f: VarTraceProc = std::mem::transmute(t.proc_);
            f(
                t.client_data as *mut c_void,
                interp,
                name1.as_ptr() as *const c_char,
                name2_ptr,
                flags,
            )
        };
        if message.is_null() {
            continue;
        }
        // Errors in unset traces are ignored (`generic/tclTrace.c:2598-2603`).
        if flags & TCL_TRACE_UNSETS != 0 {
            continue;
        }
        return Err(trace_result(t.flags, message));
    }
    Ok(())
}

/// What a trace procedure's non-NULL answer says.
///
/// `TCL_TRACE_RESULT_OBJECT` means the `char *` is really a `Tcl_Obj *`
/// (`generic/tclTrace.c:2683-2688`); `TCL_TRACE_RESULT_DYNAMIC` means the string
/// was allocated and Tcl frees it. Neither is used by Tk, and both are read
/// here rather than assumed away.
fn trace_result(flags: c_int, message: *mut c_char) -> String {
    unsafe {
        if flags & TCL_TRACE_RESULT_OBJECT != 0 {
            return obj::text_of(message as *mut TclObj);
        }
        let text = String::from_utf8_lossy(CStr::from_ptr(message).to_bytes()).into_owned();
        if flags & TCL_TRACE_RESULT_DYNAMIC != 0 {
            libc::free(message as *mut c_void);
        }
        text
    }
}

// ── the trace slots ──────────────────────────────────────────────────────────

/// Slot 248. `Tcl_TraceVar2` (`generic/tclTrace.c:2981-3010`), which allocates a
/// `VarTrace` and hands it to `TraceVarEx`.
///
/// The flags are masked exactly as `TraceVarEx` masks them
/// (`generic/tclTrace.c:3082-3084`) so that `Tcl_UntraceVar2` — which compares
/// the stored flags for equality against its own masked argument — matches a
/// trace registered with `TCL_GLOBAL_ONLY` in the flags, which is how every one
/// of Tk's is registered.
unsafe extern "C" fn trace_var2(
    _interp: *mut c_void,
    part1: *const c_char,
    part2: *const c_char,
    flags: c_int,
    proc_: *mut c_void,
    client_data: *mut c_void,
) -> c_int {
    entered!("tcl_TraceVar2");
    let (name, index) = name_parts(part1, part2);
    note("TraceVar2", &name);
    if proc_.is_null() {
        return TCL_ERROR;
    }
    with(|r| {
        r.traces.push(VarTrace {
            part1: name,
            part2: index,
            flags: flags & TRACE_FLAG_MASK,
            proc_: proc_ as usize,
            client_data: client_data as usize,
        });
    });
    TCL_OK
}

/// Slot 256. `Tcl_UntraceVar2` (`generic/tclTrace.c:2780-2887`).
///
/// One trace is removed, not all matching ones: the C breaks out of its walk at
/// the first `(traceProc, flags, clientData)` triple that matches
/// (`generic/tclTrace.c:2823-2833`). Removing the most recently added match
/// keeps the pairing with [`fire_traces`], which runs them newest first.
unsafe extern "C" fn untrace_var2(
    _interp: *mut c_void,
    part1: *const c_char,
    part2: *const c_char,
    flags: c_int,
    proc_: *mut c_void,
    client_data: *mut c_void,
) {
    entered!("tcl_UntraceVar2");
    let (name, index) = name_parts(part1, part2);
    let flags = flags & TRACE_FLAG_MASK;
    with(|r| {
        if let Some(at) = r.traces.iter().rposition(|t| {
            t.part1 == name
                && t.part2 == index
                && t.flags == flags
                && t.proc_ == proc_ as usize
                && t.client_data == client_data as usize
        }) {
            r.traces.remove(at);
        }
    });
}

/// Slot 262. `Tcl_VarTraceInfo2` (`generic/tclTrace.c:2907-2957`).
///
/// The walk-with-a-cursor contract is the whole point of the slot and is what
/// `ButtonTextVarProc` uses to tell "my variable was unset" from "some older
/// variable I no longer watch was" (`tk9.0.4/generic/tkButton.c:1764-1783`):
/// NULL `prevClientData` gives the first trace with this procedure, and passing
/// a previous answer back gives the one after it.
unsafe extern "C" fn var_trace_info2(
    _interp: *mut c_void,
    part1: *const c_char,
    part2: *const c_char,
    _flags: c_int,
    proc_: *mut c_void,
    prev_client_data: *mut c_void,
) -> *mut c_void {
    entered!("tcl_VarTraceInfo2");
    let (name, index) = name_parts(part1, part2);
    with(|r| {
        // Newest first, the order the C's singly-linked list is in.
        let mut it = r
            .traces
            .iter()
            .rev()
            .filter(|t| t.part1 == name && t.part2 == index)
            .peekable();
        if !prev_client_data.is_null() {
            // Skip past the trace the caller last saw, and only past it: the C
            // advances one step from the match and then resumes the search
            // (`generic/tclTrace.c:2941-2949`).
            let mut seen = false;
            for t in it.by_ref() {
                if t.client_data == prev_client_data as usize && t.proc_ == proc_ as usize {
                    seen = true;
                    break;
                }
            }
            if !seen {
                return std::ptr::null_mut();
            }
        }
        for t in it {
            if t.proc_ == proc_ as usize {
                return t.client_data as *mut c_void;
            }
        }
        std::ptr::null_mut()
    })
}

// ── linked variables ─────────────────────────────────────────────────────────

/// Slot 187. `Tcl_LinkVar` (`generic/tclLink.c:152-209`).
///
/// The order is the C's: refuse a variable that is already linked, set the Tcl
/// variable from the C storage, then trace it for reads, writes and unsets.
///
/// # Safety
/// `interp` is a `Tcl_Interp *` this crate handed to Tk, `name` is a NUL-
/// terminated string, and `addr` is C storage of the type `ty` names that stays
/// valid until `Tcl_UnlinkVar`.
pub unsafe fn link_var(
    interp: *mut c_void,
    name: *const c_char,
    addr: *mut c_void,
    ty: c_int,
) -> c_int {
    let var_name = c_string(name);
    note("LinkVar", &var_name);
    if with(|r| r.links.iter().any(|l| l.var_name == var_name)) {
        host::set_result_bytes(
            interp,
            format!("variable '{var_name}' is already linked").as_bytes(),
        );
        return TCL_ERROR;
    }
    let mut link = Link {
        var_name: var_name.clone(),
        addr: addr as usize,
        ty: ty & !TCL_LINK_READ_ONLY,
        flags: if ty & TCL_LINK_READ_ONLY != 0 {
            LINK_READ_ONLY
        } else {
            0
        },
        last: LastValue::None,
    };
    let Some(value) = obj_value(&mut link) else {
        host::set_result_bytes(
            interp,
            format!("bad linked variable type {}", link.ty).as_bytes(),
        );
        return TCL_ERROR;
    };
    let Some(shared) = shared_of(interp) else {
        // No interpreter yet — nothing can read the variable, so the link is
        // recorded and the write it would have made is left to the read trace.
        with(|r| r.links.push(link));
        return TCL_OK;
    };
    if runtime::set_global_of(&shared, &var_name, value).is_err() {
        return TCL_ERROR;
    }
    with(|r| {
        r.links.push(link);
        r.traces.push(VarTrace {
            part1: var_name,
            part2: None,
            flags: TCL_TRACE_READS | TCL_TRACE_WRITES | TCL_TRACE_UNSETS,
            proc_: link_trace_proc as *const () as usize,
            client_data: 0,
        });
    });
    TCL_OK
}

/// Slot 251. `Tcl_UnlinkVar` (`generic/tclLink.c:402-418`): the trace goes and
/// the record goes; the Tcl variable keeps whatever value it had.
unsafe extern "C" fn unlink_var(_interp: *mut c_void, name: *const c_char) {
    entered!("tcl_UnlinkVar");
    let var_name = c_string(name);
    with(|r| {
        r.links.retain(|l| l.var_name != var_name);
        r.traces
            .retain(|t| !(t.part1 == var_name && t.proc_ == link_trace_proc as *const () as usize));
    });
}

/// Slot 257. `Tcl_UpdateLinkedVar` (`generic/tclLink.c:439-463`).
///
/// `LINK_BEING_UPDATED` is set across the write so that the link's own write
/// trace ignores it — the C variable is the source here, and letting the trace
/// convert the value back would be a round trip that can only lose.
unsafe extern "C" fn update_linked_var(interp: *mut c_void, name: *const c_char) {
    entered!("tcl_UpdateLinkedVar");
    let var_name = c_string(name);
    let Some(shared) = shared_of(interp) else {
        return;
    };
    let value = with(|r| {
        let link = r.links.iter_mut().find(|l| l.var_name == var_name)?;
        link.flags |= LINK_BEING_UPDATED;
        obj_value(link)
    });
    if let Some(value) = value {
        let _ = runtime::set_global_of(&shared, &var_name, value);
    }
    // "Callback may have unlinked the variable. [Bug 1740631]" —
    // `generic/tclLink.c:455-462`.
    with(|r| {
        if let Some(link) = r.links.iter_mut().find(|l| l.var_name == var_name) {
            link.flags &= !LINK_BEING_UPDATED;
        }
    });
}

/// `LinkTraceProc` (`generic/tclLink.c:681-1163`), as the registry's own trace
/// procedure rather than a C one: the link records live on this side, so there
/// is nothing to pass as `clientData` and the variable name is the key.
unsafe extern "C" fn link_trace_proc(
    _client_data: *mut c_void,
    interp: *mut c_void,
    name1: *const c_char,
    _name2: *const c_char,
    flags: c_int,
) -> *mut c_char {
    let var_name = c_string(name1);
    let Some(shared) = shared_of(interp) else {
        return std::ptr::null_mut();
    };

    // "If the variable is being unset, then just re-create it (with a trace)"
    // (`generic/tclLink.c:705-722`).
    if flags & TCL_TRACE_UNSETS != 0 {
        let value = with(|r| {
            let link = r.links.iter_mut().find(|l| l.var_name == var_name)?;
            obj_value(link)
        });
        if let Some(value) = value {
            let _ = runtime::set_global_of(&shared, &var_name, value);
        }
        return std::ptr::null_mut();
    }

    if with(|r| {
        r.links
            .iter()
            .any(|l| l.var_name == var_name && l.flags & LINK_BEING_UPDATED != 0)
    }) {
        return std::ptr::null_mut();
    }

    if flags & TCL_TRACE_READS != 0 {
        // "update the Tcl variable if the C variable has changed since the last
        // time we updated the Tcl variable" (`generic/tclLink.c:734-794`).
        let value = with(|r| {
            let link = r.links.iter_mut().find(|l| l.var_name == var_name)?;
            let before = link.last;
            let value = obj_value(link)?;
            (before != link.last || matches!(link.ty, TCL_LINK_STRING)).then_some(value)
        });
        if let Some(value) = value {
            let _ = runtime::set_global_of(&shared, &var_name, value);
        }
        return std::ptr::null_mut();
    }

    let read_only = with(|r| {
        r.links
            .iter()
            .any(|l| l.var_name == var_name && l.flags & LINK_READ_ONLY != 0)
    });
    if read_only {
        let value = with(|r| {
            let link = r.links.iter_mut().find(|l| l.var_name == var_name)?;
            obj_value(link)
        });
        if let Some(value) = value {
            let _ = runtime::set_global_of(&shared, &var_name, value);
        }
        return c"linked variable is read-only".as_ptr() as *mut c_char;
    }

    let Some(written) = runtime::global_of(&shared, &var_name) else {
        return c"internal error: linked variable couldn't be read".as_ptr() as *mut c_char;
    };
    let restore = with(|r| {
        let link = r.links.iter_mut().find(|l| l.var_name == var_name)?;
        match store_value(link, &written) {
            Ok(()) => None,
            // On a refusal the C puts the old value back before answering, so
            // the variable never keeps something the C storage disagrees with
            // (`generic/tclLink.c:900-906`).
            Err(message) => Some((obj_value(link), message)),
        }
    });
    match restore {
        None => std::ptr::null_mut(),
        Some((value, message)) => {
            if let Some(value) = value {
                let _ = runtime::set_global_of(&shared, &var_name, value);
            }
            message.as_ptr() as *mut c_char
        }
    }
}

/// `ObjValue` (`generic/tclLink.c:1165-1373`): the Tcl value of the C storage,
/// recording it as `lastValue` on the way past.
///
/// `None` for a type this host does not link. `TCL_LINK_CHARS` and
/// `TCL_LINK_BINARY` are among those: both need the buffer length that only
/// `Tcl_LinkArray` sets (`generic/tclLink.c:836-838,852-854`), and this host has
/// no `Tcl_LinkArray`, so guessing one would be inventing a length.
fn obj_value(link: &mut Link) -> Option<Value> {
    let addr = link.addr as *const c_void;
    if addr.is_null() {
        return None;
    }
    unsafe {
        let value = match link.ty {
            TCL_LINK_INT => integral(link, *(addr as *const c_int) as i64),
            TCL_LINK_BOOLEAN => {
                let raw = *(addr as *const c_int);
                link.last = LastValue::Int(raw as i64);
                Value::Bool(raw != 0)
            }
            TCL_LINK_WIDE_INT => integral(link, *(addr as *const i64)),
            TCL_LINK_CHAR => integral(link, *(addr as *const i8) as i64),
            TCL_LINK_UCHAR => integral(link, *(addr as *const u8) as i64),
            TCL_LINK_SHORT => integral(link, *(addr as *const i16) as i64),
            TCL_LINK_USHORT => integral(link, *(addr as *const u16) as i64),
            TCL_LINK_UINT => integral(link, *(addr as *const u32) as i64),
            TCL_LINK_WIDE_UINT => {
                let raw = *(addr as *const u64);
                link.last = LastValue::Int(raw as i64);
                // Tcl answers an unsigned wide as an unsigned decimal, which
                // above `i64::MAX` is not an `i64` at all
                // (`generic/tclLink.c:1214-1224`).
                Value::Str(Arc::new(raw.to_string()))
            }
            TCL_LINK_DOUBLE => real(link, *(addr as *const f64)),
            TCL_LINK_FLOAT => real(link, *(addr as *const f32) as f64),
            TCL_LINK_STRING => {
                let p = *(addr as *const *const c_char);
                link.last = LastValue::Text(p as usize);
                if p.is_null() {
                    // "NULL" is the string a NULL char* links as
                    // (`generic/tclLink.c:1338-1341`).
                    Value::Str(Arc::new("NULL".to_string()))
                } else {
                    Value::Str(Arc::new(
                        String::from_utf8_lossy(CStr::from_ptr(p).to_bytes()).into_owned(),
                    ))
                }
            }
            _ => return None,
        };
        Some(value)
    }
}

fn integral(link: &mut Link, raw: i64) -> Value {
    link.last = LastValue::Int(raw);
    Value::Int(raw)
}

fn real(link: &mut Link, raw: f64) -> Value {
    link.last = LastValue::Float(raw);
    Value::Float(raw)
}

/// The write half of `LinkTraceProc` (`generic/tclLink.c:885-1137`): convert a
/// Tcl value to the C type and store it, or answer with the C's own refusal.
fn store_value(link: &mut Link, value: &Value) -> Result<(), &'static str> {
    let addr = link.addr as *mut c_void;
    if addr.is_null() {
        return Err("internal error: bad linked variable type");
    }
    let text = to_tcl_string(value);
    unsafe {
        match link.ty {
            TCL_LINK_INT => {
                let n = wide(&text).ok_or("variable must have integer value")?;
                let n: i32 = n
                    .try_into()
                    .map_err(|_| "variable must have integer value")?;
                *(addr as *mut c_int) = n;
                link.last = LastValue::Int(n as i64);
            }
            TCL_LINK_BOOLEAN => {
                let b = crate::runtime::tcl_bool(value)
                    .map_err(|_| "variable must have boolean value")?;
                *(addr as *mut c_int) = c_int::from(b);
                link.last = LastValue::Int(i64::from(b));
            }
            TCL_LINK_WIDE_INT => {
                let n = wide(&text).ok_or("variable must have wide integer value")?;
                *(addr as *mut i64) = n;
                link.last = LastValue::Int(n);
            }
            TCL_LINK_CHAR => {
                let n = narrow::<i8>(&text, "variable must have char value")?;
                *(addr as *mut i8) = n;
                link.last = LastValue::Int(n as i64);
            }
            TCL_LINK_UCHAR => {
                let n = narrow::<u8>(&text, "variable must have unsigned char value")?;
                *(addr as *mut u8) = n;
                link.last = LastValue::Int(n as i64);
            }
            TCL_LINK_SHORT => {
                let n = narrow::<i16>(&text, "variable must have short value")?;
                *(addr as *mut i16) = n;
                link.last = LastValue::Int(n as i64);
            }
            TCL_LINK_USHORT => {
                let n = narrow::<u16>(&text, "variable must have unsigned short value")?;
                *(addr as *mut u16) = n;
                link.last = LastValue::Int(n as i64);
            }
            TCL_LINK_UINT => {
                let n = narrow::<u32>(&text, "variable must have unsigned int value")?;
                *(addr as *mut u32) = n;
                link.last = LastValue::Int(n as i64);
            }
            TCL_LINK_WIDE_UINT => {
                let n: u64 = text
                    .trim()
                    .parse()
                    .map_err(|_| "variable must have unsigned wide int value")?;
                *(addr as *mut u64) = n;
                link.last = LastValue::Int(n as i64);
            }
            TCL_LINK_DOUBLE => {
                let f: f64 = text
                    .trim()
                    .parse()
                    .map_err(|_| "variable must have real value")?;
                *(addr as *mut f64) = f;
                link.last = LastValue::Float(f);
            }
            TCL_LINK_FLOAT => {
                let f: f64 = text
                    .trim()
                    .parse()
                    .map_err(|_| "variable must have float value")?;
                *(addr as *mut f32) = f as f32;
                link.last = LastValue::Float(f);
            }
            TCL_LINK_STRING => {
                // The C reallocs the caller's buffer and stores the new one
                // (`generic/tclLink.c:826-834`); the same, with `Tcl_Alloc`'s
                // allocator so the caller's `Tcl_Free` matches.
                let bytes = text.as_bytes();
                let fresh = libc::malloc(bytes.len() + 1) as *mut c_char;
                if fresh.is_null() {
                    return Err("internal error: linked variable couldn't be read");
                }
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), fresh as *mut u8, bytes.len());
                *fresh.add(bytes.len()) = 0;
                let slot = addr as *mut *mut c_char;
                if !(*slot).is_null() {
                    libc::free(*slot as *mut c_void);
                }
                *slot = fresh;
                link.last = LastValue::Text(fresh as usize);
            }
            _ => return Err("internal error: bad linked variable type"),
        }
    }
    Ok(())
}

fn wide(text: &str) -> Option<i64> {
    text.trim().parse().ok()
}

fn narrow<T: TryFrom<i64>>(text: &str, refusal: &'static str) -> Result<T, &'static str> {
    wide(text).ok_or(refusal)?.try_into().map_err(|_| refusal)
}

// ── the object-valued variable slots ─────────────────────────────────────────

/// Slot 195. `Tcl_ObjGetVar2` (`generic/tclVar.c`): the variable's value, or
/// NULL when it is unset.
///
/// This is the read `-textvariable` uses to pick up an existing value when the
/// widget is created (`tk9.0.4/generic/tkButton.c:1255`).
unsafe extern "C" fn obj_get_var2(
    interp: *mut c_void,
    part1: *mut TclObj,
    part2: *mut TclObj,
    _flags: c_int,
) -> *mut TclObj {
    entered!("tcl_ObjGetVar2");
    let name = obj::text_of(part1);
    let Some(shared) = shared_of(interp) else {
        return std::ptr::null_mut();
    };
    if !part2.is_null() {
        // Array elements are stored as a whole-name key by the string slots in
        // `host.rs`; nothing here reaches them, and answering NULL is the
        // truthful "not set" rather than a wrong value.
        return std::ptr::null_mut();
    }
    match runtime::global_of(&shared, &name) {
        Some(value) => cached_obj(interp, &name, &value),
        None => std::ptr::null_mut(),
    }
}

/// Slot 196. `Tcl_ObjSetVar2`. The write that fires the variable's write traces
/// — including, when Tk itself is the writer, the trace of another widget
/// watching the same variable.
unsafe extern "C" fn obj_set_var2(
    interp: *mut c_void,
    part1: *mut TclObj,
    part2: *mut TclObj,
    value: *mut TclObj,
    flags: c_int,
) -> *mut TclObj {
    entered!("tcl_ObjSetVar2");
    let name = obj::text_of(part1);
    note("ObjSetVar2", &name);
    if !part2.is_null() {
        return std::ptr::null_mut();
    }
    set_scalar(interp, &name, obj::to_value(value), flags)
}

/// Slot 317. `Tcl_SetVar2Ex`: the same write, with the name in two `char *`
/// pieces instead of two objects.
unsafe extern "C" fn set_var2_ex(
    interp: *mut c_void,
    part1: *const c_char,
    part2: *const c_char,
    value: *mut TclObj,
    flags: c_int,
) -> *mut TclObj {
    entered!("tcl_SetVar2Ex");
    let (name, index) = name_parts(part1, part2);
    note("SetVar2Ex", &name);
    if index.is_some() {
        return std::ptr::null_mut();
    }
    set_scalar(interp, &name, obj::to_value(value), flags)
}

/// Slot 254. `Tcl_UnsetVar2`: removes the variable and fires its unset traces.
///
/// This is the one unset [`crate::runtime`]'s projection cannot see for itself,
/// and firing here is why a widget whose `-textvariable` is unset re-creates it
/// (`tk9.0.4/generic/tkButton.c:1785-1789`).
unsafe extern "C" fn unset_var2(
    interp: *mut c_void,
    part1: *const c_char,
    part2: *const c_char,
    _flags: c_int,
) -> c_int {
    entered!("tcl_UnsetVar2");
    let (name, index) = name_parts(part1, part2);
    note("UnsetVar2", &name);
    if index.is_some() {
        return TCL_ERROR;
    }
    let Some(shared) = shared_of(interp) else {
        return TCL_ERROR;
    };
    if runtime::unset_global_of(&shared, &name) {
        TCL_OK
    } else {
        TCL_ERROR
    }
}

/// Store a scalar, fire its write traces, and answer with the object the
/// caller may keep — the shape all four setting slots share.
pub(super) fn set_scalar(
    interp: *mut c_void,
    name: &str,
    value: Value,
    flags: c_int,
) -> *mut TclObj {
    let Some(shared) = shared_of(interp) else {
        return std::ptr::null_mut();
    };
    if let Err(message) = runtime::set_global_of(&shared, name, value) {
        // `TCL_LEAVE_ERR_MSG` is what asks for the interpreter result to carry
        // the reason (`generic/tcl.h:1015`); without it the NULL is the whole
        // answer.
        if flags & TCL_LEAVE_ERR_MSG != 0 {
            unsafe { host::set_result_bytes(interp, message.as_bytes()) };
        }
        return std::ptr::null_mut();
    }
    // What the variable holds *after* its traces, not what was handed in. Tcl
    // re-reads for the same reason (`TclPtrSetVar` answers with the variable's
    // own object): a trace may have rewritten it, and a read-only linked
    // variable does exactly that.
    let stored = runtime::global_of(&shared, name).unwrap_or(Value::Str(Arc::new(String::new())));
    unsafe { cached_obj(interp, name, &stored) }
}

/// A `Tcl_Obj` for a variable's value that outlives the call.
///
/// Tcl answers these slots with the variable's own object, which lives as long
/// as the variable does. Nothing here owns an object per variable — the value is
/// a `fusevm::Value` — so one is built and kept in the host's variable table
/// under the variable's name, and the previous one is released. A caller that
/// retains what it is given (Tk does: `tkButton.c:1266-1267`) keeps it alive
/// past the next write on its own reference.
pub(super) unsafe fn cached_obj(interp: *mut c_void, name: &str, value: &Value) -> *mut TclObj {
    let fresh = obj::from_value(value);
    obj::incr_ref(fresh);
    let h = &mut *(*(interp as *mut HostInterp)).host;
    match h
        .vars
        .iter_mut()
        .find(|(n, i, _)| n == name && i.is_empty())
    {
        Some(entry) => {
            let old = std::mem::replace(&mut entry.2, fresh);
            obj::release(old);
        }
        None => h.vars.push((name.to_string(), String::new(), fresh)),
    }
    fresh
}

// ── plumbing ─────────────────────────────────────────────────────────────────

pub(super) fn shared_of(interp: *mut c_void) -> Option<runtime::Shared> {
    let host_ptr = unsafe { interp::host_of(interp) };
    if host_ptr.is_null() {
        return None;
    }
    Some(interp::shared_for(host_ptr))
}

unsafe fn c_string(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    String::from_utf8_lossy(CStr::from_ptr(p).to_bytes()).into_owned()
}

/// A two-part variable name, with the second part absent rather than empty when
/// it is NULL — the distinction Tcl draws between a scalar and an array element
/// whose index happens to be the empty string.
unsafe fn name_parts(part1: *const c_char, part2: *const c_char) -> (String, Option<String>) {
    let name = c_string(part1);
    // `TclCallVarTraces` splits `a(i)` into its two parts when the caller gave
    // one (`generic/tclTrace.c:2533-2557`); the same split, so a trace set with
    // the parenthesised spelling and one set with two arguments agree.
    if part2.is_null() {
        if let Some(open) = name.find('(') {
            if name.ends_with(')') {
                let index = name[open + 1..name.len() - 1].to_string();
                return (name[..open].to_string(), Some(index));
            }
        }
        return (name, None);
    }
    (name, Some(c_string(part2)))
}

/// Patch this module's slots into `t`, returning their indices.
///
/// `tcl_LinkVar` is not here: it is installed by [`super::host`] at the
/// measuring level, because Tk links five variables while it starts and the
/// probe has to get past that. Its body calls [`link_var`] above.
///
/// # Safety
/// Each erased signature is the one `tclDecls.h` gives the slot, quoted on the
/// line above it.
pub unsafe fn install_impls(t: &mut TclStubs) -> Vec<usize> {
    install_sink();
    vec![
        // int (*tcl_TraceVar2)(Tcl_Interp *, const char *, const char *, int,
        //     Tcl_VarTraceProc *, void *) /* 248 */
        install(t, "tcl_TraceVar2", trace_var2 as *const ()),
        // void (*tcl_UntraceVar2)(Tcl_Interp *, const char *, const char *, int,
        //     Tcl_VarTraceProc *, void *) /* 256 */
        install(t, "tcl_UntraceVar2", untrace_var2 as *const ()),
        // void *(*tcl_VarTraceInfo2)(Tcl_Interp *, const char *, const char *,
        //     int, Tcl_VarTraceProc *, void *) /* 262 */
        install(t, "tcl_VarTraceInfo2", var_trace_info2 as *const ()),
        // void (*tcl_UnlinkVar)(Tcl_Interp *, const char *) /* 251 */
        install(t, "tcl_UnlinkVar", unlink_var as *const ()),
        // void (*tcl_UpdateLinkedVar)(Tcl_Interp *, const char *) /* 257 */
        install(t, "tcl_UpdateLinkedVar", update_linked_var as *const ()),
        // Tcl_Obj *(*tcl_ObjGetVar2)(Tcl_Interp *, Tcl_Obj *, Tcl_Obj *, int) /* 195 */
        install(t, "tcl_ObjGetVar2", obj_get_var2 as *const ()),
        // Tcl_Obj *(*tcl_ObjSetVar2)(Tcl_Interp *, Tcl_Obj *, Tcl_Obj *,
        //     Tcl_Obj *, int) /* 196 */
        install(t, "tcl_ObjSetVar2", obj_set_var2 as *const ()),
        // Tcl_Obj *(*tcl_SetVar2Ex)(Tcl_Interp *, const char *, const char *,
        //     Tcl_Obj *, int) /* 317 */
        install(t, "tcl_SetVar2Ex", set_var2_ex as *const ()),
        // int (*tcl_UnsetVar2)(Tcl_Interp *, const char *, const char *, int) /* 254 */
        install(t, "tcl_UnsetVar2", unset_var2 as *const ()),
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
