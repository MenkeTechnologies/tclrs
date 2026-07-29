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
//!
//! Loops are emitted rotated — entered at the test, closed by a conditional
//! backward branch — because that is the one shape fusevm's tracing JIT installs
//! a trace for. [`Compiler::rotated_loop`] is the single emitter every loop in
//! this crate goes through; `while`, `for`, `foreach` and `dict for` differ only
//! in what they hand it.

use fusevm::{ChunkBuilder, Op, Value};
use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::assoc::{self, ArrayNames, Target};
use crate::expr::{self, BinOp, Expr, UnOp};
use crate::parser::{Command, Part, Script, Word};
use crate::procs::Signature;

/// Extension opcode ids owned by this frontend.
pub mod ext {
    pub const DIV: u16 = 0;
    pub const MOD: u16 = 1;
    pub const POW: u16 = 2;
    pub const IN: u16 = 3;
    pub const NI: u16 = 4;
    /// `[value]` → nothing, having written the value's **Tcl** string form, with
    /// a newline when `arg` is 1.
    ///
    /// `puts` does not lower to fusevm's `Print` / `PrintLn`, because those
    /// stringify with the VM's rules and the frontend owns Tcl's: a double
    /// prints in the shortest form that reads back, and a boolean as `1` or `0`
    /// rather than `1` and the empty string. Owning it here is what lets an
    /// `expr` result stay the number the VM computed instead of being converted
    /// to its string the moment it is produced — which is what kept every
    /// arithmetic loop out of the JIT and the ahead-of-time compiler. Neither
    /// `Print` nor `PrintLn` is JIT-eligible, so nothing is lost by the swap.
    pub const PUTS: u16 = 5;
    /// `eval`: `[arg, …]` with the count in the inline operand → the value of
    /// the script they concatenate to. The only op whose operand is a script
    /// that is not known until it runs; the handler lives in
    /// [`crate::runtime`], which owns the state the script runs against.
    pub const EVAL: u16 = 6;

    // Procedures and control flow (`procs`, `control`).

    /// Pop a pattern and a subject and push 1 or 0. `arg` is 0 for `switch
    /// -exact` and 1 for `switch -glob`.
    pub const MATCH: u16 = 7;
    /// Raise the Tcl error whose message is on top of the stack.
    pub const ERROR: u16 = 8;
    /// Leave the `catch` region entered by [`ext_wide::CATCH`], having reached
    /// its end without an error.
    pub const CATCH_END: u16 = 9;

    // Coroutines (`coro`). Every one but [`CORO_INFO`] parks the VM with a
    // request the driver in [`crate::runtime`] services; see [`crate::coro`].

    /// `[arg …, name, command]` with `arg` actual arguments — create the
    /// coroutine `name` running `command`, and enter it.
    pub const CORO_CREATE: u16 = 10;
    /// `[arg …, name]` with `arg` actual arguments — resume the coroutine.
    pub const CORO_RESUME: u16 = 11;
    /// `[value]` — suspend this coroutine, handing `value` to its resumer.
    pub const CORO_YIELD: u16 = 12;
    /// `[name, arg …]` with `arg` actual arguments — suspend this coroutine and
    /// enter the coroutine `name`, which inherits this one's resumer.
    pub const CORO_YIELDTO: u16 = 13;
    /// `info coroutine`: the running coroutine's qualified name, or `""`.
    pub const CORO_INFO: u16 = 14;

    /// `[name, arg …]` with the count in the inline operand — call the function
    /// an inline `rust { ... }` block exported. Emitted only for a name
    /// [`crate::rust_ffi::is_exported`] answered for while compiling.
    pub const FFI_CALL: u16 = 63;

    /// Pop a value and push Tcl's boolean reading of it — 1 or 0 — or refuse it.
    /// `arg` is 0 for a condition and 1 for `!`, which differ in how they word
    /// the refusal. Emitted only where the value could be a string, so the
    /// arithmetic a condition is usually made of stays native and traceable;
    /// [`super::Compiler::yields_number`] is the test.
    pub const BOOL: u16 = 15;

    /// `[a, b]` → 1 or 0: `expr`'s always-string comparisons — `eq ne lt gt le
    /// ge` — with `arg` naming which, in [`super::Compiler::str_cmp`]'s order.
    ///
    /// fusevm's `StrEq` and friends compare the VM's string form, which is not
    /// Tcl's for a double or a boolean. Same trade as [`PUTS`]: those ops are
    /// not JIT-eligible either, so comparing here costs a frontend op only
    /// where one was already going to stop a trace.
    pub const STR_CMP: u16 = 62;

    /// Where the list commands' ops begin. Everything at or above this id is
    /// dispatched to [`crate::cmd_list`]; the inline operand is the number of
    /// stack values the op consumes.
    pub const LIST_BASE: u16 = 16;
    pub const LIST: u16 = 16;
    pub const LLENGTH: u16 = 17;
    pub const LINDEX: u16 = 18;
    pub const LAPPEND: u16 = 19;
    pub const LRANGE: u16 = 20;
    pub const LREVERSE: u16 = 21;
    pub const LINSERT: u16 = 22;
    pub const LREPLACE: u16 = 23;
    pub const LSEARCH: u16 = 24;
    pub const LSORT: u16 = 25;
    pub const JOIN: u16 = 26;
    pub const SPLIT: u16 = 27;
    pub const CONCAT: u16 = 28;

    /// `[place, value …]` → the extended list, stored in the variable the op
    /// reaches itself: `LAPPEND_VAR` at a name index in the VM's global table,
    /// `LAPPEND_SLOT` at a frame slot. Reaching the variable here rather than
    /// through `GetVar` / `SetVar` is what lets the elements be appended to the
    /// list's own string instead of a copy of it — see [`crate::cmd_list`].
    /// [`LAPPEND`] is still emitted for a name the script also uses as an
    /// array, where the value is not a list to begin with.
    pub const LAPPEND_VAR: u16 = 33;
    pub const LAPPEND_SLOT: u16 = 34;

    /// `foreach`'s four steps. `INIT` builds the loop state from the value
    /// lists, `MORE` asks whether an iteration remains, `TAKE` pushes one
    /// iteration's values, and `ADVANCE` moves to the next.
    pub const FOREACH_INIT: u16 = 29;
    pub const FOREACH_MORE: u16 = 30;
    pub const FOREACH_TAKE: u16 = 31;
    pub const FOREACH_ADVANCE: u16 = 32;

    /// The bitwise operators, in Tcl's semantics rather than the VM's.
    ///
    /// fusevm's `Op::BitAnd`/`BitOr`/`BitXor`/`BitNot`/`Shl`/`Shr` coerce their
    /// operands through `Value::to_int`, which reads `1.5` as 1 and `"abc"` as
    /// 0; Tcl refuses both (`cannot use floating-point value "1.5" as left
    /// operand of "|"`). It also masks a shift distance to 6 bits, where Tcl
    /// saturates a right shift and promotes an overflowing left shift.
    ///
    /// Emitted only where the compiler cannot prove both operands are integers
    /// ([`super::Compiler::yields_integer`]), so an expression written in
    /// literals keeps the native op — and with it the tracing JIT, which
    /// rejects `Op::Extended`.
    ///
    /// The ids sit above [`LIST_BASE`] and so must keep their own arms in
    /// [`crate::runtime::extension`]'s match, which is tested in order and only
    /// falls through to the `id >= LIST_BASE` range once every named op has had
    /// its turn. 33 and 34 are [`LAPPEND_VAR`] and [`LAPPEND_SLOT`]; this block
    /// starts after [`UPLUS`]'s old neighbours to leave those alone.
    pub const BIT_AND: u16 = 40;
    pub const BIT_OR: u16 = 41;
    pub const BIT_XOR: u16 = 42;
    pub const SHL: u16 = 43;
    pub const SHR: u16 = 44;
    pub const BIT_NOT: u16 = 45;

    /// Unary `+`, which is the identity on a *number* and an error on anything
    /// else: `expr {+"a"}` is `cannot use non-numeric string "a" as operand of
    /// "+"` in tclsh 9.0.4, where lowering it to nothing at all answered `a`.
    /// Emitted only where the operand is not already known to be a number
    /// ([`super::Compiler::yields_number`]), so `expr {+1}` still lowers to a
    /// single `LoadInt`.
    pub const UPLUS: u16 = 46;

