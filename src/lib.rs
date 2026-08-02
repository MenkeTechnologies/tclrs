//! tclrs — Tcl as a fusevm frontend.
//!
//! Pipeline: `parser` turns a script into `parser::Script` → the compiler
//! (phase 2) lowers each command to a `fusevm::Chunk` → fusevm executes it and
//! calls back into the host for Tcl-specific operations. Execution and codegen
//! live in fusevm; there is no bespoke VM or JIT here.
//!
//! Tcl's value model needs no object heap on top of fusevm's: strings, integers
//! and floats map onto `Value` directly, and a value keeps its numeric
//! representation until something demands its string form. That deferral is the
//! point — the reference interpreter re-derives string representations inside
//! hot loops.
//!
//! Execution reaches for fusevm's higher tiers, not just its interpreter: every
//! VM this crate builds has the three-tier Cranelift JIT armed
//! ([`runtime::install_hooks`]), [`aot`] compiles a script to a native object
//! and links it into a standalone binary, and [`tiers`] reports which of those
//! tiers a given script's bytecode actually reaches.

pub mod aot;
pub mod aot_runtime;
pub mod assoc;
pub mod cache;
pub mod cmd_list;
pub mod cmd_namespace;
pub mod cmd_source;
pub mod cmd_string;
pub mod compiler;
pub mod control;
pub mod coro;
pub mod cursor;
pub mod dap;
pub mod dump;
pub mod expr;
pub mod list;
pub mod lsp;
pub mod names;
pub mod parser;
pub mod procs;
pub mod regexp;
pub mod runtime;
pub mod rust_ffi;
pub mod tiers;
/// Hosting the real Tk toolkit. Behind the `tk` feature; see `src/tk/mod.rs`.
#[cfg(feature = "tk")]
pub mod tk;

pub use parser::{parse, Command, ParseError, Part, Script, Word};
pub use runtime::{eval, eval_captured, Interp, Outcome, TclError};
