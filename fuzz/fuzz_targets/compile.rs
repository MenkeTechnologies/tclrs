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
//!   cargo +nightly fuzz run compile
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(src) = std::str::from_utf8(data) else {
        return;
    };
    if src.len() > 65_536 {
        return;
    }
    let _ = tclrs::runtime::compile(src);
});