    /// `[value]` → the value `expr` answers with, for a result the VM's own
    /// value is not: a bare operand becomes the *number* it spells (`expr {007}`
    /// is 7, `expr {" 42 "}` is 42), and a NaN is `expr(n)`'s domain error
    /// rather than a value at all.
    ///
    /// Emitted only where [`super::Compiler::normalizes`] says the two can
    /// differ. An expression whose result is provably an `i64` or a boolean —
    /// which is every counted loop and every comparison — is left alone, and
    /// that is what keeps `Op::Extended` out of a hot loop: the JIT and the
    /// ahead-of-time compiler both refuse a chunk that contains one.
    pub const NORM: u16 = 47;
    // Associative data (`assoc`). The operand order in each comment is the
    // order the compiler pushes them, so the handler pops them in reverse.

    /// Where the associative commands' ops begin — array elements, `array`
    /// and `dict` — dispatched to [`crate::assoc`].
    pub const ASSOC_BASE: u16 = 64;
    /// `[name, value]` → `value`, refusing an array. `arg` 1 assigns instead of
    /// reading and leaves nothing behind.
    pub const SCALAR: u16 = ASSOC_BASE;
    /// `[name, index, slot]` → the element's value.
    pub const ELEM_GET: u16 = ASSOC_BASE + 1;
    /// `[name, index, value, slot]` → `value`, stored.
    pub const ELEM_SET: u16 = ASSOC_BASE + 2;
    /// `[name, index, increment, slot]` → the incremented element.
    pub const ELEM_INCR: u16 = ASSOC_BASE + 3;
    /// `[name, index, slot, complain]`, leaving nothing.
    pub const UNSET_ELEM: u16 = ASSOC_BASE + 4;
    /// `[name, slot, complain]`, leaving nothing.
    pub const UNSET_VAR: u16 = ASSOC_BASE + 5;
    /// `[slot]` → 1 when the variable holds an array.
    pub const ARR_EXISTS: u16 = ASSOC_BASE + 6;
    /// `[slot]` → the element count.
    pub const ARR_SIZE: u16 = ASSOC_BASE + 7;
    /// `[mode, pattern, given, slot]` → the matching element names, as a list.
    pub const ARR_NAMES: u16 = ASSOC_BASE + 8;
    /// `[mode, pattern, given, slot]` → matching name/value pairs, as a list.
    pub const ARR_GET: u16 = ASSOC_BASE + 9;
    /// `[mode, pattern, given, slot]` → `""`, having removed the matches.
    pub const ARR_UNSET: u16 = ASSOC_BASE + 10;
    /// `[name, list, slot]` → `""`, having merged the list into the array.
    pub const ARR_SET: u16 = ASSOC_BASE + 11;
    /// `[k, v, …, count]` → a dict.
    pub const DICT_CREATE: u16 = ASSOC_BASE + 12;
    /// `[dict, key, …, count]` → the value at the key path.
    pub const DICT_GET: u16 = ASSOC_BASE + 13;
    /// `[dict, key, …, count]` → 1 when the key path resolves.
    pub const DICT_EXISTS: u16 = ASSOC_BASE + 14;
    /// `[dict, key, …, count]` → the dict without those keys.
    pub const DICT_REMOVE: u16 = ASSOC_BASE + 15;
    /// `[dict, …, count]` → the dicts combined left to right.
    pub const DICT_MERGE: u16 = ASSOC_BASE + 16;
    /// `[dict, mode, pattern, given]` → the matching keys, as a list.
    pub const DICT_KEYS: u16 = ASSOC_BASE + 17;
    /// `[dict, mode, pattern, given]` → the matching values, as a list.
    pub const DICT_VALUES: u16 = ASSOC_BASE + 18;
    /// `[dict]` → the number of pairs.
    pub const DICT_SIZE: u16 = ASSOC_BASE + 19;
    /// `[name, current, key, …, value, count]` → the updated dict.
    pub const DICT_SET: u16 = ASSOC_BASE + 20;
    /// `[dict]` → a `Value::Array` of alternating keys and values, which
    /// `dict for` walks with the VM's own `ArrayLen` and `ArrayGet`.
    pub const DICT_PAIRS: u16 = ASSOC_BASE + 21;

    /// Where the string commands' ops begin — the `string` ensemble, `append`
    /// and `format` — dispatched to [`crate::cmd_string`], which names them.
    /// The inline operand is the number of stack values the op consumes.
    pub const STRING_BASE: u16 = 128;
}

/// Wide extension opcode ids, whose payload is a `usize` rather than a byte.
pub mod ext_wide {
    /// Enter a `catch` region. The payload is the op index of the region's
    /// error handler, which the driver in [`crate::runtime`] resumes at.
    pub const CATCH: u16 = 0;

    /// A command is about to run, and the payload is its line. Emitted only
    /// when [`super::Compiler::debug`] is set — a chunk compiled the ordinary
    /// way carries none of these, so nothing is paid for a debugger that is not
    /// attached.
    pub const DBG_LINE: u16 = 1;
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
///
/// Two passes. Reading `$x` lowers to a bare `GetVar`, which cannot fail, but
/// reading a variable that holds an array must — and the `set a(i) v` that makes
/// it one may be compiled after the `$a` that reads it. The first pass records
/// every name used as an array; the second, knowing them, guards just those
/// names. Nothing else differs between the passes, so a script with no arrays
/// compiles exactly as it did before and pays nothing.
pub fn compile(script: &Script) -> Result<fusevm::Chunk, CompileError> {
    lower(script, false)
}

/// Lower a script with a line marker before every command, for the debug
/// adapter. The markers are the only difference: a debugger single-steps the
/// same bytecode a run executes, rather than a second lowering written for it.
pub fn compile_debug(script: &Script) -> Result<fusevm::Chunk, CompileError> {
    lower(script, true)
}

fn lower(script: &Script, debug: bool) -> Result<fusevm::Chunk, CompileError> {
    let first = Compiler::run(script, ArrayNames::new(), debug)?;
    let mut chunk = if first.seen_arrays.is_empty() {
        first.b.build()
    } else {
        Compiler::run(script, first.seen_arrays, debug)?.b.build()
    };
    // Tcl's integers are arbitrary-precision, and this frontend has no bignum:
    // an `i64` that overflows is an error, raised by the numeric hook. Native
    // codegen would wrap instead, so ask fusevm for the overflow-checked
    // lowering — `Add`/`Sub`/`Mul` stay native registers on the common path and
    // deopt into the hook when a result does not fit. Without this, the JIT and
    // the AOT compiler print -9223372036854775808 where the interpreter reports
    // "integer value too large to represent".
    chunk.int_overflow_deopt = true;
    Ok(chunk)
}

pub(crate) struct LoopCtx {
    /// Stack depth on entry, so an early exit knows how much to discard.
    pub(crate) depth: usize,
    /// `catch` regions open at the loop header. An exit from a deeper one
    /// would leave the driver's catch record behind, so it is refused.
    pub(crate) catch_depth: usize,
    pub(crate) breaks: Vec<usize>,
    pub(crate) continues: Vec<usize>,
}

/// The local variables of one procedure body.
///
/// A procedure's variables live in the call frame's slots, which fusevm
/// allocates per `Op::Call` — that is what keeps them off the globals and out
/// of a recursive call's way. Names listed by `global` are excluded and reach
/// the VM's global table through `Op::GetVar`/`Op::SetVar` instead.
#[derive(Default)]
pub(crate) struct Scope {
    pub locals: HashMap<String, u16>,
    pub globals: HashSet<String>,
    pub next_slot: u16,
}

/// Where a variable lives once the script is lowered: a frame slot inside a
/// procedure body, a name index in the VM's global table anywhere else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Place {
    Slot(u16),
    Global(u16),
}

