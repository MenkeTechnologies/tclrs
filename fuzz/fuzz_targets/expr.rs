//! Fuzz the `expr` grammar.
//!
//! `expr` is a second grammar layered on Tcl's word syntax, with a parser of its
//! own (`src/expr.rs`) that the `parse` target never reaches: `tclrs::parse`
//! stops at the word, and only the compiler goes on to read it as an
//! expression. Its panic surface is its own — recursive descent over eleven
//! precedence levels, a number scanner with radix prefixes, and operands that
//! call back into the command parser for `$x`, `[cmd]`, `"…"` and `{…}`.
//!
//! Two calls per input, because the two halves fail differently: the parser
//! alone, on the input as an expression, and the whole pipeline on `expr {…}`,
//! which adds the lowering of whatever tree came out. An input that parses but
//! cannot be lowered must report a `CompileError`, never panic.
//!
//! Run under cargo-fuzz:
//!   cargo +nightly fuzz run expr
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
    // On a thread of `runtime::RECOMMENDED_STACK`, which is what found the need
    // for `shared::on_deep_stack`: `expr::MAX_EXPR_DEPTH` is calibrated for that
    // stack, so the corpus' 8_000-parenthesis seed overflowed libfuzzer's main
    // thread while the `tclrs` binary reported a Tcl error for it.
    let src = src.to_string();
    shared::on_deep_stack(move || {
        let _ = tclrs::expr::parse(&src);
        // Braced, so the expression reaches the compiler as one word rather
        // than as several. An input with unbalanced braces is refused by the
        // command parser before the expression parser sees it, which costs one
        // cheap execution.
        let _ = tclrs::runtime::compile(&format!("expr {{{src}}}"));
    });
});
