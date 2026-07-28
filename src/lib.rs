//! tclrs — Tcl as a fusevm frontend.
//!
//! Pipeline: `parser` turns a script into [`parser::Script`] → the compiler
//! (phase 2) lowers each command to a `fusevm::Chunk` → fusevm executes it and
//! calls back into the host for Tcl-specific operations. Execution and codegen
//! live in fusevm; there is no bespoke VM or JIT here.
//!
//! Tcl's value model needs no object heap on top of fusevm's: strings, integers
//! and floats map onto `Value` directly, and a value keeps its numeric
//! representation until something demands its string form. That deferral is the
//! point — the reference interpreter re-derives string representations inside
//! hot loops.

pub mod assoc;
pub mod cache;
pub mod cmd_list;
pub mod cmd_string;
pub mod compiler;
pub mod control;
pub mod coro;
pub mod expr;
pub mod list;
pub mod parser;
pub mod procs;
pub mod runtime;

pub use parser::{parse, Command, ParseError, Part, Script, Word};
pub use runtime::{eval, Interp, Outcome, TclError};