pub(crate) struct Compiler {
    pub(crate) b: ChunkBuilder,
    pub(crate) depth: usize,
    pub(crate) loops: Vec<LoopCtx>,
    /// The line of the command being lowered, recorded against every op it
    /// emits so `--disasm` can attribute them. Inside a body this is relative to
    /// the body's own text, because a body is parsed as a script of its own.
    pub(crate) line: usize,
    /// The line of the script's own command that is being lowered — the line a
    /// failure is reported at. See [`Compiler::err`].
    pub(crate) command_line: usize,
    /// Names known to be used as arrays, from the previous pass.
    pub(crate) arrays: ArrayNames,
    /// Names found to be used as arrays during this pass.
    pub(crate) seen_arrays: ArrayNames,
    /// `Some` while compiling a procedure body.
    pub(crate) scope: Option<Scope>,
    /// Signatures of every procedure the script defines, keyed by name. The
    /// call site needs one to apply defaults and collect `args`.
    pub(crate) procs: HashMap<String, Signature>,
    /// Procedures whose body has been compiled, so a redefinition is caught.
    pub(crate) defined: HashSet<String>,
    /// Names the script's own `coroutine` commands create. A call to one of
    /// them resumes the coroutine instead of calling a procedure.
    pub(crate) coros: HashSet<String>,
    /// How many `catch` regions enclose the code being compiled.
    pub(crate) catch_depth: usize,
    /// How many re-parsed bodies enclose the code being compiled. Nonzero means
    /// the commands being lowered are numbered relative to a body's own text
    /// rather than to the script, so they must not move
    /// [`Compiler::command_line`].
    pub(crate) body_depth: usize,
    /// Whether the command being compiled is one of the script's own, rather
    /// than one inside a body or a command substitution.
    pub(crate) top_level: bool,
    /// Whether the command being compiled runs exactly once, at a position
    /// [`crate::coro::prescan`] also reaches: the script's own commands and the
    /// command substitutions inside them. A `coroutine` command may only appear
    /// there, since its name has to be known to every call site.
    pub(crate) static_ctx: bool,
    /// Emit a [`ext_wide::DBG_LINE`] marker before every command, which is what
    /// lets a debugger stop at one. Off for every ordinary compilation.
    pub(crate) debug: bool,
    /// How many command substitutions enclose the command being compiled. A
    /// debugger stops before a statement, and a substitution is part of one.
    pub(crate) subst_depth: usize,
}

impl Compiler {
    /// One compilation pass over the script, with the array names the previous
    /// pass discovered.
    fn run(script: &Script, arrays: ArrayNames, debug: bool) -> Result<Compiler, CompileError> {
        let mut c = Compiler {
            b: ChunkBuilder::new(),
            depth: 0,
            loops: Vec::new(),
            line: 1,
            command_line: 1,
            arrays,
            seen_arrays: ArrayNames::new(),
            scope: None,
            procs: HashMap::new(),
            defined: HashSet::new(),
            coros: HashSet::new(),
            catch_depth: 0,
            body_depth: 0,
            top_level: true,
            static_ctx: true,
            debug,
            subst_depth: 0,
        };
        // Signatures are collected before anything is emitted so a procedure
        // may call one that the script defines further down, which is legal in
        // Tcl as long as the call is not reached first.
        crate::procs::prescan(&mut c.procs, script);
        crate::coro::prescan(&mut c.coros, script);
        c.script_value(script)?;
        Ok(c)
    }

    pub(crate) fn emit(&mut self, op: Op, delta: i32) -> usize {
        let idx = self.b.emit(op, self.line as u32);
        self.depth = (self.depth as i32 + delta) as usize;
        idx
    }

    pub(crate) fn error<T>(&self, msg: impl Into<String>) -> Result<T, CompileError> {
        Err(self.err(msg))
    }

    /// A failure located where the reference interpreter locates one: at the
    /// script's own command, not at the position inside a body that a re-parse
    /// gave its own line numbers.
    ///
    /// A braced body is parsed as a script of its own, so its commands are
    /// numbered from 1 relative to the body's text — which is why an error
    /// inside `if {1} {f}` on line 3 used to be reported at line 1. tclsh's
    /// `(file "…" line N)` names the top-level command that was running
    /// (measured: `while {1} {\n incr\n}` reports `("while" body line 2)` for
    /// the position inside the body and `(file … line 1)` for the file), and
    /// that is the line this reports. [`Compiler::line`] keeps the per-op line
    /// the disassembler shows, so the two are tracked separately.
    pub(crate) fn err(&self, msg: impl Into<String>) -> CompileError {
        CompileError {
            msg: msg.into(),
            line: self.command_line,
        }
    }

    pub(crate) fn push_value(&mut self, v: Value) {
        let idx = self.b.add_constant(v);
        self.emit(Op::LoadConst(idx), 1);
    }

    pub(crate) fn push_empty(&mut self) {
        self.push_value(Value::Str(std::sync::Arc::new(String::new())));
    }

    /// Push a string constant verbatim, without the numeric canonicalisation
    /// [`Compiler::push_text`] applies. Operands the compiler synthesises — an
    /// option name, a variable name an op resolves at run time — go through
    /// here, since they are never numbers.
    pub(crate) fn push_str(&mut self, text: &str) {
        self.push_value(Value::Str(std::sync::Arc::new(text.to_string())));
    }

    /// Push a literal string as a value, canonicalising it the way a literal
    /// word is canonicalised.
    pub(crate) fn push_text(&mut self, text: &str) {
        let v = literal_value(text);
        self.push_value(v);
    }

    // ── variables ────────────────────────────────────────────────────────

    /// The frame slot holding `name`, allocating one if this is its first
    /// mention. `None` outside a procedure body, and for a name that `global`
    /// has bound to the global of the same name.
    fn slot_of(&mut self, name: &str) -> Option<u16> {
        let scope = self.scope.as_mut()?;
        if scope.globals.contains(name) {
            return None;
        }
        if let Some(slot) = scope.locals.get(name) {
            return Some(*slot);
        }
        let slot = scope.next_slot;
        scope.next_slot += 1;
        scope.locals.insert(name.to_string(), slot);
        Some(slot)
    }

    /// Whether `name` would resolve to a frame slot rather than to the VM's
    /// global table — true inside a procedure body for every name `global` has
    /// not bound. Unlike [`Compiler::slot_of`] this allocates nothing, so it is
    /// safe to ask about a name the emitted code may never touch.
    pub(crate) fn is_local(&self, name: &str) -> bool {
        self.scope
            .as_ref()
            .is_some_and(|s| !s.globals.contains(name))
    }

    /// Whether `set name word` only *grows* `name`: the word begins with that
    /// variable and everything after it is text or another variable's value.
    ///
    /// Such an assignment is the same operation `append` is, and lowering it
    /// that way is what keeps a build loop — `set s "$s$i"` — from copying the
    /// whole accumulated string every iteration.
    ///
    /// The parts after the first have to be substitutions that cannot run a
    /// script, because the op reads the variable *after* they are evaluated
    /// while the word reads it before: `set s "$s[set s x]"` would answer
    /// differently. Text and a scalar read cannot change a variable, so those
    /// are the two allowed. A name the script also uses as an array is left
    /// alone as well, so that the guarded read still refuses one.
    fn grows_itself(&self, name: &str, word: &Word) -> bool {
        !word.expand
            && word.parts.len() > 1
            && !self.is_array(name)
            && matches!(&word.parts[0], Part::Var(first) if first == name)
            && word.parts[1..]
                .iter()
                .all(|part| matches!(part, Part::Lit(_) | Part::Var(_)))
    }

    /// Emit an in-place append of `parts` onto `name`, leaving the new value —
    /// the lowering `append` uses, reached from `set` through [`grows_itself`].
    fn append_parts(&mut self, name: &str, parts: &[Part]) -> Result<(), CompileError> {
        let id = self.append_target(name);
        for part in parts {
            self.part(part)?;
        }
        let argc = parts.len() + 2;
        let Ok(argc8) = u8::try_from(argc) else {
            return self.error("too many arguments for one command");
        };
        self.emit(Op::Extended(id, argc8), 1 - argc as i32);
        Ok(())
    }

    /// Where a variable lives, for an op that reaches it itself rather than
    /// through `GetVar` / `SetVar` — [`crate::cmd_list`]'s `lappend` is the one
    /// that does, so that it can extend the list in place.
    pub(crate) fn var_place(&mut self, name: &str) -> Place {
        match self.slot_of(name) {
            Some(slot) => Place::Slot(slot),
            None => Place::Global(self.b.add_name(name)),
        }
    }

