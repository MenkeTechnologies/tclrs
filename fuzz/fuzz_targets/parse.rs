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

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(src) = std::str::from_utf8(data) else {
        return;
    };
    if src.len() > 65_536 {
        return;
    }
    let _ = tclrs::parse(src);
});
