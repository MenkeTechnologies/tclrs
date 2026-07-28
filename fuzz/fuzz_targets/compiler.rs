//! Fuzz the parser and the compiler together, without running anything.
//!
//! The compiler has a panic surface of its own that the parser does not: slot
//! allocation for a procedure's locals, the loop and `catch` context stacks, the
//! name pool, and the jump patching a `break` out of a nested body needs. An
//! input that parses but cannot be lowered must report a `CompileError`, never
//! panic.
//!
//! Execution is deliberately not fuzzed here. A generated script may write an
//! unbounded `while`, so a target that ran one would report libfuzzer timeouts
//! rather than bugs; `scripts/fuzz_parity.sh` runs scripts instead, with a
//! per-process timeout on both engines and a generator whose loops are bounded
//! structurally.
//!
//! Run under cargo-fuzz:
//!   cargo +nightly fuzz run compiler
#![no_main]
#![allow(non_upper_case_globals)]

use libfuzzer_sys::fuzz_target;

#[allow(dead_code)]
#[path = "shared.rs"]
mod shared;

fuzz_target!(|data: &[u8]| {
    let Some(src) = shared::source(data) else {
        return;
    };
    // On a thread of `runtime::RECOMMENDED_STACK` — see
    // `shared::on_deep_stack`. Lowering recurses on the same nesting the parser
    // does, so it needs the same stack.
    let src = src.to_string();
    shared::on_deep_stack(move || {
        let _ = tclrs::runtime::compile(&src);
    });
});