    /// Read a variable onto the stack.
    pub(crate) fn emit_get_var(&mut self, name: &str) {
        match self.var_place(name) {
            Place::Slot(slot) => self.emit(Op::GetSlot(slot), 1),
            Place::Global(idx) => self.emit(Op::GetVar(idx), 1),
        };
    }

    /// Pop the top of the stack into a variable.
    pub(crate) fn emit_set_var(&mut self, name: &str) {
        match self.var_place(name) {
            Place::Slot(slot) => self.emit(Op::SetSlot(slot), -1),
            Place::Global(idx) => self.emit(Op::SetVar(idx), -1),
        };
    }

    // ── scripts ──────────────────────────────────────────────────────────

    /// Emit a nested script — a body — for its value. Commands that may only
    /// appear at the script's own top level are refused inside one, and so are
    /// the ones that need a position the prescan reaches: a body may run any
    /// number of times, or not at all.
    pub(crate) fn nested_value(&mut self, script: &Script) -> Result<(), CompileError> {
        self.in_body(|c| c.script_value(script))
    }

    /// Emit a nested script for its effect, leaving the stack as it was found.
    pub(crate) fn nested_effect(&mut self, script: &Script) -> Result<(), CompileError> {
        self.in_body(|c| c.script_effect(script))
    }

    /// Run `emit` with the compiler inside a body: not the script's top level,
    /// not a position the prescan reaches, and numbered relative to the body's
    /// own text.
    fn in_body(
        &mut self,
        emit: impl FnOnce(&mut Self) -> Result<(), CompileError>,
    ) -> Result<(), CompileError> {
        let outer = std::mem::replace(&mut self.top_level, false);
        let outer_static = std::mem::replace(&mut self.static_ctx, false);
        self.body_depth += 1;
        let result = emit(self);
        self.body_depth -= 1;
        self.top_level = outer;
        self.static_ctx = outer_static;
        result
    }

    /// Emit a command substitution for its value. Unlike a body it runs exactly
    /// where it is written, once per evaluation of the command it belongs to,
    /// so a command the prescan needs to see may appear in one.
    fn subst_value(&mut self, script: &Script) -> Result<(), CompileError> {
        let outer = std::mem::replace(&mut self.top_level, false);
        // A command substitution is part of the command containing it, not a
        // command a debugger stops before: `set out [double 21]` is one step,
        // and its nested command carries the same line anyway.
        self.subst_depth += 1;
        let result = self.script_value(script);
        self.subst_depth -= 1;
        self.top_level = outer;
        result
    }

