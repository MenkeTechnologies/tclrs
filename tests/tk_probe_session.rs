//! The probe run itself, as a test.
//!
//! Runs `tk-probe` against the real Tk and checks the two things that would
//! silently stop being true if the ABI drifted: that `Tk_Init` gets past
//! `Tcl_InitStubs` at all, and that the run ends where it is supposed to end
//! rather than somewhere earlier.
//!
//! The probe aborts on purpose when it reaches an unimplemented slot, so a
//! non-zero exit is the expected outcome and the assertions are on its output.
//!
//! Needs a Tk dylib, and is skipped without one. `cargo test --features tk` on
//! a machine with no Tk still passes.

#![cfg(feature = "tk")]

use std::process::Command;

/// Run the probe and return its stderr, or `None` if there is no Tk to run
/// against.
fn probe(degraded: bool) -> Option<String> {
    let exe = std::path::Path::new(env!("CARGO_BIN_EXE_tk-probe"));
    let mut cmd = Command::new(exe);
    if degraded {
        cmd.env("TCLRS_TK_DEGRADED", "1");
    }
    let out = cmd.output().expect("run tk-probe");
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    if err.contains("no Tk dylib at") || err.contains("dlopen(") {
        eprintln!("skipping: {}", err.trim());
        return None;
    }
    Some(err)
}

/// The names of the slots called, in order, one entry per call.
fn call_log(stderr: &str) -> Vec<&str> {
    stderr
        .lines()
        .filter(|l| l.starts_with("tkslot "))
        .filter_map(|l| l.split_whitespace().nth(4))
        .collect()
}

/// The slot the run stopped on, if it stopped on one.
fn trap(stderr: &str) -> Option<(usize, String)> {
    let line = stderr.lines().find(|l| l.starts_with("tktrap "))?;
    let f: Vec<&str> = line.split_whitespace().collect();
    Some((f[3].parse().ok()?, f[4].to_string()))
}

#[test]
fn tk_accepts_the_interpreter_and_asks_for_a_package_first() {
    let Some(err) = probe(false) else { return };
    let log = call_log(&err);

    // The first call is not arbitrary. Tk's very first act is
    // `Tcl_InitStubs(interp, "9.0", 0)` (tk9.0.4/generic/tkWindow.c:3218),
    // which checks the magic and then calls exactly one thing —
    // `stubsPtr->tcl_PkgRequireEx` (generic/tclStubLib.c:101). Seeing this
    // first proves the interpreter layout and the magic were both accepted;
    // seeing nothing would mean Tcl_InitStubs bailed at the magic check.
    assert_eq!(
        log.first().copied(),
        Some("tcl_PkgRequireEx"),
        "Tk's first stub call should be the one Tcl_InitStubs makes"
    );

    // Then the object types: `TkRegisterObjTypes` registers ten of Tk's own
    // before anything else happens (tk9.0.4/generic/tkObj.c:1223-1232).
    assert_eq!(
        log.iter().filter(|n| **n == "tcl_RegisterObjType").count(),
        10,
        "TkRegisterObjTypes registers exactly ten Tcl_ObjTypes"
    );
}

#[test]
fn an_undegraded_run_stops_at_the_variadic_slot() {
    let Some(err) = probe(false) else { return };
    let (slot, name) = trap(&err).expect("the run should stop on an unimplemented slot");

    // Tcl_AppendStringsToObj is variadic and its variadic arguments carry the
    // payload, which stable Rust cannot read: defining a C-variadic function is
    // rejected with E0658. A correct run therefore cannot get past slot 15, and
    // if it ever does, either the toolchain changed or something was faked.
    assert_eq!((slot, name.as_str()), (15, "tcl_AppendStringsToObj"));
}

#[test]
fn a_degraded_run_reaches_the_first_call_that_wants_a_real_interpreter() {
    let Some(err) = probe(true) else { return };
    let (slot, name) = trap(&err).expect("the degraded run should stop on an unimplemented slot");

    // With the variadic slot faked out, Tk gets as far as asking for a script
    // to be evaluated: `Tcl_EvalEx(interp, "file tildeexpand ~/.Xdefaults", ...)`
    // (tk9.0.4/generic/tkOption.c:1592). That is the boundary this whole
    // exercise was looking for — everything before it can be answered with a
    // data structure, and this cannot be answered with anything short of an
    // evaluator.
    assert_eq!((slot, name.as_str()), (291, "tcl_EvalEx"));

    // And it got there having created a second interpreter, which is the other
    // thing no placeholder covers (tk9.0.4/generic/tkOption.c:1497).
    assert!(
        call_log(&err).contains(&"tcl_CreateInterp"),
        "the degraded run should reach Tcl_CreateInterp"
    );
}

#[test]
fn the_degraded_run_is_a_strict_extension_of_the_correct_one() {
    let (Some(plain), Some(degraded)) = (probe(false), probe(true)) else {
        return;
    };
    let a = call_log(&plain);
    let b = call_log(&degraded);
    // Faking one slot must not change what Tk did before it. If the prefixes
    // diverge, the degraded log cannot be read as a continuation of the honest
    // one and the enumeration past slot 15 means nothing.
    assert!(b.len() > a.len());
    assert_eq!(
        a,
        b[..a.len()],
        "the two runs diverge before the faked slot"
    );
}

#[test]
fn tk_manages_the_refcount_of_objects_it_did_not_allocate() {
    let Some(err) = probe(false) else { return };
    let log = call_log(&err);

    // Every value in the run was allocated with `libc::malloc` on the Rust
    // side; Tcl's own allocator was never involved. `Tcl_IncrRefCount` and
    // `Tcl_DecrRefCount` are macros that write `objPtr->refCount` directly and
    // never consult the stub table (generic/tcl.h:2517-2531), so the only
    // visible trace of them is `TclFreeObj` (slot 30) firing when a count Tk
    // decremented reaches zero.
    //
    // Seeing those calls at all is the measurement: Tk's inline arithmetic
    // landed on offset 0 of a foreign allocation and produced the right answer.
    // `tcl_free_obj` asserts the count is non-positive on arrival, so a wrong
    // layout would abort the run there instead of reaching them.
    assert!(
        log.contains(&"tclFreeObj"),
        "Tk should have released at least one value it never allocated"
    );
    let created = log
        .iter()
        .filter(|n| matches!(**n, "tcl_NewObj" | "tcl_NewStringObj" | "tcl_NewListObj"))
        .count();
    assert!(created > 0, "Tk should have asked for values");
}
