//! Lowering: [`parser::Script`] → `fusevm::Chunk`.
//!
//! Every command leaves exactly one value on the stack — its result — because
//! a Tcl script's value is the value of its last command, and command
//! substitution needs the same thing from a nested script. The compiler tracks
//! that depth statically, which is what lets `break` and `continue` unwind to a
//! balanced stack with a known number of pops instead of a runtime unwinder.
//!
//! Operations whose Tcl semantics differ from the VM's generic ones — integer
//! division and remainder floor toward negative infinity, `**` stays integral
//! for integral operands — are frontend extension ops rather than the VM's
//! `Div`/`Mod`/`Pow`, as fusevm's own documentation directs for frontends whose
//! arithmetic differs. Everything else lowers to native ops so the JIT can see
//! it.

use fusevm::{ChunkBuilder, Op, Value};
use std::fmt;

use crate::expr::{self, BinOp, Expr, UnOp};
use crate::parser::{Command, Part, Script, Word};

/// Extension opcode ids owned by this frontend.
pub mod ext {
    pub const DIV: u16 = 0;
    pub const MOD: u16 = 1;
    pub const POW: u16 = 2;
    pub const IN: u16 = 3;
    pub const NI: u16 = 4;
    /// Convert a VM-native result into its Tcl value: booleans become 1 or 0,
    /// doubles take Tcl's formatting.
    pub const NORM: u16 = 5;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileError {
    pub msg: String,
    pub line: usize,
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (line {})", self.msg, self.line)
    }
}

impl std::error::Error for CompileError {}

/// Compile a parsed script into a chunk whose result is the script's value.
pub fn compile(script: &Script) -> Result<fusevm::Chunk, CompileError> {
    let mut c = Compiler {
        b: ChunkBuilder::new(),
        depth: 0,
        loops: Vec::new(),
        line: 1,
    };
    c.script_value(script)?;
    Ok(c.b.build())
}

struct LoopCtx {
    /// Stack depth on entry, so an early exit knows how much to discard.
    depth: usize,
    breaks: Vec<usize>,
    continues: Vec<usize>,
}

struct Compiler {
    b: ChunkBuilder,
    depth: usize,
    loops: Vec<LoopCtx>,
    line: usize,
}

impl Compiler {
    fn emit(&mut self, op: Op, delta: i32) -> usize {
        let idx = self.b.emit(op, self.line as u32);
        self.depth = (self.depth as i32 + delta) as usize;
        idx
    }

    fn error<T>(&self, msg: impl Into<String>) -> Result<T, CompileError> {
        Err(CompileError {
            msg: msg.into(),
            line: self.line,
        })
    }

    fn push_value(&mut self, v: Value) {
        let idx = self.b.add_constant(v);
        self.emit(Op::LoadConst(idx), 1);
    }

    fn push_empty(&mut self) {
        self.push_value(Value::Str(std::sync::Arc::new(String::new())));
    }

    // ── scripts ──────────────────────────────────────────────────────────

    /// Emit a script that leaves its value on the stack.
    fn script_value(&mut self, script: &Script) -> Result<(), CompileError> {
        if script.commands.is_empty() {
            self.push_empty();
            return Ok(());
        }
        for (i, cmd) in script.commands.iter().enumerate() {
            if i > 0 {
                self.emit(Op::Pop, -1);
            }
            self.command(cmd)?;
        }
        Ok(())
    }

    /// Emit a script for its effect, leaving the stack as it was found.
    fn script_effect(&mut self, script: &Script) -> Result<(), CompileError> {
        for cmd in &script.commands {
            self.command(cmd)?;
            self.emit(Op::Pop, -1);
        }
        Ok(())
    }

    // ── words ────────────────────────────────────────────────────────────

    /// Emit a word, leaving its value on the stack.
    fn word(&mut self, word: &Word) -> Result<(), CompileError> {
        if word.expand {
            return self.error("{*} argument expansion is not supported yet");
        }
        match word.parts.len() {
            0 => self.push_empty(),
            1 => self.part(&word.parts[0])?,
            _ => {
                self.part(&word.parts[0])?;
                for part in &word.parts[1..] {
                    self.part(part)?;
                    self.emit(Op::Concat, -1);
                }
            }
        }
        Ok(())
    }

    fn part(&mut self, part: &Part) -> Result<(), CompileError> {
        match part {
            Part::Lit(text) => {
                self.push_value(literal_value(text));
                Ok(())
            }
            Part::Var(name) => {
                let idx = self.b.add_name(name);
                self.emit(Op::GetVar(idx), 1);
                Ok(())
            }
            Part::Elem { .. } => self.error("array variables are not supported yet"),
            Part::Script(script) => self.script_value(script),
        }
    }

