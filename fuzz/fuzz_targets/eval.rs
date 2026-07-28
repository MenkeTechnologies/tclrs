//! Fuzz the runtime `eval` path: compile a script and run it.
//!
//! The parse and compile targets stop before anything executes, so nothing they
//! reach can find a panic in a *command* — and the commands are where the
//! crashes have been: `format`'s field width, `string repeat`'s count, a list
//! index, an arithmetic conversion. This target runs the script.
//!
//! The input is not fed to the VM as bytes. `shared::script` builds a Tcl
//! program from it — fixed command skeletons with the fuzzer's bytes as their
//! arguments — because a byte string is a weak input for a runtime: almost every
//! mutation of one is a parse error, so the VM is never reached.
//!
//! Bounded on both axes that would otherwise report noise instead of bugs:
//! every generated loop counts to a literal, and the interpreter's recursion
//! limit is lowered from `runtime::DEFAULT_RECURSION_LIMIT`, which is calibrated
//! for `runtime::RECOMMENDED_STACK` and not for the stack libfuzzer runs a
//! target on. That is what `Interp::set_recursion_limit` is documented for.
//!
//! Run under cargo-fuzz:
//!   cargo +nightly fuzz run eval
#![no_main]
#![allow(non_upper_case_globals)]

use libfuzzer_sys::fuzz_target;

#[allow(dead_code)]
#[path = "shared.rs"]
mod shared;

/// How deep a generated script may nest evaluations. Far under the default,
/// because the target runs on libfuzzer's stack rather than on a thread of
/// [`tclrs::runtime::RECOMMENDED_STACK`].
const RECURSION_LIMIT: usize = 32;

fuzz_target!(|data: &[u8]| {
    if data.len() > shared::MAX_INPUT {
        return;
    }
    let src = shared::script(data);
    shared::on_deep_stack(move || {
        // Capturing, so a generated `puts` writes into a buffer this call drops
        // rather than onto the fuzzer's own stdout.
        let mut interp = tclrs::Interp::capturing();
        interp.set_recursion_limit(RECURSION_LIMIT);
        let _ = interp.eval(&src);
    });
});