    /// Emit a script that leaves its value on the stack.
    pub(crate) fn script_value(&mut self, script: &Script) -> Result<(), CompileError> {
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
    pub(crate) fn script_effect(&mut self, script: &Script) -> Result<(), CompileError> {
        for cmd in &script.commands {
            self.command(cmd)?;
            self.emit(Op::Pop, -1);
        }
        Ok(())
    }

    // ── words ────────────────────────────────────────────────────────────

    /// Emit a word, leaving its value on the stack.
    pub(crate) fn word(&mut self, word: &Word) -> Result<(), CompileError> {
        if word.expand {
            return self.error("{*} argument expansion is not supported yet");
        }
        match word.parts.len() {
            0 => self.push_empty(),
            1 => self.part(&word.parts[0])?,
            _ => {
                // One op over every part rather than a `Concat` per pair,
                // because the parts have to be joined in Tcl's string form and
                // fusevm's `Concat` joins them in the VM's — see [`ext::PUTS`]
                // for why the difference is now visible. A word with more parts
                // than an operand count can hold is joined in groups, each
                // group's result becoming the next group's first operand.
                let mut pending = 0usize;
                for part in &word.parts {
                    self.part(part)?;
                    pending += 1;
                    if pending == u8::MAX as usize {
                        self.concat_parts(pending)?;
                        pending = 1;
                    }
                }
                if pending > 1 {
                    self.concat_parts(pending)?;
                }
            }
        }
        Ok(())
    }

    /// Join the top `count` values into one, in Tcl's string form.
    fn concat_parts(&mut self, count: usize) -> Result<(), CompileError> {
        let Ok(argc) = u8::try_from(count) else {
            return self.error("too many parts in one word");
        };
        self.emit(
            Op::Extended(crate::cmd_string::ext::CAT, argc),
            1 - count as i32,
        );
        Ok(())
    }

    fn part(&mut self, part: &Part) -> Result<(), CompileError> {
        match part {
            Part::Lit(text) => {
                self.push_value(literal_value(text));
                Ok(())
            }
            Part::Var(name) => {
                self.scalar_get(name);
                Ok(())
            }
            Part::Elem { name, index } => self.elem_get(name, index),
            Part::Script(script) => self.subst_value(script),
        }
    }

    /// The literal text of a word, when the compiler needs it at compile time
    /// (a command name, a variable name, a braced body).
    pub(crate) fn literal_of<'w>(
        &self,
        word: &'w Word,
        what: &str,
    ) -> Result<&'w str, CompileError> {
        word.as_literal()
            .ok_or_else(|| self.err(format!("{what} must be a literal in this phase")))
    }

    /// What a variable-name word names. `a(i)` is an array element even though
    /// the parser hands it over as ordinary text — the parentheses are only
    /// syntax inside a `$` substitution, so the interpretation happens here.
    fn target_of(&self, word: &Word) -> Result<Target, CompileError> {
        assoc::target_of(word)
            .ok_or_else(|| self.err("variable name must be a literal in this phase".to_string()))
    }

    /// The plain name of a scalar variable, for the commands that take only
    /// one. An array element is refused here rather than silently treated as a
    /// variable whose name happens to contain parentheses.
    pub(crate) fn var_name_of(&self, word: &Word) -> Result<String, CompileError> {
        match self.target_of(word)? {
            Target::Scalar(name) => Ok(name),
            Target::Elem { .. } => self.error("this command does not take an array element yet"),
        }
    }

    // ── commands ─────────────────────────────────────────────────────────

    /// The command names [`Compiler::command`] matches before it consults
    /// `procs`. A procedure may not take one of these names: Tcl would let the
    /// definition replace the command, and here the built-in lowering would
    /// keep winning. The list commands are absent on purpose — they are
    /// dispatched after `procs`, so a procedure does replace one.
    pub const BUILTINS: &'static [&'static str] = &[
        "set",
        "eval",
        "puts",
        "expr",
        "incr",
        "if",
        "while",
        "for",
        "foreach",
        "switch",
        "string",
        "append",
        "format",
        "break",
        "continue",
        "proc",
        "return",
        "global",
        "catch",
        "error",
        "array",
        "dict",
        "unset",
        "coroutine",
        "yield",
        "yieldto",
        "info",
    ];

    fn command(&mut self, cmd: &Command) -> Result<(), CompileError> {
        self.line = cmd.line;
        // A command substitution is parsed from the script's own text, so its
        // commands carry absolute lines and may set this; a body is re-parsed
        // and carries lines of its own, so it may not. `top_level` is false in
        // both, which is why the two are told apart by `body_depth`.
        if self.body_depth == 0 {
            self.command_line = cmd.line;
        }
        // Before the command, so a stop reports the line about to run rather
        // than the one that just did. Emitted inside procedure bodies too,
        // which is what makes stepping work below the top level.
        if self.debug && self.subst_depth == 0 {
            self.emit(Op::ExtendedWide(ext_wide::DBG_LINE, cmd.line), 0);
        }
        let Some(first) = cmd.words.first() else {
            self.push_empty();
            return Ok(());
        };
        let name = self.literal_of(first, "command name")?.to_string();
        let args = &cmd.words[1..];

        match name.as_str() {
            "set" => self.cmd_set(args),
            "eval" => self.cmd_eval(args),
            "puts" => self.cmd_puts(args),
            "expr" => self.cmd_expr(args),
            "incr" => self.cmd_incr(args),
            "if" => self.cmd_if(args),
            "while" => self.cmd_while(args),
            "for" => self.cmd_for(args),
            "foreach" => self.cmd_foreach(args),
            "switch" => self.cmd_switch(args),
            "string" | "append" | "format" => self.cmd_string_family(&name, args),
            "break" => self.cmd_loop_exit(args, true),
            "continue" => self.cmd_loop_exit(args, false),
            "proc" => self.cmd_proc(args),
            "return" => self.cmd_return(args),
            "global" => self.cmd_global(args),
            "catch" => self.cmd_catch(args),
            "error" => self.cmd_error(args),
            "array" => self.cmd_array(args),
            "dict" => self.cmd_dict(args),
            "unset" => self.cmd_unset(args),
            "coroutine" => self.cmd_coroutine(args),
            "yield" => self.cmd_yield(args),
            "yieldto" => self.cmd_yieldto(args),
            "info" => self.cmd_info(args),
            // The command an inline `rust { ... }` block was rewritten into.
            name if name == crate::rust_ffi::COMPILE_COMMAND => self.cmd_rust_compile(args),
            // A coroutine's context command. Its name is refused to `proc`, so
            // there is never both a procedure and a coroutine to choose from.
            other if self.coros.contains(other) => self.call_coro(other, args),
            // A procedure the script defines shadows nothing built in: the
            // names above are refused to `proc` at its definition.
            other if self.procs.contains_key(other) => self.call_proc(other, args),
            // A function an inline `rust { ... }` block exported. Asked after
            // the procedures, so a Tcl procedure of the same name still wins —
            // a script's own definition is never shadowed by a library it
            // loaded.
            other if crate::rust_ffi::is_exported(other) => self.call_ffi(other, args),
            // The list commands own the tail of the dispatch, and report the
            // unknown-command error for anything no module claims.
            other => crate::cmd_list::compile(self, other, args),
        }
    }

    fn cmd_set(&mut self, args: &[Word]) -> Result<(), CompileError> {
        match args.len() {
            1 => match self.target_of(&args[0])? {
                Target::Scalar(name) => {
                    self.scalar_get(&name);
                    Ok(())
                }
                Target::Elem { name, index } => self.elem_get(&name, &index),
            },
            2 => match self.target_of(&args[0])? {
                Target::Scalar(name) => {
                    if self.grows_itself(&name, &args[1]) {
                        return self.append_parts(&name, &args[1].parts[1..]);
                    }
                    self.scalar_set_guard(&name);
                    self.word(&args[1])?;
                    // `set` yields the value it assigned.
                    self.emit(Op::Dup, 1);
                    self.emit_set_var(&name);
                    Ok(())
                }
                Target::Elem { name, index } => self.elem_set(&name, &index, &args[1]),
            },
            _ => self.error("wrong # args: should be \"set varName ?newValue?\""),
        }
    }

    /// `eval arg ?arg ...?`.
    ///
    /// Every other command's script is braced text this compiler can lower in
    /// place. `eval`'s is a value, so its arguments are compiled as ordinary
    /// words and the script they produce is compiled when the op runs — once
    /// per distinct text, since [`crate::cache`] keeps what it lowered.
    ///
    /// The nested script is a chunk of its own, and a chunk addresses variables
    /// through the interpreter's global table. A procedure's parameters and
    /// locals are frame slots instead, so a script compiled inside one could
    /// not see them: `eval` in a procedure body is refused rather than run
    /// against the wrong variables.
    fn cmd_eval(&mut self, args: &[Word]) -> Result<(), CompileError> {
        if args.is_empty() {
            return self.error("wrong # args: should be \"eval arg ?arg ...?\"");
        }
        if self.scope.is_some() {
            return self.error(
                "\"eval\" inside a procedure is not supported: the script it builds cannot \
                 reach the procedure's local variables",
            );
        }
        let count = u8::try_from(args.len())
            .map_err(|_| self.err("too many arguments for \"eval\"".to_string()))?;
        for arg in args {
            self.word(arg)?;
        }
        self.emit(Op::Extended(ext::EVAL, count), 1 - args.len() as i32);
        Ok(())
    }

    fn cmd_puts(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let (newline, value) = match args {
            [v] => (true, v),
            [flag, v] if flag.as_literal() == Some("-nonewline") => (false, v),
            _ => return self.error("wrong # args: should be \"puts ?-nonewline? string\""),
        };
        self.word(value)?;
        // The op writes and leaves `puts`'s own empty result, so the stack is
        // one deep either side of it.
        self.emit(Op::Extended(ext::PUTS, u8::from(newline)), 0);
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
        let parsed = expr::parse(&text).map_err(|e| self.err(e.msg))?;
        self.expr(&parsed)?;
        // Almost nothing normalizes the result: it stays the value the VM
        // computed — an integer, a double, a boolean — and Tcl's string form is
        // applied wherever one is asked for (`ext::PUTS`, `ext::STR_CMP`, the
        // word concatenation above, `crate::runtime::tcl_str`). That is what
        // keeps an arithmetic loop free of extension ops, which is what fusevm's
        // JIT and its ahead-of-time compiler need in order to lower one.
        //
        // The exceptions are the two the shapes above cannot spell: a result
        // that can be a NaN, and one that is a bare operand rather than
        // anything an operator computed. [`normalizes`](Self::normalizes) is the
        // test, and it is false for every counting and accumulating loop.
        if Self::normalizes(&parsed) {
            self.emit(Op::Extended(ext::NORM, 0), 0);
        }
        Ok(())
    }

    fn cmd_incr(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let (name, by) = match args {
            [n] => (n, None),
            [n, by] => (n, Some(by)),
            _ => return self.error("wrong # args: should be \"incr varName ?increment?\""),
        };
        // `incr` takes an integer, not an `expr` operand, and says so in its own
        // words. An increment the script wrote out is checked here, where the
        // check is free; see the note on the lowering below for the one it
        // cannot reach.
        if let Some(text) = by.and_then(|w| w.as_literal()) {
            if crate::runtime::tcl_int(&Value::Str(std::sync::Arc::new(text.to_string()))).is_err()
            {
                return self.error(format!(
                    "expected integer but got {}",
                    crate::runtime::named(text, 50)
                ));
            }
        }
        let name = match self.target_of(name)? {
            Target::Scalar(name) => name,
            Target::Elem { name, index } => return self.elem_incr(&name, &index, by),
        };
        self.scalar_get(&name);
        match by {
            Some(w) => self.word(w)?,
            None => {
                self.emit(Op::LoadInt(1), 1);
            }
        }
        // Native `Op::Add`, deliberately: an extension op here would put one
        // inside every loop that counts with `incr`, and fusevm's tracing tier
        // rejects `Op::Extended`, so `bench/counted_loop_proc.tcl` would stop
        // reaching native code. The cost is that a *variable* holding something
        // that is not an integer is refused by the numeric hook in `expr`'s
        // wording rather than `incr`'s — recorded in BUGS.md.
        self.emit(Op::Add, -1);
        self.emit(Op::Dup, 1);
        self.emit_set_var(&name);
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
        let script = self.body_script(body)?;
        self.rotated_loop(
            |c| c.nested_effect(&script),
            |_| Ok(()),
            |c| c.expr_word(cond),
        )?;
        // A loop's own value is empty.
        self.push_empty();
        Ok(())
    }

    /// `foreach varList list ?varList list ...? body`.
    ///
    /// The loop's state — how far it has run and every variable's value for
    /// every iteration — is a single value carried on the stack beneath the
    /// body, so nothing is stashed in a variable the script could see. The
    /// iteration count is fixed before the first pass, as it is in the
    /// reference implementation: the longest list decides it, and shorter ones
    /// supply empty values once they run out.
    fn cmd_foreach(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let Some((body, pairs)) = args.split_last() else {
            return self.error(
                "wrong # args: should be \"foreach varList list ?varList list ...? command\"",
            );
        };
        if pairs.is_empty() || pairs.len() % 2 != 0 {
            return self.error(
                "wrong # args: should be \"foreach varList list ?varList list ...? command\"",
            );
        }

        let mut names = Vec::new();
        for pair in pairs.chunks(2) {
            let text = self
                .literal_of(&pair[0], "foreach variable list")?
                .to_string();
            let vars = crate::list::split(&text).map_err(|msg| CompileError {
                msg,
                line: self.line,
            })?;
            if vars.is_empty() {
                return self.error("foreach varlist is empty");
            }
            let count = vars.len();
            for name in vars {
                if name.ends_with(')') && name.contains('(') {
                    return self.error("array variables are not supported yet");
                }
                names.push(name);
            }
            self.push_value(Value::Int(count as i64));
            self.word(&pair[1])?;
        }
        let lists = u8::try_from(pairs.len() / 2)
            .map_err(|_| self.err("too many lists for \"foreach\"".to_string()))?;
        let width = u8::try_from(names.len())
            .map_err(|_| self.err("too many variables for \"foreach\"".to_string()))?;
        self.emit(
            Op::Extended(ext::FOREACH_INIT, lists),
            1 - pairs.len() as i32,
        );

        // `MORE` and `TAKE` read the state where it lies instead of consuming
        // it, so there is no `Dup` here and no copy of the state per iteration.
        let script = self.body_script(body)?;
        let taken: Vec<String> = names.iter().rev().cloned().collect();
        self.rotated_loop(
            |c| {
                c.emit(Op::Extended(ext::FOREACH_TAKE, width), i32::from(width));
                for name in &taken {
                    c.emit_set_var(name);
                }
                c.nested_effect(&script)
            },
            |c| {
                c.emit(Op::Extended(ext::FOREACH_ADVANCE, 0), 0);
                Ok(())
            },
            |c| {
                c.emit(Op::Extended(ext::FOREACH_MORE, 0), 1);
                Ok(())
            },
        )?;
        self.emit(Op::Pop, -1);
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
        if ctx.catch_depth != self.catch_depth {
            // Tcl turns such an exit into the return code the enclosing
            // `catch` reports rather than letting it reach the loop, which
            // this frontend does not model.
            return self.error(format!(
                "\"{word}\" out of a \"catch\" script is not supported"
            ));
        }
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

    /// Emit a loop in the rotated — do-while — shape, which is the one shape
    /// fusevm's tracing JIT installs a trace for.
    ///
    /// ```text
    ///     Jump -> cond          ; enter at the test, so it still runs first
    ///   body:
    ///     <body>
    ///   step:
    ///     <step>
    ///   cond:
    ///     <cond>
    ///     JumpIfTrue -> body    ; conditional BACKWARD branch
    ///   end:
    /// ```
    ///
    /// fusevm's trace recorder arms at a backward branch and closes the
    /// recording when a branch lands back on the anchor. A `while`-shaped loop
    /// — a forward `JumpIfFalse` exit closed by an unconditional backward
    /// `Jump` — records an eligible op sequence that its trace compiler then
    /// declines, so the trace is aborted and nothing is ever installed. The
    /// rotated shape compiles. That is a fusevm property, reproducible against
    /// the same bytecode with no Tcl involved.
    ///
    /// `body` and `step` must leave the stack as they found it; `cond` must
    /// leave exactly one value, which the closing branch consumes. Because the
    /// next test is at the bottom, `continue` jumps to `step` and `break` to
    /// `end` — the loop's entry depth at both.
    pub(crate) fn rotated_loop<B, S, C>(
        &mut self,
        body: B,
        step: S,
        cond: C,
    ) -> Result<(), CompileError>
    where
        B: FnOnce(&mut Self) -> Result<(), CompileError>,
        S: FnOnce(&mut Self) -> Result<(), CompileError>,
        C: FnOnce(&mut Self) -> Result<(), CompileError>,
    {
        let entry = self.depth;
        let enter = self.emit(Op::Jump(usize::MAX), 0);
        let top = self.b.current_pos();

        self.loops.push(LoopCtx {
            depth: entry,
            catch_depth: self.catch_depth,
            breaks: Vec::new(),
            continues: Vec::new(),
        });
        // The step is compiled with the loop still open: `for(n)` gives a
        // `break` there the same meaning it has in the body.
        let emitted = body(self).and_then(|()| {
            let at = self.b.current_pos();
            step(self).map(|()| at)
        });
        let ctx = self.loops.pop().expect("loop context");
        let step_at = emitted?;

        let cond_at = self.b.current_pos();
        self.b.patch_jump(enter, cond_at);
        for j in ctx.continues {
            self.b.patch_jump(j, step_at);
        }
        // The body and the step are balanced, so the test is compiled at the
        // same depth the entry jump reached it with.
        debug_assert_eq!(self.depth, entry, "rotated loop body is unbalanced");
        cond(self)?;
        self.emit(Op::JumpIfTrue(top), -1);

        let end = self.b.current_pos();
        for j in ctx.breaks {
            self.b.patch_jump(j, end);
        }
        Ok(())
    }

    /// A control-flow body: braced text compiled in place.
    pub(crate) fn body(&mut self, word: &Word) -> Result<(), CompileError> {
        let script = self.body_script(word)?;
        self.nested_value(&script)
    }

    pub(crate) fn body_script(&mut self, word: &Word) -> Result<Script, CompileError> {
        let text = self.literal_of(word, "script body")?;
        crate::parser::parse(text).map_err(|e| self.err(e.msg))
    }

    /// A word used as a condition: its text is an expression, and its value has
    /// to be a Tcl boolean.
    pub(crate) fn expr_word(&mut self, word: &Word) -> Result<(), CompileError> {
        let text = self.literal_of(word, "condition")?.to_string();
        let parsed = expr::parse(&text).map_err(|e| self.err(e.msg))?;
        self.condition(&parsed)
    }

    /// Emit an expression whose value a branch will consume, as Tcl's rule for a
    /// condition rather than as the VM's truthiness: `if {"b"}` is
    /// `expected boolean value but got "b"`, not a taken branch.
    ///
    /// The conversion is an extension op, and an extension op inside a loop body
    /// makes the body ineligible for fusevm's tracing tier
    /// (`is_trace_op_allowed_at` rejects `Op::Extended`), so it is emitted only
    /// where it can change the answer — where the expression's value could be a
    /// string. An arithmetic or relational condition, which is what a counted
    /// loop's test is, already produces a number and keeps the loop traceable.
    pub(crate) fn condition(&mut self, e: &Expr) -> Result<(), CompileError> {
        self.expr(e)?;
        if !Self::yields_number(e) {
            self.emit(Op::Extended(ext::BOOL, 0), 0);
        }
        Ok(())
    }

    /// Whether this expression's value is necessarily a number, whatever the
    /// variables in it hold — which is what decides whether a condition needs
    /// [`ext::BOOL`] at all.
    ///
    /// Every operator lowers to an op that answers with an `Int`, a `Float` or a
    /// `Bool`; the exceptions are an operand that is substituted text
    /// ([`Expr::Subst`]) and unary `+`, which is the identity and so passes its
    /// operand's value straight through.
    /// An operand of an always-string operator, pushed as a string. A numeric
    /// literal becomes the text the script wrote rather than the number it
    /// parses to, which is the difference between `expr {1e3 eq 1000.0}` being
    /// false — tclsh's answer — and true.
    fn string_operand(&mut self, e: &Expr) -> Result<(), CompileError> {
        match e {
            Expr::Int(_, text) | Expr::Float(_, text) => {
                self.push_str(text);
                Ok(())
            }
            other => self.expr(other),
        }
    }

    /// Which string comparison an operator is, as [`ext::STR_CMP`]'s operand.
    /// The order is this function; the handler in [`crate::runtime`] reads it.
    fn str_cmp(op: &BinOp) -> Option<u8> {
        match op {
            BinOp::StrLt => Some(0),
            BinOp::StrGt => Some(1),
            BinOp::StrLe => Some(2),
            BinOp::StrGe => Some(3),
            BinOp::StrEq => Some(4),
            BinOp::StrNe => Some(5),
            _ => None,
        }
    }

    fn yields_number(e: &Expr) -> bool {
        match e {
            Expr::Int(_, _) | Expr::Float(_, _) => true,
            Expr::Subst(_) => false,
            Expr::Unary(UnOp::Plus, operand) => Self::yields_number(operand),
            Expr::Unary(_, _) => true,
            Expr::Binary(_, _, _) => true,
            // Either arm may be the value, so both have to answer with a number.
            Expr::Ternary(_, then, other) => {
                Self::yields_number(then) && Self::yields_number(other)
            }
            // Refused when lowered; the answer here does not matter.
            Expr::Call(_, _) => true,
        }
    }

    /// Whether this expression can only ever produce an `i64`, whatever the
    /// variables in it hold — which is what lets a bitwise operator stay a
    /// native VM op instead of the extension op that carries Tcl's operand
    /// rule ([`ext::BIT_AND`]).
    ///
    /// Deliberately conservative: `false` costs a native op, `true` costs
    /// correctness, so anything whose value is substituted text — and any
    /// double literal, and any operator that can widen to one — answers `false`.
    /// A decimal literal too large for an `i64` is already an [`Expr::Subst`]
    /// by the time it reaches here, so it answers `false` too.
    fn yields_integer(e: &Expr) -> bool {
        match e {
            Expr::Int(_, _) => true,
            Expr::Float(_, _) | Expr::Subst(_) => false,
            // `!` and the comparisons answer 1 or 0 whatever they are given.
            Expr::Unary(UnOp::Not, _) => true,
            // `~` answers an `i64` whatever it is given: the native op coerces
            // and the extension op refuses, and neither can produce a double.
            Expr::Unary(UnOp::BitNot, _) => true,
            Expr::Unary(_, operand) => Self::yields_integer(operand),
            Expr::Binary(
                BinOp::Lt
                | BinOp::Gt
                | BinOp::Le
                | BinOp::Ge
                | BinOp::Eq
                | BinOp::Ne
                | BinOp::StrLt
                | BinOp::StrGt
                | BinOp::StrLe
                | BinOp::StrGe
                | BinOp::StrEq
                | BinOp::StrNe
                | BinOp::In
                | BinOp::Ni
                | BinOp::And
                | BinOp::Or
                // The integer-only operators, for the same reason `~` is above:
                // `%` and the bitwise operators are refusals or `i64`s, never
                // doubles, whatever their operands turn out to be. Saying so
                // here is what lets `expr {($x >> 3) & 1}` keep the native
                // `Op::BitAnd` — the shift has already made an integer of it.
                | BinOp::Mod
                | BinOp::Shl
                | BinOp::Shr
                | BinOp::BitAnd
                | BinOp::BitOr
                | BinOp::BitXor,
                _,
                _,
            ) => true,
            Expr::Binary(_, a, b) => Self::yields_integer(a) && Self::yields_integer(b),
            Expr::Ternary(_, then, other) => {
                Self::yields_integer(then) && Self::yields_integer(other)
            }
            // Refused when lowered; the answer here does not matter.
            Expr::Call(_, _) => false,
        }
    }

    /// The `i64` this expression is worth, when the whole of it is integer
    /// literals and the operators that keep an `i64` an `i64` exactly.
    ///
    /// Read only to decide whether a shift can stay a native VM op: fusevm's
    /// `Op::Shl` and `Op::Shr` mask the distance to six bits and wrap on
    /// overflow, so they are the right lowering exactly when the compiler can
    /// see that neither happens. `None` for anything with a variable, a double
    /// or an overflow in it, which is the answer that keeps `ext::SHL`.
    ///
    /// The arithmetic is the VM's own — `checked_*`, so an overflow is `None`
    /// rather than a wrapped value — which is what makes the answer a proof
    /// about the ops that will actually run rather than a second opinion.
    fn const_int(e: &Expr) -> Option<i64> {
        match e {
            Expr::Int(v, _) => Some(*v),
            Expr::Unary(UnOp::Neg, operand) => Self::const_int(operand)?.checked_neg(),
            Expr::Unary(UnOp::BitNot, operand) => Some(!Self::const_int(operand)?),
            Expr::Binary(op, a, b) => {
                let (x, y) = (Self::const_int(a)?, Self::const_int(b)?);
                match op {
                    BinOp::Add => x.checked_add(y),
                    BinOp::Sub => x.checked_sub(y),
                    BinOp::Mul => x.checked_mul(y),
                    BinOp::BitAnd => Some(x & y),
                    BinOp::BitOr => Some(x | y),
                    BinOp::BitXor => Some(x ^ y),
                    BinOp::Shl | BinOp::Shr => Self::const_shift(*op, x, y),
                    // `/ % **` floor and saturate in ways `crate::runtime::arith`
                    // owns; nothing here needs them, so they are not restated.
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// A shift of known operands, `None` unless the VM's native op would answer
    /// exactly what `crate::runtime::shift` does: a distance in range, and for
    /// a left shift no bit pushed out of the word.
    fn const_shift(op: BinOp, value: i64, by: i64) -> Option<i64> {
        if !(0..64).contains(&by) {
            return None;
        }
        if op == BinOp::Shr {
            // The native op and `runtime::shift` agree over the whole range: an
            // `i64` shifted right by 63 is its sign either way.
            return Some(value >> by);
        }
        value
            .checked_shl(by as u32)
            .filter(|shifted| shifted >> by == value)
    }

    /// Whether a shift may stay the VM's own op. Both operands have to be
    /// known — [`const_int`](Self::const_int) — except for a right shift,
    /// where a distance inside the word is enough because `>>` cannot overflow
    /// and the native op's own answer is the saturating one.
    fn native_shift(op: &BinOp, a: &Expr, b: &Expr) -> bool {
        let Some(by) = Self::const_int(b).filter(|by| (0..64).contains(by)) else {
            return false;
        };
        match op {
            BinOp::Shr => Self::yields_integer(a),
            _ => Self::const_int(a).is_some_and(|v| Self::const_shift(*op, v, by).is_some()),
        }
    }

    /// Whether this expression's value can be a NaN — which `expr(n)` reports
    /// (`domain error: argument not in valid range`) rather than answers, and
    /// which fusevm's arithmetic produces silently.
    ///
    /// Rests on the invariant that no NaN ever leaves an `expr`: [`normalizes`]
    /// puts [`ext::NORM`] on every expression this answers `true` for, and that
    /// op raises. So no *variable* can hold one, which is why an
    /// [`Expr::Subst`] answers `false` — a substituted `nan` is a
    /// `Value::Str`, and reaching an operator with one is the operand refusal
    /// `runtime::num_operand` reports, not a NaN result.
    ///
    /// [`normalizes`]: Self::normalizes
    fn may_be_nan(e: &Expr) -> bool {
        match e {
            Expr::Int(_, _) | Expr::Subst(_) => false,
            Expr::Float(f, _) => f.is_nan(),
            // `!` and `~` answer 1 or 0 or an `i64`; `-` and `+` pass a NaN
            // through but cannot make one.
            Expr::Unary(UnOp::Not | UnOp::BitNot, _) => false,
            Expr::Unary(_, operand) => Self::may_be_nan(operand),
            // `+` and `-` on two numbers are NaN only for `inf - inf`, so one
            // operand that cannot be an infinity is enough to rule it out.
            Expr::Binary(BinOp::Add | BinOp::Sub, a, b) => {
                Self::may_be_nan(a)
                    || Self::may_be_nan(b)
                    || !(Self::yields_finite(a) || Self::yields_finite(b))
            }
            // `*` needs both finite, because `0 * inf` is a NaN. Two finite
            // operands are enough: a product of two finite doubles is an
            // infinity at worst.
            Expr::Binary(BinOp::Mul, a, b) => !(Self::yields_finite(a) && Self::yields_finite(b)),
            // `/` and `**` are NaN on perfectly finite operands — `0.0/0.0` and
            // `(-8) ** 0.5` are both the domain error — so finite is not enough
            // and the test is integral. `arith` computes those two entirely in
            // `i64`, where the awkward cases are refusals (`1/0` is "divide by
            // zero") rather than NaNs.
            Expr::Binary(BinOp::Div | BinOp::Pow, a, b) => {
                !(Self::yields_integer(a) && Self::yields_integer(b))
            }
            // Everything else is a comparison, a boolean, or integer-only.
            Expr::Binary(_, _, _) => false,
            Expr::Ternary(_, then, other) => Self::may_be_nan(then) || Self::may_be_nan(other),
            // Refused when lowered; the answer here does not matter.
            Expr::Call(_, _) => false,
        }
    }

    /// Whether this expression's value is provably a finite number — an `i64`,
    /// or a double literal that is neither an infinity nor a NaN. Deliberately
    /// shallow: it is read only by [`may_be_nan`](Self::may_be_nan), where
    /// `false` costs an extension op and `true` costs correctness.
    fn yields_finite(e: &Expr) -> bool {
        match e {
            Expr::Float(f, _) => f.is_finite(),
            other => Self::yields_integer(other),
        }
    }

    /// Whether this expression needs [`ext::NORM`] — whether the value the VM
    /// computes and the value `expr` answers with can differ.
    ///
    /// Two ways they can. A NaN is the domain error rather than a value at all
    /// ([`may_be_nan`](Self::may_be_nan)). And an expression that is a bare
    /// *operand* answers with the number that operand spells rather than the
    /// text of it — `expr {" 42 "}` is 42, `set x 007; expr {$x}` is 7 — where
    /// every operator already yields a number of its own.
    ///
    /// Everything else skips the op, and that is what keeps a counting loop
    /// lowerable: `expr {$i + 1}` cannot be a NaN, because the literal `1` is
    /// finite, and it cannot pass an operand through, because `+` computed it.
    fn normalizes(e: &Expr) -> bool {
        Self::may_be_nan(e) || Self::passes_operand_through(e)
    }

    /// Whether this expression can answer with an operand's own value rather
    /// than with a number an operator computed.
    fn passes_operand_through(e: &Expr) -> bool {
        match e {
            Expr::Subst(_) => true,
            // Unary `+` is the identity where the operand is already a number,
            // and `ext::UPLUS` — which canonicalizes — where it is not.
            Expr::Unary(UnOp::Plus, operand) => {
                Self::yields_number(operand) && Self::passes_operand_through(operand)
            }
            Expr::Ternary(_, then, other) => {
                Self::passes_operand_through(then) || Self::passes_operand_through(other)
            }
            _ => false,
        }
    }

    // ── expressions ──────────────────────────────────────────────────────

    fn expr(&mut self, e: &Expr) -> Result<(), CompileError> {
        match e {
            Expr::Int(v, _) => {
                self.emit(Op::LoadInt(*v), 1);
                Ok(())
            }
            Expr::Float(v, _) => {
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
            // `!` wants a number or a boolean word, so a numeric operand is
            // `Op::LogNot` — whose truthiness agrees with Tcl's on every number
            // — and anything that could be a string goes through `ext::BOOL`.
            Expr::Unary(UnOp::Not, operand) if !Self::yields_number(operand) => {
                self.expr(operand)?;
                self.emit(Op::Extended(ext::BOOL, 1), 0);
                Ok(())
            }
            // `~` wants an integer, and fusevm's `Op::BitNot` takes anything —
            // `expr {~1.5}` answered -2 where tclsh refuses the operand. Native
            // only when the operand is provably an integer.
            Expr::Unary(UnOp::BitNot, operand) if !Self::yields_integer(operand) => {
                self.expr(operand)?;
                self.emit(Op::Extended(ext::BIT_NOT, 1), 0);
                Ok(())
            }
            Expr::Unary(UnOp::Plus, operand) if !Self::yields_number(operand) => {
                self.expr(operand)?;
                self.emit(Op::Extended(ext::UPLUS, 1), 0);
                Ok(())
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
            // `eq ne lt gt le ge` compare strings, always — that is why they sit
            // beside `==` and `<`. Both operands are lowered as strings, which
            // for a numeric literal means the text the script wrote: `010` and
            // `10` are one number and two strings, and tclsh answers on the
            // strings.
            Expr::Binary(op, a, b) if Self::str_cmp(op).is_some() => {
                let which = Self::str_cmp(op).expect("guarded above");
                self.string_operand(a)?;
                self.string_operand(b)?;
                self.emit(Op::Extended(ext::STR_CMP, which), -1);
                Ok(())
            }
            Expr::Binary(op, a, b) => {
                // The bitwise operators are the VM's only when both operands are
                // provably integers; otherwise they are extension ops that hold
                // Tcl's operand rule. See [`ext::BIT_AND`].
                let integral = Self::yields_integer(a) && Self::yields_integer(b);
                self.expr(a)?;
                self.expr(b)?;
                let native = match op {
                    BinOp::Add => Some(Op::Add),
                    BinOp::Sub => Some(Op::Sub),
                    BinOp::Mul => Some(Op::Mul),
                    // A shift is the VM's op only where the compiler can see
                    // the distance and — for `<<` — the value: fusevm masks the
                    // distance to six bits, so `1 << 64` answered 0 where tclsh
                    // promotes, and `1 << -1` answered 0 where tclsh reports
                    // "negative shift argument". Neither is knowable from an
                    // operand's *shape*, so `yields_integer` is not the test
                    // here; [`native_shift`](Self::native_shift) is, and it asks
                    // for the values.
                    BinOp::Shl | BinOp::Shr if Self::native_shift(op, a, b) => Some(match op {
                        BinOp::Shl => Op::Shl,
                        _ => Op::Shr,
                    }),
                    BinOp::BitAnd if integral => Some(Op::BitAnd),
                    BinOp::BitOr if integral => Some(Op::BitOr),
                    BinOp::BitXor if integral => Some(Op::BitXor),
                    BinOp::Lt => Some(Op::NumLt),
                    BinOp::Gt => Some(Op::NumGt),
                    BinOp::Le => Some(Op::NumLe),
                    BinOp::Ge => Some(Op::NumGe),
                    BinOp::Eq => Some(Op::NumEq),
                    BinOp::Ne => Some(Op::NumNe),
                    // No `StrLt` and friends: the always-string comparisons are
                    // taken by the `str_cmp` arm above, which compares Tcl's
                    // string form rather than the VM's. No unguarded `BitAnd`
                    // either — the `integral` arms above are the only place a
                    // bitwise operator is the VM's own op.
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
                            BinOp::BitAnd => ext::BIT_AND,
                            BinOp::BitOr => ext::BIT_OR,
                            BinOp::BitXor => ext::BIT_XOR,
                            BinOp::Shl => ext::SHL,
                            BinOp::Shr => ext::SHR,
                            _ => unreachable!("binary op {op:?} has no lowering"),
                        };
                        self.emit(Op::Extended(id, 2), -1);
                    }
                }
                Ok(())
            }
            Expr::Ternary(cond, then, other) => {
                self.condition(cond)?;
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
    ///
    /// Both operands are conditions, so both are held to Tcl's boolean rule —
    /// and only the one that is evaluated is: `expr {0 && "b"}` is 0 in tclsh,
    /// not an error, because the left operand already decided it.
    fn short_circuit(&mut self, a: &Expr, b: &Expr, on_true: bool) -> Result<(), CompileError> {
        self.condition(a)?;
        let jump = if on_true {
            self.emit(Op::JumpIfTrueKeep(usize::MAX), 0)
        } else {
            self.emit(Op::JumpIfFalseKeep(usize::MAX), 0)
        };
        self.emit(Op::Pop, -1);
        self.condition(b)?;
        // Normalize both arms to a boolean, as Tcl's logical operators yield
        // 1 or 0 rather than the operand that decided the result. Two `LogNot`s
        // rather than an extension op: both are native, so a short-circuit
        // inside a loop does not cost that loop its trace.
        let end = self.b.current_pos();
        self.b.patch_jump(jump, end);
        self.emit(Op::LogNot, 0);
        self.emit(Op::LogNot, 0);
        Ok(())
    }
}

/// A literal word's runtime value.
///
/// Tcl's first rule is that a value's string representation is what the script
/// wrote, so a literal is a string unless carrying it as a number cannot be
/// observed. That holds for an integer — `i64::to_string` is exactly the
/// spelling Tcl prints, and `05` fails the round-trip and stays a string — and
/// it does **not** hold for a double, whose spelling Tcl keeps: `puts 007.0`
/// prints `007.0`, and a `Value::Float` would print the shortest form that reads
/// back — `7.0` — because that is what Tcl's formatter answers for the number.
/// So no literal is interned as a `Float`; the text is the value.
///
/// A double literal inside an `expr` still becomes `Op::LoadFloat`
/// ([`Compiler::expr`]), which is the arithmetic fast path this used to be
/// about; what it costs here is one parse of a literal double at run time.
pub(crate) fn literal_value(text: &str) -> Value {
    if let Ok(i) = text.parse::<i64>() {
        if i.to_string() == text {
            return Value::Int(i);
        }
    }
    Value::Str(std::sync::Arc::new(text.to_string()))
}
