//! Fuzz the parser.
//!
//! Needs no tclsh: the property under test is that arbitrary bytes either parse
//! or produce a `ParseError`, and never panic. That covers what the
//! differential fuzzer cannot reach — `scripts/fuzz_parity.sh` only ever feeds
//! tclrs text a grammar produced, so it never sees a lone `\x00`, a truncated
//! `\u{` escape, or three thousand nested braces.
//!
//! Run under cargo-fuzz:
//!   cargo +nightly fuzz run parse
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
    // On a thread of `runtime::RECOMMENDED_STACK`: the parser's own depth bound
    // is calibrated for that stack, not for the 8 MiB libfuzzer's main thread
    // has. See `shared::on_deep_stack`.
    let src = src.to_string();
    shared::on_deep_stack(move || {
        let _ = tclrs::parse(&src);
    });
});