    /// The literal text of a word, when the compiler needs it at compile time
    /// (a command name, a variable name, a braced body).
    fn literal_of<'w>(&self, word: &'w Word, what: &str) -> Result<&'w str, CompileError> {
        word.as_literal().ok_or_else(|| CompileError {
            msg: format!("{what} must be a literal in this phase"),
            line: self.line,
        })
    }

    /// A variable name for a command that writes one. `a(i)` names an array
    /// element even though the parser hands it over as ordinary text — the
    /// parentheses are only syntax inside a `$` substitution.
    fn var_name_of(&self, word: &Word) -> Result<String, CompileError> {
        let name = self.literal_of(word, "variable name")?;
        if name.ends_with(')') && name.contains('(') {
            return self.error("array variables are not supported yet");
        }
        Ok(name.to_string())
    }

    // ── commands ─────────────────────────────────────────────────────────

    fn command(&mut self, cmd: &Command) -> Result<(), CompileError> {
        self.line = cmd.line;
        let Some(first) = cmd.words.first() else {
            self.push_empty();
            return Ok(());
        };
        let name = self.literal_of(first, "command name")?.to_string();
        let args = &cmd.words[1..];

        match name.as_str() {
            "set" => self.cmd_set(args),
            "puts" => self.cmd_puts(args),
            "expr" => self.cmd_expr(args),
            "incr" => self.cmd_incr(args),
            "if" => self.cmd_if(args),
            "while" => self.cmd_while(args),
            "break" => self.cmd_loop_exit(args, true),
            "continue" => self.cmd_loop_exit(args, false),
            other => self.error(format!("invalid command name \"{other}\"")),
        }
    }

    fn cmd_set(&mut self, args: &[Word]) -> Result<(), CompileError> {
        match args.len() {
            1 => {
                let name = self.var_name_of(&args[0])?;
                let idx = self.b.add_name(&name);
                self.emit(Op::GetVar(idx), 1);
                Ok(())
            }
            2 => {
                let name = self.var_name_of(&args[0])?;
                self.word(&args[1])?;
                // `set` yields the value it assigned.
                self.emit(Op::Dup, 1);
                let idx = self.b.add_name(&name);
                self.emit(Op::SetVar(idx), -1);
                Ok(())
            }
            _ => self.error("wrong # args: should be \"set varName ?newValue?\""),
        }
    }

    fn cmd_puts(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let (newline, value) = match args {
            [v] => (true, v),
            [flag, v] if flag.as_literal() == Some("-nonewline") => (false, v),
            _ => return self.error("wrong # args: should be \"puts ?-nonewline? string\""),
        };
        self.word(value)?;
        if newline {
            self.emit(Op::PrintLn(1), -1);
        } else {
            self.emit(Op::Print(1), -1);
        }
        self.push_empty();
        Ok(())
    }

    /// `expr` joins its arguments with spaces and evaluates the result. A single
    /// braced argument — the form that matters — is compiled straight from its
    /// text with no runtime parse.
    fn cmd_expr(&mut self, args: &[Word]) -> Result<(), CompileError> {
        if args.is_empty() {
            return self.error("wrong # args: should be \"expr arg ?arg ...?\"");
        }
        let mut text = String::new();
        for (i, w) in args.iter().enumerate() {
            let piece = self.literal_of(w, "expression")?;
            if i > 0 {
                text.push(' ');
            }
            text.push_str(piece);
        }
        let parsed = expr::parse(&text).map_err(|e| CompileError {
            msg: e.msg,
            line: self.line,
        })?;
        self.expr(&parsed)?;
        self.emit(Op::Extended(ext::NORM, 0), 0);
        Ok(())
    }

    fn cmd_incr(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let (name, by) = match args {
            [n] => (n, None),
            [n, by] => (n, Some(by)),
            _ => return self.error("wrong # args: should be \"incr varName ?increment?\""),
        };
        let name = self.var_name_of(name)?;
        let idx = self.b.add_name(&name);
        self.emit(Op::GetVar(idx), 1);
        match by {
            Some(w) => self.word(w)?,
            None => {
                self.emit(Op::LoadInt(1), 1);
            }
        }
        self.emit(Op::Add, -1);
        self.emit(Op::Dup, 1);
        self.emit(Op::SetVar(idx), -1);
        Ok(())
    }

    fn cmd_if(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let mut i = 0;
        let mut end_jumps = Vec::new();
        let branch_depth = self.depth;

        loop {
            let Some(cond) = args.get(i) else {
                return self.error("wrong # args: no expression after \"if\" argument");
            };
            self.expr_word(cond)?;
            let jump_false = self.emit(Op::JumpIfFalse(usize::MAX), -1);

            i += 1;
            if args.get(i).and_then(|w| w.as_literal()) == Some("then") {
                i += 1;
            }
            let Some(body) = args.get(i) else {
                return self.error("wrong # args: no script following \"if\" argument");
            };
            self.body(body)?;
            i += 1;

            end_jumps.push(self.emit(Op::Jump(usize::MAX), 0));
            let else_start = self.b.current_pos();
            self.b.patch_jump(jump_false, else_start);
            // Each branch is compiled at the same entry depth.
            self.depth = branch_depth;

            match args.get(i).and_then(|w| w.as_literal()) {
                Some("elseif") => {
                    i += 1;
                    continue;
                }
                Some("else") => {
                    i += 1;
                    let Some(body) = args.get(i) else {
                        return self.error("wrong # args: no script following \"else\" argument");
                    };
                    self.body(body)?;
                    i += 1;
                    break;
                }
                None if i == args.len() => {
                    // No else: the value of a taken-nowhere `if` is empty.
                    self.push_empty();
                    break;
                }
                Some(other) => {
                    return self.error(format!("expected \"elseif\" or \"else\", got \"{other}\""))
                }
                None => return self.error("non-literal clause after \"if\" body"),
            }
        }

        if i != args.len() {
            return self.error("wrong # args: extra arguments after \"if\" script");
        }
        let end = self.b.current_pos();
        for j in end_jumps {
            self.b.patch_jump(j, end);
        }
        Ok(())
    }

    fn cmd_while(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let [cond, body] = args else {
            return self.error("wrong # args: should be \"while test command\"");
        };
        let top = self.b.current_pos();
        self.expr_word(cond)?;
        let exit = self.emit(Op::JumpIfFalse(usize::MAX), -1);

        self.loops.push(LoopCtx {
            depth: self.depth,
            breaks: Vec::new(),
            continues: Vec::new(),
        });
        let script = self.body_script(body)?;
        self.script_effect(&script)?;
        let ctx = self.loops.pop().expect("loop context");

        let backedge = self.emit(Op::Jump(usize::MAX), 0);
        self.b.patch_jump(backedge, top);
        for j in ctx.continues {
            self.b.patch_jump(j, top);
        }
        let end = self.b.current_pos();
        self.b.patch_jump(exit, end);
        for j in ctx.breaks {
            self.b.patch_jump(j, end);
        }
        // A loop's own value is empty.
        self.push_empty();
        Ok(())
    }

    fn cmd_loop_exit(&mut self, args: &[Word], is_break: bool) -> Result<(), CompileError> {
        let word = if is_break { "break" } else { "continue" };
        if !args.is_empty() {
            return self.error(format!("wrong # args: should be \"{word}\""));
        }
        let Some(ctx) = self.loops.last() else {
            return self.error(format!("invoked \"{word}\" outside of a loop"));
        };
        // Discard whatever this iteration pushed before jumping, so the exit
        // point sees the depth it was compiled for.
        let surplus = self.depth.saturating_sub(ctx.depth);
        for _ in 0..surplus {
            self.emit(Op::Pop, -1);
        }
        let jump = self.emit(Op::Jump(usize::MAX), 0);
        let ctx = self.loops.last_mut().expect("loop context");
        if is_break {
            ctx.breaks.push(jump);
        } else {
            ctx.continues.push(jump);
        }
        // The jump leaves; the value keeps the sequencer's arithmetic honest.
        self.push_empty();
        Ok(())
    }

    /// A control-flow body: braced text compiled in place.
    fn body(&mut self, word: &Word) -> Result<(), CompileError> {
        let script = self.body_script(word)?;
        self.script_value(&script)
    }

    fn body_script(&mut self, word: &Word) -> Result<Script, CompileError> {
        let text = self.literal_of(word, "script body")?;
        crate::parser::parse(text).map_err(|e| CompileError {
            msg: e.msg,
            line: self.line,
        })
    }

    /// A word used as a condition: its text is an expression.
    fn expr_word(&mut self, word: &Word) -> Result<(), CompileError> {
        let text = self.literal_of(word, "condition")?.to_string();
        let parsed = expr::parse(&text).map_err(|e| CompileError {
            msg: e.msg,
            line: self.line,
        })?;
        self.expr(&parsed)
    }

    // ── expressions ──────────────────────────────────────────────────────

    fn expr(&mut self, e: &Expr) -> Result<(), CompileError> {
        match e {
            Expr::Int(v) => {
                self.emit(Op::LoadInt(*v), 1);
                Ok(())
            }
            Expr::Float(v) => {
                self.emit(Op::LoadFloat(*v), 1);
                Ok(())
            }
            Expr::Subst(parts) => {
                let word = Word {
                    parts: parts.clone(),
                    ..Word::default()
                };
                self.word(&word)
            }
            Expr::Unary(op, operand) => {
                self.expr(operand)?;
                match op {
                    UnOp::Neg => self.emit(Op::Negate, 0),
                    UnOp::Plus => 0, // identity, but still requires a number
                    UnOp::BitNot => self.emit(Op::BitNot, 0),
                    UnOp::Not => self.emit(Op::LogNot, 0),
                };
                Ok(())
            }
            Expr::Binary(BinOp::And, a, b) => self.short_circuit(a, b, false),
            Expr::Binary(BinOp::Or, a, b) => self.short_circuit(a, b, true),
            Expr::Binary(op, a, b) => {
                self.expr(a)?;
                self.expr(b)?;
                let native = match op {
                    BinOp::Add => Some(Op::Add),
                    BinOp::Sub => Some(Op::Sub),
                    BinOp::Mul => Some(Op::Mul),
                    BinOp::Shl => Some(Op::Shl),
                    BinOp::Shr => Some(Op::Shr),
                    BinOp::Lt => Some(Op::NumLt),
                    BinOp::Gt => Some(Op::NumGt),
                    BinOp::Le => Some(Op::NumLe),
                    BinOp::Ge => Some(Op::NumGe),
                    BinOp::Eq => Some(Op::NumEq),
                    BinOp::Ne => Some(Op::NumNe),
                    BinOp::StrLt => Some(Op::StrLt),
                    BinOp::StrGt => Some(Op::StrGt),
                    BinOp::StrLe => Some(Op::StrLe),
                    BinOp::StrGe => Some(Op::StrGe),
                    BinOp::StrEq => Some(Op::StrEq),
                    BinOp::StrNe => Some(Op::StrNe),
                    BinOp::BitAnd => Some(Op::BitAnd),
                    BinOp::BitOr => Some(Op::BitOr),
                    BinOp::BitXor => Some(Op::BitXor),
                    _ => None,
                };
                match native {
                    Some(op) => {
                        self.emit(op, -1);
                    }
                    None => {
                        let id = match op {
                            BinOp::Div => ext::DIV,
                            BinOp::Mod => ext::MOD,
                            BinOp::Pow => ext::POW,
                            BinOp::In => ext::IN,
                            BinOp::Ni => ext::NI,
                            _ => unreachable!("binary op {op:?} has no lowering"),
                        };
                        self.emit(Op::Extended(id, 2), -1);
                    }
                }
                Ok(())
            }
            Expr::Ternary(cond, then, other) => {
                self.expr(cond)?;
                let to_else = self.emit(Op::JumpIfFalse(usize::MAX), -1);
                let branch_depth = self.depth;
                self.expr(then)?;
                let to_end = self.emit(Op::Jump(usize::MAX), 0);
                let else_start = self.b.current_pos();
                self.b.patch_jump(to_else, else_start);
                self.depth = branch_depth;
                self.expr(other)?;
                let end = self.b.current_pos();
                self.b.patch_jump(to_end, end);
                Ok(())
            }
            Expr::Call(name, _) => {
                self.error(format!("math function \"{name}\" is not supported yet"))
            }
        }
    }

    /// `&&` and `||` evaluate their right operand only when the left does not
    /// decide the result.
    fn short_circuit(&mut self, a: &Expr, b: &Expr, on_true: bool) -> Result<(), CompileError> {
        self.expr(a)?;
        let jump = if on_true {
            self.emit(Op::JumpIfTrueKeep(usize::MAX), 0)
        } else {
            self.emit(Op::JumpIfFalseKeep(usize::MAX), 0)
        };
        self.emit(Op::Pop, -1);
        self.expr(b)?;
        // Normalize both arms to a boolean, as Tcl's logical operators yield
        // 1 or 0 rather than the operand that decided the result.
        let end = self.b.current_pos();
        self.b.patch_jump(jump, end);
        self.emit(Op::Extended(ext::NORM, 1), 0);
        Ok(())
    }
}

/// A literal word's runtime value.
///
/// Tcl values are strings, but a value whose string form is exactly the
/// canonical spelling of an integer or double can be carried as a number
/// without observable difference — and that keeps arithmetic on the VM's fast
/// path. `05` and `1.10` are not canonical, so they stay strings.
fn literal_value(text: &str) -> Value {
    if let Ok(i) = text.parse::<i64>() {
        if i.to_string() == text {
            return Value::Int(i);
        }
    }
    if let Ok(f) = text.parse::<f64>() {
        if crate::runtime::format_double(f) == text {
            return Value::Float(f);
        }
    }
    Value::Str(std::sync::Arc::new(text.to_string()))
}
