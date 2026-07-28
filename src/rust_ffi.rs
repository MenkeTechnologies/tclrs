//! Inline Rust: `rust { ... }` blocks in a Tcl script.
//!
//! ```tcl
//! rust {
//!     pub extern "C" fn add(a: i64, b: i64) -> i64 { a + b }
//! }
//! puts [add 21 21]
//! ```
//!
//! `rust {` is not a Tcl command, so the block never reaches the parser: the
//! source is rewritten first, and the block becomes an ordinary command,
//! `__rust_compile <base64> <line>`. Everything after that is Tcl the parser
//! already understands.
//!
//! The work behind it belongs to [`fusevm::ffi`], which compiles the block to a
//! cdylib (cached by the hash of its body), `dlopen`s it, and registers every
//! `pub extern "C" fn` it exports. This module only supplies the Tcl-flavored
//! [`fusevm::RustSugar`] and the two questions the compiler asks: is this
//! source worth rewriting, and is this name one the block exported.
//!
//! **Registration happens while compiling, not while running.** Tcl dispatch is
//! resolved by this frontend at compile time — a name is a builtin, a
//! procedure, a coroutine or an error before the VM ever starts — so a call to
//! an exported function has to be resolvable then. The compiler runs
//! `__rust_compile` as it lowers it (`Compiler::cmd_rust_compile`), which is
//! why `add` is a known name by the time the next command is lowered. The
//! ordering that makes this sound is the script's own: a block is written above
//! the calls to it, and `fusevm::ffi` is idempotent per body.

use fusevm::{Op, RustSugar};

use crate::compiler::{ext, CompileError, Compiler};
use crate::parser::Word;

/// The command a block becomes. Leading underscores are ordinary characters in
/// a Tcl command name, and no script would define this one by accident.
pub const COMPILE_COMMAND: &str = "__rust_compile";

/// Emit the Tcl command a `rust { ... }` block desugars to. The base64 body is
/// alphanumeric with `+`, `/` and `=`, none of which need quoting in a Tcl
/// word, so the command is written bare.
fn emit(body_b64: &str, line: usize) -> String {
    format!("{COMPILE_COMMAND} {body_b64} {line}")
}

/// Tcl's flavor of the block syntax.
///
/// `newline_boundary` is true because a Tcl command ends at a newline or a `;`,
/// so `rust {` is recognized only where a command may start. `#` is the comment
/// introducer: in Tcl it only opens a comment where a command would start,
/// while the scanner skips one anywhere — the difference can only hide a block
/// written on the same line as a `#` inside a word, which is not a thing anyone
/// writes.
pub const SUGAR: RustSugar = RustSugar {
    keyword: "rust",
    line_comments: &["#"],
    block_comment: None,
    newline_boundary: true,
    emit,
};

/// Rewrite every top-level `rust { ... }` block into its command. A single
/// substring scan when the source has no `rust` in it, which is the usual case.
pub fn desugar(src: &str) -> String {
    SUGAR.desugar(src)
}

/// Whether `name` was exported by a block this process has compiled.
pub fn is_exported(name: &str) -> bool {
    fusevm::ffi::is_registered(name)
}

/// Compile and register a block's body, reporting a failure the way the rest of
/// the frontend reports one.
pub fn register(body_b64: &str) -> Result<(), String> {
    fusevm::ffi::compile_and_register(body_b64)
}

/// Call an exported function. `None` when the name is not one, which the
/// compiler has already ruled out by the time this runs.
pub fn call(name: &str, args: &[fusevm::Value]) -> Result<fusevm::Value, String> {
    fusevm::ffi::try_call(name, args)
        .unwrap_or_else(|| Err(format!("invalid command name \"{name}\"")))
}

impl Compiler {
    /// `__rust_compile <base64> <line>` — what a `rust { ... }` block was
    /// rewritten into.
    ///
    /// The block is compiled and registered *here*, while this command is being
    /// lowered, because the calls to it are resolved while the commands below
    /// are lowered. The emitted code is nothing: the block's effect on the
    /// running program is that its functions exist, and they exist as soon as
    /// the process that compiled them is running.
    pub(crate) fn cmd_rust_compile(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let [body, line] = args else {
            return self.error("wrong # args: should be \"__rust_compile base64 line\"");
        };
        let body = self.literal_of(body, "the block body")?.to_string();
        // The line the block was written on, which the rewrite carried along so
        // that a failure inside it is reported where the Rust is, not where the
        // rewritten command ended up.
        if let Some(line) = line.as_literal().and_then(|l| l.parse::<usize>().ok()) {
            self.line = line;
            self.command_line = line;
        }
        if let Err(e) = register(&body) {
            return self.error(e);
        }
        self.push_empty();
        Ok(())
    }

    /// A call to a function an inline block exported: the name, then the
    /// arguments, then the op that pops all of them.
    pub(crate) fn call_ffi(&mut self, name: &str, args: &[Word]) -> Result<(), CompileError> {
        let count = u8::try_from(args.len() + 1).map_err(|_| {
            self.err(format!(
                "more than 254 arguments to the rust function \"{name}\""
            ))
        })?;
        self.push_text(name);
        for arg in args {
            self.word(arg)?;
        }
        self.emit(Op::Extended(ext::FFI_CALL, count), -(args.len() as i32));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_becomes_a_command_and_the_rust_leaves_the_source() {
        let src = "rust {\n    pub extern \"C\" fn add(a: i64, b: i64) -> i64 { a + b }\n}\nputs [add 2 3]\n";
        let out = desugar(src);
        assert!(out.contains(COMPILE_COMMAND), "{out}");
        assert!(!out.contains("pub extern"), "the Rust body leaked: {out}");
        assert!(out.contains("puts [add 2 3]"), "{out}");
        // The rewrite keeps the line count, so a later error still points at
        // the line it was written on.
        assert_eq!(out.lines().count(), src.lines().count());
    }

    #[test]
    fn a_script_without_a_block_is_returned_as_it_was() {
        let src = "set x 1\nputs $x\n";
        assert_eq!(desugar(src), src);
    }

    /// The keyword inside a comment is not a block: a script that mentions it
    /// must still parse as the Tcl it is.
    #[test]
    fn the_keyword_in_a_comment_is_left_alone() {
        let src = "# rust { not a block }\nset x 1\n";
        assert_eq!(desugar(src), src);
    }

    /// And the keyword as an argument is not a block either — only a statement
    /// boundary starts one.
    #[test]
    fn the_keyword_as_a_word_is_left_alone() {
        let src = "puts rust\nset rust 1\n";
        assert_eq!(desugar(src), src);
    }
}
