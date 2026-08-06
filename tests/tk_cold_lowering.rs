//! What a `--features tk` build compiles when no Tk interpreter exists.
//!
//! Its own file because the question is about a *process* that has never built
//! a host, and `tests/tk_eval.rs` builds one. Cargo gives each integration test
//! file its own binary, which is the only way to ask this question honestly.
//!
//! The claim under test is the one that makes the feature safe to leave on:
//! turning `tk` on must not change how an ordinary script is lowered. It cannot
//! any more — a name no module claims is a run-time lookup in both feature sets,
//! because a procedure another chunk defined answers to one — so what this file
//! polices is that the lookup stays confined to names nothing resolves, and that
//! a call the compiler *can* resolve gains no extension op. An extension op is
//! what stops a loop being trace-compiled, which is why the benchmark is counted
//! here too.

#![cfg(feature = "tk")]

use tclrs::compiler::ext;

/// No host, so no dynamic dispatch on offer.
#[test]
fn without_a_host_no_name_is_taken_over() {
    assert!(!tclrs::tk::dispatch::may_exist());
    assert!(!tclrs::tk::dispatch::takes_over("button"));
    assert!(!tclrs::tk::dispatch::takes_over("nosuchcommand"));
}

/// An unknown name is a run-time lookup, and refuses exactly as it always did.
///
/// This assertion was `dispatches == 0`: a cold process was required to lower an
/// unknown name to the compiler's deferred refusal rather than to a dispatch op,
/// so that turning the feature on could not change a script's lowering. Both
/// halves of that reason are still served, differently.
///
/// A name no module claims is now a lookup in *either* feature set, because a
/// procedure another chunk defined answers to one — `source`, `eval` and a
/// binding script are chunks of their own, and a `proc` at one script's top level
/// is callable from all of them. So the lowering no longer depends on whether a
/// Tk interpreter exists, which is what this file exists to police, and it is the
/// same lowering with the feature off (`tests/expand_differential.rs` counts the
/// same op there).
///
/// What must not change is the refusal, and it has not: the message is the
/// compiler's own wording, it is raised when the command is *reached* rather than
/// while the script is read, and a command in a branch that never runs is not an
/// error. The op that raises it is the one the run-time table already used.
#[test]
fn an_unknown_name_lowers_to_one_run_time_lookup_and_still_refuses() {
    let chunk = tclrs::runtime::compile("nosuchcommand a b").expect("lowers");
    let dispatches = chunk
        .ops
        .iter()
        .filter(|op| matches!(op, fusevm::Op::Extended(ext::DYN_CALL, _)))
        .count();
    assert_eq!(dispatches, 1, "one lookup for the one unresolvable name");

    let err = tclrs::eval("nosuchcommand a b").unwrap_err();
    assert!(
        err.contains("invalid command name \"nosuchcommand\""),
        "{err}"
    );
    // Reached, not read: `catch` traps it and a branch never taken never fails.
    assert_eq!(
        tclrs::eval("puts [catch {nosuchcommand} m]\nputs $m")
            .expect("the script itself is fine")
            .output,
        "1\ninvalid command name \"nosuchcommand\"\n"
    );
    assert_eq!(
        tclrs::eval("if {0} {nosuchcommand}\nset x done")
            .expect("a branch never taken cannot fail")
            .result,
        "done"
    );
    // A name the compiler *can* resolve gains nothing.
    let known = tclrs::runtime::compile("proc f {} {return 1}\nputs [f]").expect("lowers");
    assert!(
        !known
            .ops
            .iter()
            .any(|op| matches!(op, fusevm::Op::Extended(ext::DYN_CALL, _))),
        "a resolvable call became a lookup"
    );
}

/// The benchmark whose trace eligibility the tiers report measures, lowered
/// under `--features tk`.
///
/// `--tiers bench/counted_loop_proc.tcl` has to keep reporting `traced=true`,
/// and it will as long as the loop body carries no extension op it did not
/// carry before. Counting the ops here is the cheap version of that check, run
/// on every `cargo test --features tk` rather than by hand.
#[test]
fn the_traced_benchmark_gains_no_extension_op() {
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/bench/counted_loop_proc.tcl"
    ))
    .expect("read the benchmark");
    let chunk = tclrs::runtime::compile(&src).expect("lowers");
    assert!(
        !chunk
            .ops
            .iter()
            .any(|op| matches!(op, fusevm::Op::Extended(ext::DYN_CALL, _))),
        "the benchmark grew a dynamic-dispatch op"
    );
}
