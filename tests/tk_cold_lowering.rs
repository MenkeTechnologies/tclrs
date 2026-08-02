//! What a `--features tk` build compiles when no Tk interpreter exists.
//!
//! Its own file because the question is about a *process* that has never built
//! a host, and `tests/tk_eval.rs` builds one. Cargo gives each integration test
//! file its own binary, which is the only way to ask this question honestly.
//!
//! The claim under test is the one that makes the feature safe to leave on:
//! turning `tk` on must not change how an ordinary script is lowered, because
//! the dynamic-dispatch arm is guarded by "a Tk interpreter exists" and nothing
//! here has made one. If that guard ever stops holding, an extension op appears
//! in scripts that had none — and an extension op is what stops a loop being
//! trace-compiled.

#![cfg(feature = "tk")]

use tclrs::compiler::ext;

/// No host, so no dynamic dispatch on offer.
#[test]
fn without_a_host_no_name_is_taken_over() {
    assert!(!tclrs::tk::dispatch::may_exist());
    assert!(!tclrs::tk::dispatch::takes_over("button"));
    assert!(!tclrs::tk::dispatch::takes_over("nosuchcommand"));
}

/// An unknown name is the deferred refusal it has always been, not an op.
#[test]
fn an_unknown_name_still_lowers_to_the_deferred_refusal() {
    let chunk = tclrs::runtime::compile("nosuchcommand a b").expect("lowers");
    let dispatches = chunk
        .ops
        .iter()
        .filter(|op| matches!(op, fusevm::Op::Extended(ext::TK_DISPATCH, _)))
        .count();
    assert_eq!(dispatches, 0, "a cold process emitted a dispatch op");

    let err = tclrs::eval("nosuchcommand a b").unwrap_err();
    assert!(
        err.contains("invalid command name \"nosuchcommand\""),
        "{err}"
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
            .any(|op| matches!(op, fusevm::Op::Extended(ext::TK_DISPATCH, _))),
        "the benchmark grew a dynamic-dispatch op"
    );
}
