//! Fuzz a compiled chunk through the VM.
//!
//! `eval` reaches the VM through the interpreter's chunk cache, which compiles
//! a script once and keeps it. This target takes the chunk itself
//! (`runtime::compile`) and hands it to `Interp::run_chunk`, then runs the *same
//! chunk again on a second interpreter*. That second run is the property under
//! test: a chunk is not consumed by being executed, and lowering that depended
//! on the state of the VM it first ran on — a slot index, a name-pool entry, a
//! jump patched at run time — would show up as a panic or a different answer the
//! second time round.
//!
//! The input is turned into a Tcl program by `shared::script` for the same
//! reason as in the `eval` target: raw bytes rarely reach the VM at all.
//!
//! Run under cargo-fuzz:
//!   cargo +nightly fuzz run vm
#![no_main]
#![allow(non_upper_case_globals)]

use libfuzzer_sys::fuzz_target;

#[allow(dead_code)]
#[path = "shared.rs"]
mod shared;

/// As in the `eval` target: the target runs on libfuzzer's stack, not on a
/// thread of [`tclrs::runtime::RECOMMENDED_STACK`].
const RECURSION_LIMIT: usize = 32;

fn interp() -> tclrs::Interp {
    let mut interp = tclrs::Interp::capturing();
    interp.set_recursion_limit(RECURSION_LIMIT);
    interp
}

fuzz_target!(|data: &[u8]| {
    if data.len() > shared::MAX_INPUT {
        return;
    }
    let src = shared::script(data);
    shared::on_deep_stack(move || {
        let Ok(chunk) = tclrs::runtime::compile(&src) else {
            return;
        };
        let _ = interp().run_chunk(chunk.clone());
        let _ = interp().run_chunk(chunk);
    });
});
