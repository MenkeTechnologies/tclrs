//! Lowering: `parser::Script` → `fusevm::Chunk`.
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
//! a trace for. `Compiler::rotated_loop` is the single emitter every loop in
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
    /// `[message, extra …]` with the number of `extra` words in the inline
    /// operand — raise `message` as a Tcl error.
    ///
    /// The extras are `error`'s `errorInfo` and `errorCode` arguments. They are
    /// evaluated, because `Tcl_ErrorObjCmd` receives them substituted and a
    /// command substitution in one has already run by then, and then dropped:
    /// what they set is `-errorinfo` and `-errorcode`, the two return options
    /// this frontend does not carry (BUGS.md). Dropping them is visible rather
    /// than silent — asking the options dictionary for either key fails — where
    /// refusing the whole command made `catch {error boom info code} m` leave
    /// the *arity message* in `m` instead of `boom`.
    pub const ERROR: u16 = 8;
    /// Leave the `catch` region entered by `ext_wide::CATCH`, having reached
    /// its end without an error.
    pub const CATCH_END: u16 = 9;
    /// `[break_ip, continue_ip]` — open a loop region, which absorbs a `break`
    /// or a `continue` that arrives as a *raised return code* rather than as
    /// the direct jump the compiler emits for one written in the loop's own
    /// body: from a nested `eval`, from a procedure that returned `-code
    /// break`, or from an `uplevel`. See `Compiler::rotated_loop`.
    pub const LOOP_ENTER: u16 = 35;
    /// Leave the loop region opened by [`LOOP_ENTER`].
    pub const LOOP_LEAVE: u16 = 36;
    /// `[message, code, level]` — leave the running command with a Tcl return
    /// code. `break` and `continue` outside any loop the chunk can see raise
    /// one, and so does every `return` whose code is not `ok`.
    pub const RAISE: u16 = 37;
    /// `[type, message]` — `throw`. Raises `message` as an error once `type`
    /// has been checked to be a list of at least one element, which is the
    /// whole of `Tcl_ThrowObjCmd` (`generic/tclCmdMZ.c:3959-4002`) this
    /// frontend can carry: the `-errorcode` the command's type word becomes is
    /// part of the options dictionary, and that dictionary's error entries are
    /// the gap BUGS.md records for `return -errorcode`.
    ///
    /// 38 rather than a new block: it needs nothing but the stack, and 38 and
    /// 39 are the two ids the core space still had free.
    pub const THROW: u16 = 38;
    /// `[code, options, message]` — raise again exactly what a `catch` region's
    /// handler was resumed with. The three values are the ones
    /// [`crate::runtime::Interp::raise`] pushes, in that order, and the code and
    /// the level are read back out of `options` rather than off the stack: the
    /// integer pushed there is [`crate::runtime::TclError::visible_code`], which
    /// is `TCL_RETURN` while a `return`'s levels are unspent, and re-raising
    /// *that* would spend one level too few.
    ///
    /// This is what makes a handler a `finally` rather than a `catch`: the
    /// region absorbs every code so the cleanup runs, and then hands the code
    /// back on unchanged. `dict update` and `dict with` are built out of it —
    /// `FinalizeDictUpdate` (`generic/tclDictObj.c:3545`) and
    /// `FinalizeDictWith` (`:3696`) are NRE callbacks that run whatever the body
    /// did and then `Tcl_RestoreInterpState` the saved result.
    pub const RERAISE: u16 = 39;

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

    // ── the event loop and the scope commands ────────────────────────────
    //
    // `crate::cmd_after`, `crate::cmd_info` and `crate::cmd_scope`, in the
    // [`EVENT_BASE`] block. They were numbered into the two gaps below
    // [`ASSOC_BASE`] — 35–39 and 58–60 — which is where `namespace`, `source`
    // and `package` were each independently numbered too. Each has an explicit
    // arm ahead of the range tests in `crate::runtime`, so the block only has
    // to be disjoint, and being a block is what makes that checkable.

    /// `after`: `[arg …]` with the count in the inline operand → the handle, or
    /// the empty string. Everything the command decides — whether the first
    /// word is a delay or a subcommand, what the scripts concatenate to — is
    /// decided here, as the reference implementation decides it.
    pub const AFTER: u16 = EVENT_BASE;
    /// `update ?idletasks?`: `[arg …]` with the count in the inline operand.
    pub const UPDATE: u16 = EVENT_BASE + 1;
    /// `vwait ?varName?`: `[arg …]` with the count in the inline operand.
    pub const VWAIT: u16 = EVENT_BASE + 2;
    /// `uplevel ?level? arg …`: `[declared, arg …]` with the count in the inline
    /// operand → the value of the script, run against the frame the first word
    /// resolves to.
    ///
    /// Nothing about the level is settled while compiling. `uplevel $n {…}` is
    /// ordinary Tcl and tclsh reads the level off the *substituted* word
    /// (`Tcl_UplevelObjCmd` → `TclObjGetFrame`), so the word rides on the stack
    /// in Tcl's own spelling and the handler decides whether it is a level at
    /// all. Deciding it here instead answered `invalid command name "1"` where
    /// tclsh answers the script's value (measured against tclsh 9.0.4).
    ///
    /// `declared` is the list of names the enclosing body gave to `global`, for
    /// the reason [`EVAL_FRAME`] states.
    pub const UPLEVEL: u16 = EVENT_BASE + 3;
    // `info exists`, `info commands`/`procs`/`globals`/`locals`/`vars`,
    // `info level` and `info complete` had ids at `EVENT_BASE + 4` through `+ 7`
    // while `info` was lowered from this block. The whole ensemble is
    // [`crate::cmd_info`]'s now and its ids are that module's, in
    // [`INFO_BASE`]'s block — so `EVENT_BASE + 4` through `+ 7` are free, and
    // are left free rather than reused, because a chunk cached on disk carries
    // the id and not the name.
    /// `upvar ?level? otherVar localVar …`: `[(slot, local) …, level, other …]`
    /// with the number of stack values in the inline operand → `""`, having
    /// stored a [`crate::cmd_scope::Link`] descriptor in each local's frame slot.
    /// The level rides as the empty string when the command gave none, which is
    /// the `hasLevel` flag `Tcl_UpvarObjCmd` carries; a slot of `-1` means there
    /// is no frame to hold a descriptor and the pair is an alias in the
    /// interpreter's variable table instead.
    ///
    /// The one `upvar` whose target the script computes. `upvar #0 other local`
    /// written out is bound while the script is read and emits nothing inside a
    /// procedure body; outside one it emits this as well, because the pair has to
    /// outlive the chunk that made it.
    pub const UPVAR: u16 = EVENT_BASE + 8;
    /// `[name, slot]` → what the link in that frame slot points at, or the
    /// `no such variable` refusal for the name it was bound under. `arg` is 1
    /// for a read that tolerates an unset target, which is what `incr` needs.
    pub const LINK_GET: u16 = EVENT_BASE + 9;
    /// `[value, name, slot]` → nothing, having stored the value where the link
    /// points. A store, so it leaves the stack as it found it.
    pub const LINK_SET: u16 = EVENT_BASE + 10;
    /// `[declared, arg …]` with the count in the inline operand — `eval` inside a
    /// procedure body, whose script runs against that procedure's *frame*.
    ///
    /// `declared` is the list of names the body gave to `global`, pushed by the
    /// compiler because only it knows them: tclsh runs an `eval`'s script in
    /// exactly the calling frame's variable context, so a bare read of a global
    /// refuses there unless `global` linked it, and the projection the handler
    /// builds is the frame's own names plus those.
    ///
    /// In this block rather than in the core space it arrived in, because it and
    /// the two ids around it run a nested script through the interpreter — which
    /// is what this block is for — and because 58 through 60 were three of the
    /// core ids `proc`, `{*}` and the event loop had already taken.
    pub const EVAL_FRAME: u16 = EVENT_BASE + 11;
    /// `[lambda, arg …]` with the count in the inline operand — `apply`.
    pub const APPLY: u16 = EVENT_BASE + 12;
    /// `subst ?-nobackslashes? ?-nocommands? ?-novariables? string`:
    /// `[declared, arg …]` with the count in the inline operand → the
    /// substituted text.
    ///
    /// In this block for the reason the three above it are: the command runs
    /// nested scripts, and it runs them — and reads its variables — against the
    /// *calling* frame. Doing either against the globals instead would read the
    /// wrong variables inside a procedure and never say so, which is why
    /// [`crate::cmd_subst`] goes through the same projection `uplevel` does.
    ///
    /// Nothing is settled while compiling: which words are options, whether the
    /// value parses, and where a parse of it fails are all decided when the op
    /// runs, exactly as `TclNRSubstObjCmd` decides them.
    pub const SUBST: u16 = EVENT_BASE + 13;

    // ── variables whose *name* the script computes ───────────────────────
    // `set $n`, `unset $n`, `incr $n`, `append $n`, `info exists $n`: the four
    // ops below are what a variable access lowers to when the name is not a
    // literal, and they are in this block because each resolves that name the
    // way a computed `upvar` target is resolved — through the interpreter, so a
    // name the chunk's table does not carry can be interned against the
    // interpreter's variables. See [`crate::cmd_scope::dynamic_link`].
    //
    // Numbered above [`SUBST`] rather than into the `+ 4` … `+ 7` gap, which is
    // left free deliberately: a chunk cached on disk carries the id and not the
    // name, so an id that once meant `info exists` must never mean anything
    // else.
    /// `[declared, name]` → what the variable that name spells holds.
    ///
    /// `arg` says what an *unset* variable answers, because the three commands
    /// that read one disagree: 0 refuses, as `$x` does; 1 answers `0`, which is
    /// what `incr` needs to create a counter at zero; 2 answers the empty
    /// string, which is what `append` and `lappend` need to create a variable by
    /// extending it. [`LINK_GET`] takes the same flag for the first two.
    pub const DYN_GET: u16 = EVENT_BASE + 14;
    /// `[value, declared, name]` → nothing, having stored the value in the
    /// variable that name spells, creating it if it did not exist. A store, so
    /// it leaves the stack as it found it — the caller `Dup`s when the command
    /// yields what it assigned, exactly as [`Compiler::emit_set_var`]'s callers
    /// do.
    ///
    /// The name rides on *top*, above the value, which is the opposite of
    /// [`DYN_GET`]'s order and is what lets `append` and `incr` evaluate a
    /// computed name exactly once: the read leaves `[name, value]`, and from
    /// there this order is three stack ops away. See
    /// [`Compiler::dyn_write_back`].
    pub const DYN_SET: u16 = EVENT_BASE + 15;
    /// `[declared, name]` → nothing, having unset the variable that name spells.
    /// `arg` is 1 when an absent variable is an error, which is `unset` without
    /// `-nocomplain`.
    pub const DYN_UNSET: u16 = EVENT_BASE + 16;
    /// `[declared, name]` → 1 when the variable that name spells is set, 0
    /// otherwise. Never creates it: `info exists $n` must not make `n`'s value a
    /// variable by asking about it.
    pub const DYN_EXISTS: u16 = EVENT_BASE + 17;
    // ── end of the computed-name block ───────────────────────────────────

    /// `[name, arg …]` with the count in the inline operand — call the function
    /// an inline `rust { ... }` block exported. Emitted only for a name
    /// [`crate::rust_ffi::is_exported`] answered for while compiling.
    pub const FFI_CALL: u16 = 63;

    /// `[name, spec, entry]` → `""`, having registered the procedure `name`
    /// with the formal-argument list `spec` and the body entry point `entry` in
    /// the interpreter's run-time command table.
    ///
    /// What a `proc` outside a script's top level lowers to. Its body is
    /// compiled in place, behind a jump, exactly as a top-level one's is — the
    /// difference is entirely in *when the name starts answering*, and this op
    /// is that moment. `if {0} {proc f {} {}}` compiles the body and never runs
    /// this, so `f` is `invalid command name "f"`, which is what tclsh 9.0.4
    /// answers (measured).
    ///
    /// 60 rather than 35: nothing between 48 and 61 without an explicit arm in
    /// [`crate::runtime::install_hooks`] reaches this module at all — the range
    /// test below it routes to [`crate::cmd_list`] — so this id and
    /// [`DYN_CALL`] each need one, and keeping them adjacent keeps that pair
    /// visible.
    pub const PROC_DEFINE: u16 = 60;

    /// `[line, name, arg …]` with the count in the inline operand — call the
    /// command `name`, resolving it in the run-time command table when the call
    /// happens rather than while the script is compiled.
    ///
    /// The one op in this table whose callee is not known while compiling, and
    /// there are two reasons a callee can fail to be known:
    ///
    /// * a procedure defined by a `proc` that is not at the script's top level
    ///   ([`PROC_DEFINE`]) — the name exists only once the defining code has
    ///   run, so a call site cannot be lowered to `Op::Call`;
    /// * a command Tk registered. Tk registers `button`, `pack`, `wm` and the
    ///   rest during `Tk_Init`, long after a script that says `button .b` was
    ///   compiled; see [`crate::tk::dispatch`].
    ///
    /// The two are one lookup, in that order: a procedure the script defined
    /// shadows a foreign command of the same name, which is the order tclsh
    /// resolves in.
    ///
    /// The line rides on the stack because the failure this op can raise —
    /// `invalid command name`, `wrong # args` — is located, and dropping the
    /// line would change a diagnostic that is pinned against tclsh.
    ///
    /// 61 rather than 64: [`ASSOC_BASE`] is 64, and an id at or above it is
    /// dispatched to [`crate::assoc`] by range.
    pub const DYN_CALL: u16 = 61;

    /// `[line, flag, word, flag, word, …]` with the number of stack values in
    /// the inline operand — call the command those words spell, after splicing
    /// every word whose `flag` is 1 into the arguments it lists.
    ///
    /// What a command containing a `{*}` word lowers to (rule 5 of the
    /// dodekalogue). Such a command has no argument count until it runs: `n
    /// {*}$list` passes as many arguments as `$list` has elements, and the
    /// *name* may be expanded too — `{*}{n x} y` calls `n` with `x y` in tclsh
    /// 9.0.4 (measured). Both are things this compiler decides for every other
    /// command while it reads the script, so neither the built-in lowerings nor
    /// `Op::Call` can be reached: the whole dispatch happens in
    /// [`crate::procs::expand_call_op`], which is [`DYN_CALL`]'s resolution with
    /// the builtins added under it.
    ///
    /// The reference implementation makes the same division. `CompileExpanded`
    /// (`generic/tclCompile.c:1883-1941`) is reached for *any* command with an
    /// expanded word, ahead of the per-command compile procedures, and emits an
    /// `INST_EXPAND_STKTOP` per expanded word followed by one
    /// `INST_INVOKE_EXPANDED` — because, as the comment there says, "the stack
    /// depth during argument expansion can only be managed at runtime, as the
    /// number of elements in the expanded lists is not known at compile time".
    /// `set {*}{a b}` therefore does not reach `TclCompileSetCmd` in tclsh
    /// either; it reaches the generic invoke, which is what this op is.
    ///
    /// The flags ride on the stack beside the words rather than in the operand
    /// because the operand is the value count the op consumes — the one number
    /// the VM needs in order to balance the stack — and a mask there would cap a
    /// command at 64 words while making that cap invisible. One `Op::LoadInt`
    /// per word is the whole cost, and only a command that contains a `{*}` pays
    /// it.
    ///
    /// 59 rather than a fresh block: 58 and 59 are the last two free ids under
    /// [`PROC_DEFINE`], and this op belongs beside the other two the call
    /// machinery owns. Like them it needs an explicit arm in
    /// [`crate::runtime::install_hooks`], since the range test below 61 routes to
    /// [`crate::cmd_list`].
    pub const EXPAND_CALL: u16 = 59;

    /// Pop a value and push Tcl's boolean reading of it — 1 or 0 — or refuse it.
    /// `arg` is 0 for a condition and 1 for `!`, which differ in how they word
    /// the refusal. Emitted only where the value could be a string, so the
    /// arithmetic a condition is usually made of stays native and traceable;
    /// `super::Compiler::yields_number` is the test.
    pub const BOOL: u16 = 15;

    /// `[value]` → the number that value spells, or a refusal if it spells a
    /// NaN. `expr {$x}` is 7 when `x` is `007`, 16 when it is `0x10`, and `abc`
    /// when it spells no number at all; `expr {0.0/0.0}` is `domain error:
    /// argument not in valid range`.
    ///
    /// The op an `expr` used to end in unconditionally. It is emitted in two
    /// places now, both of them cold: after an expression whose result could
    /// still be a string (`super::Compiler::yields_number` says so — never
    /// after arithmetic), and after arithmetic on an operand that is a literal
    /// `inf` or `nan` (`super::Compiler::may_be_non_finite`), which is the
    /// only way a script can spell a NaN into an operation that would otherwise
    /// lower natively. A counted loop reaches neither, which is what keeps its
    /// body free of extension ops and inside the tracing JIT.
    pub const CANON: u16 = 47;

    /// `[a, b]` → 1 or 0: `expr`'s always-string comparisons — `eq ne lt gt le
    /// ge` — with `arg` naming which, in `super::Compiler::str_cmp`'s order.
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

    // The list commands that name a variable or build one, 48–57. They are in
    // the range `runtime::extension` routes to [`crate::cmd_list`] by id, and
    // between 48 and 61 only [`PROC_DEFINE`] and [`DYN_CALL`] have an explicit
    // arm ahead of that range test — another one there would shadow this block
    // silently.

    /// `[list, count]` → the unassigned remainder, then one value per variable
    /// in reverse, so that a `SetVar` per variable pops them in order.
    pub const LASSIGN: u16 = 48;
    /// `[name, slot?, place, index …, value]` → the variable's new value, also
    /// stored. Like [`LAPPEND_VAR`] it reaches the variable itself, so that an
    /// unset one is `can't read "…": no such variable` rather than a read of
    /// the empty string.
    pub const LSET: u16 = 49;
    /// `[name, slot?, place, index …]` → the element removed, with the
    /// variable left holding the rest.
    pub const LPOP: u16 = 50;
    /// `[name, slot?, place, first, last, element …]` → the variable's new
    /// value, also stored.
    pub const LEDIT: u16 = 51;
    /// `[count, element …]` → the elements repeated `count` times.
    pub const LREPEAT: u16 = 52;
    /// `[list, index …]` → the list without those elements.
    pub const LREMOVE: u16 = 53;
    /// `[arg …]` → Tcl 9's arithmetic sequence.
    pub const LSEQ: u16 = 54;

    /// `lmap`'s three steps beyond the four `foreach` already has. `INIT`
    /// builds the same loop state with an accumulator on the end, `COLLECT`
    /// moves one iteration's value into it, and `RESULT` takes the state apart
    /// and yields the accumulated list. The accumulator rides the VM stack with
    /// the rest of the state rather than living in a hidden global, so an
    /// `lmap` inside a recursive procedure keeps its own.
    pub const LMAP_INIT: u16 = 55;
    pub const LMAP_COLLECT: u16 = 56;
    pub const LMAP_RESULT: u16 = 57;

    /// The bitwise operators, in Tcl's semantics rather than the VM's.
    ///
    /// fusevm's `Op::BitAnd`/`BitOr`/`BitXor`/`BitNot`/`Shl`/`Shr` coerce their
    /// operands through `Value::to_int`, which reads `1.5` as 1 and `"abc"` as
    /// 0; Tcl refuses both (`cannot use floating-point value "1.5" as left
    /// operand of "|"`). It also masks a shift distance to 6 bits, where Tcl
    /// saturates a right shift and promotes an overflowing left shift.
    ///
    /// Emitted only where the compiler cannot prove both operands are integers
    /// (`super::Compiler::yields_integer`), so an expression written in
    /// literals keeps the native op — and with it the tracing JIT, which
    /// rejects `Op::Extended`. A shift by a literal distance is the other case
    /// that stays native; see `super::Compiler::native_shift`.
    ///
    /// Numbered from 40 rather than 33: 33 and 34 are [`LAPPEND_VAR`] and
    /// [`LAPPEND_SLOT`], and an id collision here dispatches one command's op to
    /// another's handler with nothing to catch it.
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
    /// (`super::Compiler::yields_number`), so `expr {+1}` still lowers to a
    /// single `LoadInt`.
    pub const UPLUS: u16 = 46;
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
    /// `[name, place, key, increment]` → the updated dict, stored. `dict incr`
    /// reaches its variable by place like `dict set` does, because it creates
    /// the variable when it does not exist and its read must therefore tolerate
    /// absence where a bare `$d` refuses it.
    pub const DICT_INCR: u16 = ASSOC_BASE + 22;
    /// `[dict, key, value, …, count]` → the dict with those pairs written over
    /// it. `dict replace` differs from `dict merge` only in taking loose pairs
    /// rather than whole dicts.
    pub const DICT_REPLACE: u16 = ASSOC_BASE + 23;
    /// `[dict, key, …, key, default, count]` → the value at the key path, or
    /// `default` when any step of the path is missing. `dict getdef` and
    /// `dict getwithdefault` are the same subcommand under two names.
    pub const DICT_GETDEF: u16 = ASSOC_BASE + 24;
    /// `[name, place, key, …, count]` → the dict without that key path.
    /// Reached by place rather than by value for the same reason `dict set` is:
    /// the variable may be missing, and reading it must not refuse.
    pub const DICT_UNSET: u16 = ASSOC_BASE + 25;
    /// `[name, place, key, value, …, count]` → the updated dict, with the
    /// values appended to the key's value as list elements.
    pub const DICT_LAPPEND: u16 = ASSOC_BASE + 26;
    /// `[name, place, key, value, …, count]` → the updated dict, with the
    /// values concatenated onto the key's value as text.
    pub const DICT_APPEND: u16 = ASSOC_BASE + 27;
    /// `[dict, which, pattern, …, count]` → the pairs whose key (`which` = 0)
    /// or value (`which` = 1) matches any of the glob patterns. No pattern
    /// matches nothing, which is what the reference implementation answers.
    pub const DICT_FILTER: u16 = ASSOC_BASE + 28;
    /// The walk `dict for`, `dict map` and `dict filter … script` share, with
    /// the step in the inline operand — see [`crate::assoc::Step`].
    ///
    /// The walk's state rides the VM stack, pushed before the loop and read
    /// through the top of it, exactly as `lmap`'s does and for the same reason:
    /// hidden globals gave one call site one cursor, so a `dict for` whose body
    /// re-entered the same `dict for` clobbered the outer walk's position and
    /// the outer loop stopped early with no error to show for it (measured
    /// against tclsh 9.0.4, which visits every pair at every level).
    pub const DICT_EACH: u16 = ASSOC_BASE + 29;
    /// `dict update`'s binding half: `[name, place, (key, varName, varPlace) …,
    /// count]` → the *record* the write-back needs, as a `Value::Array` of the
    /// dictionary's name and place followed by one `(key, varPlace)` pair each.
    ///
    /// The keys are read here and kept, because the reference implementation
    /// evaluates them once and hands the finalizer the list it built from them
    /// (`generic/tclDictObj.c:3536-3539`): `dict update d [incr n] x {…}` leaves
    /// `n` at 1 in tclsh 9.0.4, measured, whatever the body does to it.
    pub const DICT_UPDATE_BIND: u16 = ASSOC_BASE + 30;
    /// `dict update`'s write-back, run as the cleanup of a
    /// [`Compiler::finally_region`](crate::compiler::Compiler): the record is
    /// consumed from under the `arg` values the ending left on top of it, and
    /// each key takes the value its variable now holds — or leaves the
    /// dictionary, when the variable does not (`:3573-3591`, where a failed read
    /// is "an instruction to remove the key").
    pub const DICT_UPDATE_END: u16 = ASSOC_BASE + 31;
    /// `dict with`'s binding half: `[name, place, pathKey …, pathCount]` → the
    /// record the write-back needs.
    ///
    /// Unlike [`DICT_UPDATE_BIND`] the names are not in the chunk: they are the
    /// dictionary's own *keys*, read when the command runs, so each is resolved
    /// to a home then rather than to a place while the script is lowered
    /// (`crate::cmd_scope::dict_with_home`). The record carries the resolved
    /// home for a key the body names and the key's *value* for one it does not —
    /// which is what lets the write-back put every key back the way
    /// `TclDictWithFinish` does (`generic/tclDictObj.c:3926-3941`), including
    /// the ones the body deleted from the dictionary.
    pub const DICT_WITH_BIND: u16 = ASSOC_BASE + 32;
    /// `dict with`'s write-back, run as the cleanup of a
    /// [`Compiler::finally_region`](crate::compiler::Compiler) exactly as
    /// [`DICT_UPDATE_END`] is — `FinalizeDictWith` (`generic/tclDictObj.c:3696`)
    /// is the same NRE callback shape as `FinalizeDictUpdate`, with a key *path*
    /// added and both of its silent paths kept: a dictionary variable that no
    /// longer exists drops the whole write-back (`:3875-3877`), and so does a
    /// path that no longer leads anywhere (`:3912-3917`).
    pub const DICT_WITH_END: u16 = ASSOC_BASE + 33;

    /// Where the string commands' ops begin — the `string` ensemble, `append`
    /// and `format` — dispatched to [`crate::cmd_string`], which names them.
    /// The inline operand is the number of stack values the op consumes.
    pub const STRING_BASE: u16 = 128;

    /// Where `regexp` and `regsub` begin, dispatched to [`crate::regexp`].
    ///
    /// Above [`STRING_BASE`] because [`crate::runtime`] tests the ranges from
    /// the highest base down: a base below it would be swallowed by the string
    /// ensemble's arm, and silently — an id that lands in the wrong module's
    /// range is a wrong answer at run time, not a compile error.
    pub const REGEXP_BASE: u16 = 192;

    // ── the command modules' id blocks ───────────────────────────────────
    //
    // Everything from [`SUBSYSTEM_BASE`] up belongs to exactly one command
    // module, and each module's ids are a block of its own. The blocks exist
    // because the alternative — picking whichever id under 64 happened to be
    // free — is how two modules come to claim the same number, and a duplicate
    // id is not a build error: the first arm that matches it wins, so the op
    // silently calls the wrong handler. That has happened in this tree once
    // already. `tests/ext_ids.rs` fails the build if any two of these overlap
    // or if a module allocates outside its own block.
    //
    // A block is dispatched by the module's own `is_op`, or by an exact arm,
    // *before* control reaches `runtime::extension` — whose guard chain tests
    // `id >= REGEXP_BASE` first and would otherwise swallow every one of them.

    /// The first id belonging to a command module's block rather than to the
    /// core op space of 0–63 and the three ranges above it.
    pub const SUBSYSTEM_BASE: u16 = 256;
    /// How wide every module's block is. Wide enough that no module has yet
    /// filled one, and a power of two so a block's owner is `id / BLOCK`.
    pub const BLOCK: u16 = 64;

    /// Channels — `open`, `close`, `gets`, `read`, `puts` to a channel, and the
    /// rest of the ensemble ([`crate::cmd_channel`]). Dispatched from the
    /// extension *closure* rather than from `runtime::extension`, because these
    /// are the ops that need the running interpreter's output sink.
    pub const CHANNEL_BASE: u16 = SUBSYSTEM_BASE;
    /// One past the channel block, so the dispatcher can test a bounded range
    /// rather than `id >= CHANNEL_BASE` — which would swallow every block
    /// below.
    pub const CHANNEL_END: u16 = CHANNEL_BASE + BLOCK;
    /// `open fileName ?access?` → the channel's name.
    pub const OPEN: u16 = CHANNEL_BASE;
    /// `close channelId ?direction?`.
    pub const CLOSE: u16 = CHANNEL_BASE + 1;
    /// `gets channel ?varName?`. With a variable the operand is where it lives,
    /// as `regexp`'s match variables are, and the result is the count.
    pub const GETS: u16 = CHANNEL_BASE + 2;
    /// `read channel ?numChars?` and `read ?-nonewline? channel`.
    pub const READ: u16 = CHANNEL_BASE + 3;
    /// `puts ?-nonewline? channelId string`. [`PUTS`] stays the lowering for
    /// the form with no channel, so a script that never names one pays nothing.
    pub const CH_PUTS: u16 = CHANNEL_BASE + 4;
    /// `flush channelId`.
    pub const FLUSH: u16 = CHANNEL_BASE + 5;
    /// `eof channelId`.
    pub const EOF: u16 = CHANNEL_BASE + 6;
    /// `seek channelId offset ?origin?`.
    pub const SEEK: u16 = CHANNEL_BASE + 7;
    /// `tell channelId`.
    pub const TELL: u16 = CHANNEL_BASE + 8;
    /// `fconfigure channelId ?-option value ...?`.
    pub const FCONFIGURE: u16 = CHANNEL_BASE + 9;
    /// `namespace`, `variable` and `rename` ([`crate::cmd_namespace`]), and
    /// `source` and `tcl_findLibrary` ([`crate::cmd_source`]), which share a
    /// block because they are one feature: a namespace-aware `source`.
    pub const NS_BASE: u16 = SUBSYSTEM_BASE + BLOCK;
    /// `after`, `update`, `vwait`, `uplevel` and the `info` queries that need
    /// the interpreter ([`crate::cmd_after`], [`crate::cmd_scope`],
    /// [`crate::cmd_info`]).
    pub const EVENT_BASE: u16 = SUBSYSTEM_BASE + 2 * BLOCK;
    /// `package`, whose `require Tk` drives Tk's initialisation.
    pub const PKG_BASE: u16 = SUBSYSTEM_BASE + 3 * BLOCK;
    /// `[line, name, arg …]` with the count in the inline operand — the
    /// `package` command, whole.
    ///
    /// Nothing about it is decided while compiling, not even the argument
    /// count: every answer depends on a registry that only exists at run time,
    /// and a `package require` may run an `ifneeded` script, so the op is
    /// handed the same evaluator [`EVAL`] gets. See [`crate::cmd_package`].
    pub const PACKAGE: u16 = PKG_BASE;
    /// `expr`'s math functions, dispatched to [`crate::expr_math`]. The id past
    /// this base is the function's index in that module's table and the inline
    /// operand is the actual argument count, which is what lets arity be
    /// reported when the call runs rather than while it compiles.
    ///
    /// This block and the two below it are dispatched by
    /// `crate::runtime::extension`'s range chain rather than from the closure,
    /// because none of them needs the interpreter — which is why the chain
    /// tests them ahead of [`REGEXP_BASE`].
    pub const MATH_BASE: u16 = SUBSYSTEM_BASE + 4 * BLOCK;
    /// The `clock` ensemble, dispatched to [`crate::cmd_clock`].
    pub const CLOCK_BASE: u16 = SUBSYSTEM_BASE + 5 * BLOCK;
    /// `file`, `glob`, `pwd` and `cd`, dispatched to [`crate::cmd_file`].
    pub const FILE_BASE: u16 = SUBSYSTEM_BASE + 6 * BLOCK;
    // ── the encoding block ───────────────────────────────────────────────
    /// The `encoding` ensemble, dispatched to [`crate::cmd_encoding`].
    pub const ENCODING_BASE: u16 = SUBSYSTEM_BASE + 7 * BLOCK;
    // ── end of the encoding block ────────────────────────────────────────
    /// The `info` ensemble's own ops, dispatched to [`crate::cmd_info`].
    ///
    /// `info` arrived on the published line with its ids based at 208, which is
    /// inside the range `crate::runtime::extension`'s guard chain reads as
    /// [`REGEXP_BASE`]'s. It gets a block of its own here instead, because a
    /// block is what `tests/ext_ids.rs` can assert and a bare 208 is what it
    /// cannot.
    pub const INFO_BASE: u16 = SUBSYSTEM_BASE + 8 * BLOCK;
    /// One past the `info` block, so the dispatcher tests a bounded range
    /// rather than `id >= INFO_BASE` — which would claim whatever block is
    /// added above it next.
    pub const INFO_END: u16 = INFO_BASE + BLOCK;
    // ── the binary block ─────────────────────────────────────────────────
    /// The `binary` ensemble, dispatched to [`crate::cmd_binary`]. Bounded like
    /// the two blocks below it, so that adding a block above this one does not
    /// silently route its ops here.
    pub const BINARY_BASE: u16 = SUBSYSTEM_BASE + 9 * BLOCK;
    /// One past the `binary` block.
    pub const BINARY_END: u16 = BINARY_BASE + BLOCK;
    // ── end of the binary block ──────────────────────────────────────────
}

/// Wide extension opcode ids, whose payload is a `usize` rather than a byte.
pub mod ext_wide {
    /// Enter a `catch` region. The payload is the op index of the region's
    /// error handler, which the driver in [`crate::runtime`] resumes at.
    pub const CATCH: u16 = 0;

    /// A command is about to run, and the payload is its line. Emitted only
    /// when `super::Compiler::debug` is set — a chunk compiled the ordinary
    /// way carries none of these, so nothing is paid for a debugger that is not
    /// attached.
    pub const DBG_LINE: u16 = 1;

    /// Raise the message on top of the stack as an error located at the script
    /// line the payload carries.
    ///
    /// [`super::ext::ERROR`] raises the same message with no location, which is
    /// what `error` and `return -code error` want: a script raising its own
    /// error is not reporting a place in the source. A failure the compiler
    /// found and deferred is — it knows the command's line, and reporting it is
    /// what keeps `(file "…" line N)` on the diagnostics that used to be
    /// refusals. See `super::Compiler::defer`.
    pub const ERROR_AT: u16 = 2;
}

/// Whether a failure is one the reference interpreter reports only when the
/// command runs, rather than while the script is being read.
///
/// The three classes are named by the interpreter's own wording rather than by
/// a guess at what a message means, and each is generated in this crate with
/// exactly one meaning:
///
/// * `wrong # args:` — a command invoked with an argument count its signature
///   does not admit. `Tcl_WrongNumArgs` is reached from a command's
///   implementation, so tclsh cannot report it before the command is called.
/// * `invalid command name ` — a name no command answers to. tclsh resolves a
///   name at invocation, which is why a typo in a branch never taken is not an
///   error there.
/// * `unknown or ambiguous subcommand ` — the same, one level down, for an
///   ensemble.
///
/// A fourth class defers for a different reason:
///
/// * `… is not supported yet` — something tclrs cannot lower and tclsh *can*.
///   The interpreter never reports these at all, so the question is not when to
///   report but whether the script survives: `catch {info locals}` answers 0
///   there and killed the whole script here, and `if {0} {info locals}` ran
///   there and was refused here. Deferring makes the refusal catchable and
///   leaves an unexecuted one silent, which is what tclsh does; a script that
///   really reaches the construct still gets the same message, one phase later.
///   Being refused earlier than tclsh is not a service when tclsh's answer is to
///   work.
///
/// Only a parse error stays where it is: it is a property of the text and cannot
/// wait for control to arrive. A refusal raised *after* its handler has emitted
/// ops cannot be deferred either — there is nothing to roll back — and
/// [`Compiler::command`] re-arms it as an ordinary compile error.
///
/// The first three wordings are Tcl's own and are pinned against tclsh by the
/// differential suites, so they cannot drift here without a test noticing.
/// `if`'s words, sorted into the branches they name.
struct IfPlan<'a> {
    /// `(expression, script)`, in the order they are tested.
    branches: Vec<(&'a Word, &'a Word)>,
    /// The `else` script, present or not.
    otherwise: Option<&'a Word>,
}

/// Read `if expr ?then? body ?elseif expr ?then? body ...? ?else? ?body?`.
///
/// Ported from `Tcl_IfObjCmd`, whose grammar is looser than the synopsis in
/// `if(n)` suggests in one way that matters: **the `else` keyword is
/// optional**. What stands after the last body is the else script whatever it
/// says, so `if {$x} {a} {b}` — a form ordinary Tcl is written in — is legal,
/// and a stray word there is `extra words after "else" clause` rather than a
/// complaint about the word itself. Both measured against tclsh 9.0.3.
///
/// The two arity diagnostics quote a *word*, which is the one detail this
/// cannot always reproduce: tclsh quotes the substituted value and only the
/// source is here. A literal word is quoted as written; a word that substitutes
/// falls back to the keyword that introduced its clause. Recorded in BUGS.md.
fn parse_if(args: &[Word]) -> Result<IfPlan<'_>, String> {
    let mut branches = Vec::new();
    let mut i = 0;
    loop {
        // `Tcl_IfObjCmd` quotes `objv[i-1]` here: the command name on the
        // first pass and the `elseif` that led back on every later one.
        let clause = match i {
            0 => "if",
            _ => args[i - 1].as_literal().unwrap_or("if"),
        };
        let Some(cond) = args.get(i) else {
            return Err(format!(
                "wrong # args: no expression after \"{clause}\" argument"
            ));
        };
        // The expression's own text, which the next diagnostic quotes.
        let mut following = cond.as_literal().unwrap_or(clause);
        i += 1;
        if i >= args.len() {
            return Err(format!(
                "wrong # args: no script following \"{following}\" argument"
            ));
        }
        if args[i].as_literal() == Some("then") {
            following = "then";
            i += 1;
        }
        let Some(body) = args.get(i) else {
            return Err(format!(
                "wrong # args: no script following \"{following}\" argument"
            ));
        };
        branches.push((cond, body));
        i += 1;

        if i >= args.len() {
            return Ok(IfPlan {
                branches,
                otherwise: None,
            });
        }
        if args[i].as_literal() == Some("elseif") {
            i += 1;
            continue;
        }
        // Anything else ends the chain. `else` is a keyword when it is written
        // and nothing when it is not.
        if args[i].as_literal() == Some("else") {
            i += 1;
            if i >= args.len() {
                return Err("wrong # args: no script following \"else\" argument".to_string());
            }
        }
        if i < args.len() - 1 {
            return Err(
                "wrong # args: extra words after \"else\" clause in \"if\" command".to_string(),
            );
        }
        return Ok(IfPlan {
            branches,
            otherwise: Some(&args[i]),
        });
    }
}

fn defers_to_run_time(msg: &str) -> bool {
    msg.starts_with("wrong # args:")
        || msg.starts_with("invalid command name ")
        || msg.starts_with("unknown or ambiguous subcommand ")
        || msg.contains("is not supported yet")
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
/// Two passes, for two things a single forward walk cannot know.
///
/// Reading `$x` lowers to a bare `GetVar`, which cannot fail, but reading a
/// variable that holds an array must — and the `set a(i) v` that makes it one
/// may be compiled after the `$a` that reads it. The first pass records every
/// name used as an array; the second, knowing them, guards just those names.
///
/// A `proc` that is not at the script's top level defines its procedure only
/// when the enclosing code runs, so every call to that name has to resolve at
/// run time — including the calls written *above* the definition, and including
/// the ones that would otherwise have found a top-level `proc` of the same name
/// at compile time. The first pass records those names; the second, knowing
/// them, lowers their call sites through [`ext::DYN_CALL`].
///
/// Nothing else differs between the passes, so a script with neither an array
/// nor a nested `proc` compiles in one pass exactly as it did before and pays
/// nothing.
pub fn compile(script: &Script) -> Result<fusevm::Chunk, CompileError> {
    lower(script, false, false)
}

/// Lower a script that will run inside a frame projection — a nested script an
/// `eval`, `uplevel`, `subst` or `apply` runs against a procedure activation's
/// variables. See [`Compiler::projected`] for the single difference.
pub fn compile_projected(script: &Script) -> Result<fusevm::Chunk, CompileError> {
    lower(script, false, true)
}

/// Lower a script with a line marker before every command, for the debug
/// adapter. The markers are the only difference: a debugger single-steps the
/// same bytecode a run executes, rather than a second lowering written for it.
pub fn compile_debug(script: &Script) -> Result<fusevm::Chunk, CompileError> {
    lower(script, true, false)
}

fn lower(script: &Script, debug: bool, projected: bool) -> Result<fusevm::Chunk, CompileError> {
    // Both extra passes, and both reasons a second one is needed: a name used
    // as an array, and a `proc` whose definition only happens when the
    // enclosing code runs.
    let first = Compiler::run(script, ArrayNames::new(), HashSet::new(), debug, projected)?;
    let (mut chunk, tolerant, incr_sites, procs, slot_names) =
        if first.seen_arrays.is_empty() && first.seen_runtime.is_empty() {
            let procs = signature_table(&first);
            let names = first.slot_names.clone();
            (
                first.b.build(),
                first.tolerant_reads,
                first.incr_sites,
                procs,
                names,
            )
        } else {
            let second = Compiler::run(
                script,
                first.seen_arrays,
                first.seen_runtime,
                debug,
                projected,
            )?;
            let reads = second.tolerant_reads.clone();
            let incrs = second.incr_sites.clone();
            let procs = signature_table(&second);
            let names = second.slot_names.clone();
            (second.b.build(), reads, incrs, procs, names)
        };
    // Tcl's integers are arbitrary-precision, and so are this frontend's: an
    // `i64` that overflows promotes, in the numeric hook. Native codegen would
    // wrap instead, so ask fusevm for the overflow-checked lowering —
    // `Add`/`Sub`/`Mul` stay native registers on the common path and deopt into
    // the hook when a result does not fit. Without this, the JIT and the AOT
    // compiler print -9223372036854775808 where the interpreter answers
    // 9223372036854775808.
    chunk.int_overflow_deopt = true;
    crate::runtime::note_tolerant_reads(&chunk, &tolerant);
    crate::runtime::note_incr_sites(&chunk, &incr_sites);
    crate::runtime::note_procs(&chunk, &procs);
    // Which name each frame slot was written as, for the frames a *lambda*
    // occupies. A procedure's are carried by the chunk itself
    // (`fusevm::Chunk::sub_slot_names`, 0.17.0); a lambda body is emitted inside
    // the enclosing chunk and attributed by op range instead — see
    // [`crate::cmd_scope`].
    crate::cmd_scope::note_slot_names(&chunk, &slot_names);
    Ok(chunk)
}

/// The procedures a lowering collected, in the shape `info` answers from.
///
/// `prescan` gathers every signature before anything is emitted, so this is
/// already complete by the time the chunk is built — which is what lets
/// `info args` answer for a procedure defined further down the script.
fn signature_table(c: &Compiler) -> Vec<(String, crate::runtime::ProcParams)> {
    c.procs
        .iter()
        .map(|(name, sig)| {
            let params = sig
                .params
                .iter()
                .map(|p| (p.name.clone(), p.default.clone()))
                .collect();
            (
                name.clone(),
                crate::runtime::ProcParams {
                    params,
                    body: sig.body.clone(),
                },
            )
        })
        .collect()
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
    /// Local names `upvar #0 other local` bound to a *differently named*
    /// global while the script was read, which is the one link that needs no
    /// run-time indirection at all. Consulted by [`Compiler::var_place`]; see
    /// [`crate::cmd_scope`].
    pub aliases: crate::cmd_scope::Aliases,
    /// Local names bound by an `upvar` whose target only the running script
    /// knows — a computed level, a computed name, an array element. The slot
    /// holds a [`crate::cmd_scope::Link`] descriptor rather than a value, and
    /// [`Compiler::var_place`] answers [`Place::Link`] for the name.
    pub links: crate::cmd_scope::Links,
    pub next_slot: u16,
}

/// A control-flow body, in whichever of its two states it is in.
///
/// A body's text is parsed separately from the script that contains it, and a
/// failure there is one the reference interpreter reports only when the body
/// runs. [`Compiler::body_of`] carries that failure instead of raising it, so
/// the command owning the body still compiles and the raise lands where the
/// body's code would have.
pub(crate) enum Body {
    Script(Script),
    /// The message the body's own parse failed with.
    Deferred(String),
}

/// What reading an unset variable answers, for the commands that read one
/// through a name the script computed. See [`ext::DYN_GET`].
///
/// A property of the *command*, not of the variable: `$x` refuses, `incr x`
/// creates a counter at zero, and `append x`/`lappend x` create the variable by
/// extending nothing. The literal-name path settles the same three by choosing
/// between `scalar_get`, a tolerant read site and `elem_get_tolerant`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Absent {
    /// `can't read "name": no such variable`, which is what `$x` gives.
    Refuse = 0,
    /// `0` — `incr`.
    Zero = 1,
    /// `""` — `append` and `lappend`.
    Empty = 2,
}

/// Where a variable lives once the script is lowered: a frame slot inside a
/// procedure body, a name index in the VM's global table anywhere else, or —
/// for a name `upvar` bound — a frame slot holding a *link* to one of those,
/// resolved when the command ran rather than while the script was read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Place {
    Slot(u16),
    Global(u16),
    /// A frame slot holding a [`crate::cmd_scope::Link`] descriptor. Every read
    /// and write of the name goes through the descriptor, which is what lets
    /// `upvar $level $other local` name its target when the command runs.
    Link(u16),
}

impl Place {
    /// The one integer form every op that reaches a variable itself takes: a
    /// name index as itself, a frame slot as `-(slot + 1)`, and a link as
    /// `-(slot + 1) - LINK_BIAS`.
    ///
    /// Three ranges in one signed operand rather than a second operand, because
    /// the ops that take it already carry their argument count in the inline
    /// operand and a place is one of the counted values. The bias is far outside
    /// the `u16` slot range, so the three cannot collide.
    pub(crate) const LINK_BIAS: i64 = 1 << 20;

    pub(crate) fn encode(self) -> i64 {
        match self {
            Place::Global(idx) => i64::from(idx),
            Place::Slot(slot) => -i64::from(slot) - 1,
            Place::Link(slot) => -i64::from(slot) - 1 - Place::LINK_BIAS,
        }
    }

    pub(crate) fn decode(raw: i64) -> Place {
        if raw <= -Place::LINK_BIAS {
            Place::Link((-(raw + Place::LINK_BIAS) - 1) as u16)
        } else if raw < 0 {
            Place::Slot((-raw - 1) as u16)
        } else {
            Place::Global(raw as u16)
        }
    }

    /// Whether this place lives in the call frame rather than the global table —
    /// which a link does, since the descriptor is a frame slot.
    pub(crate) fn in_frame(self) -> bool {
        matches!(self, Place::Slot(_) | Place::Link(_))
    }

    /// The operand for an op that already says "this is a frame place" some
    /// other way — a `_SLOT`-flavoured op id, or a separate pushed flag. A slot
    /// stays its own index and a link is written `-(slot + 1)`, which the
    /// non-negative slot range cannot reach.
    pub(crate) fn frame_operand(self) -> i64 {
        match self {
            Place::Slot(slot) => i64::from(slot),
            Place::Link(slot) => -i64::from(slot) - 1,
            Place::Global(idx) => i64::from(idx),
        }
    }
}

pub(crate) struct Compiler {
    pub(crate) b: ChunkBuilder,
    /// Op indices of variable reads that must tolerate an unset variable.
    ///
    /// `incr x` on a variable that does not exist creates it at zero, where
    /// `$x` refuses; both lower to the same read op on the same name, so the
    /// site is the only thing that separates them. `lower` pairs these with the
    /// chunk they belong to and [`crate::runtime`] answers fusevm's undef hook
    /// from that set.
    pub(crate) tolerant_reads: Vec<usize>,
    /// Op indices of the `Op::Add` / `Op::Sub` an `incr` lowered.
    ///
    /// `incr` words an operand refusal in its own terms — `expected integer but
    /// got "abc"` — where `expr` names the operator. The two lower to the same
    /// arithmetic on the same value, so only the site separates them, and the
    /// alternative was an extension op here, which costs every counted loop its
    /// trace. Read by the sited numeric hook in [`crate::runtime`].
    pub(crate) incr_sites: Vec<usize>,
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
    /// Names a `proc` outside the script's top level defines, from the previous
    /// pass. A call to one of them cannot be lowered to `Op::Call`: the name
    /// only starts answering once the defining code has run, so it resolves in
    /// the run-time command table instead. See [`compile`].
    pub(crate) runtime: HashSet<String>,
    /// The same, found during this pass — what the next one is given.
    pub(crate) seen_runtime: HashSet<String>,
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
    /// Emit a `ext_wide::DBG_LINE` marker before every command, which is what
    /// lets a debugger stop at one. Off for every ordinary compilation.
    pub(crate) debug: bool,
    /// Whether this script will run inside a *frame projection* — a nested
    /// script an `eval`, `uplevel`, `subst` or `apply` is running against a
    /// procedure activation's variables rather than against the interpreter's.
    ///
    /// The one thing it changes is how a `::`-qualified name is keyed. In a
    /// projected frame `$g` is the frame's local and `$::g` is the
    /// interpreter's variable — two variables — so the qualified spelling keeps
    /// its prefix and takes a name of its own
    /// (`crate::cmd_namespace::chunk_key`). Everywhere else the two are the same
    /// variable and must share one name, or a chunk that wrote through one
    /// spelling and read through the other would answer from a slot nothing had
    /// written.
    ///
    /// It is a property of the *evaluation*, not of the text, which is why it is
    /// part of the cache key in [`crate::cache::ChunkCache`]: the same script may
    /// be evaluated both ways.
    pub(crate) projected: bool,
    /// How many command substitutions enclose the command being compiled. A
    /// debugger stops before a statement, and a substitution is part of one.
    pub(crate) subst_depth: usize,
    /// Which name each frame slot was written as, per procedure body, published
    /// by [`Compiler::publish_slot_names`] where the body's scope is discarded.
    /// The half a built chunk was missing; see [`crate::cmd_scope`].
    pub(crate) slot_names: crate::cmd_scope::SlotNames,
    /// Global names an `upvar #0` *outside* a procedure bound to another global
    /// while the script was read. The same compile-time binding `Scope::aliases`
    /// is, for the scope that has no `Scope` — see [`crate::cmd_scope`].
    pub(crate) top_aliases: crate::cmd_scope::Aliases,
    /// Whether the failure now propagating is one the reference interpreter
    /// only reports when the command runs, set where the failure is *raised*
    /// rather than guessed from its wording.
    ///
    /// The three command-level classes are recognisable by their message
    /// ([`defers_to_run_time`]), but an expression's are not: `expr` refuses in
    /// a dozen wordings, and matching a list of prefixes would silently stop
    /// covering one the day it is reworded. So `expr::parse` and a body's own
    /// parse mark their failures here instead, and [`Compiler::command`] reads
    /// the mark. Cleared per command, and re-armed when a failure passes
    /// through a command that could not absorb it, so an enclosing one still
    /// can.
    pub(crate) deferrable: bool,
    /// Which namespace the code being lowered belongs to, and what `variable`
    /// has linked inside a procedure body. See `crate::cmd_namespace`.
    pub(crate) ns: crate::cmd_namespace::NsCtx,
}

impl Compiler {
    /// One compilation pass over the script, with the array names and the
    /// run-time procedure names the previous pass discovered.
    fn run(
        script: &Script,
        arrays: ArrayNames,
        runtime: HashSet<String>,
        debug: bool,
        projected: bool,
    ) -> Result<Compiler, CompileError> {
        let mut c = Compiler {
            b: ChunkBuilder::new(),
            tolerant_reads: Vec::new(),
            incr_sites: Vec::new(),
            depth: 0,
            loops: Vec::new(),
            line: 1,
            command_line: 1,
            arrays,
            seen_arrays: ArrayNames::new(),
            scope: None,
            procs: HashMap::new(),
            defined: HashSet::new(),
            runtime,
            seen_runtime: HashSet::new(),
            coros: HashSet::new(),
            catch_depth: 0,
            body_depth: 0,
            top_level: true,
            static_ctx: true,
            debug,
            projected,
            subst_depth: 0,
            deferrable: false,
            ns: crate::cmd_namespace::NsCtx::default(),
            slot_names: crate::cmd_scope::SlotNames::default(),
            top_aliases: crate::cmd_scope::Aliases::default(),
        };
        // Signatures are collected before anything is emitted so a procedure
        // may call one that the script defines further down, which is legal in
        // Tcl as long as the call is not reached first.
        crate::procs::prescan(&mut c.procs, script);
        // The same, for the procedures a `namespace eval` block defines, under
        // the qualified names they take.
        crate::cmd_namespace::prescan_script(&mut c.procs, script, "::");
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

    /// Lower a command that cannot succeed as code that fails when it runs.
    ///
    /// Tcl resolves a command name and checks its argument count at the moment
    /// the command is invoked, so a command that could never work costs a
    /// script nothing until control reaches it: `if {0} {incr}` prints nothing
    /// and exits 0, and `catch {nosuchcmd}` answers 1. Deciding either while
    /// compiling took the whole script down instead, which is one class and 113
    /// of the 162 cases the differential fuzzer reports.
    ///
    /// The arguments are evaluated and discarded before the error is raised,
    /// because Tcl substitutes every word of a command *before* dispatching on
    /// the first: `p [puts hi] extra` prints `hi` and then reports the argument
    /// count. Evaluating them is also what keeps a nested failure nested — a
    /// command substitution inside an argument lowers through this same path.
    ///
    /// Nothing here weakens a refusal. The message, its usage string and its
    /// line are the handler's own; the only change is when it fires. A command
    /// this frontend does not implement still refuses, loudly, on the line it
    /// was written — it simply refuses when reached, as the unknown command it
    /// is to a Tcl interpreter.
    fn defer(&mut self, msg: &str, args: &[Word]) -> Result<(), CompileError> {
        for arg in args {
            self.word(arg)?;
            self.emit(Op::Pop, -1);
        }
        self.push_str(msg);
        // Located, not plain: the line is the one the refusal carried when this
        // was decided while compiling, so a deferred failure still reports
        // `(file "…" line N)` — and tclsh locates its runtime errors too.
        self.emit(Op::ExtendedWide(ext_wide::ERROR_AT, self.command_line), -1);
        // Control has left; the value keeps the depth arithmetic honest, the
        // way `error` and `return` do.
        self.push_empty();
        Ok(())
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

    /// The same, for a failure the reference interpreter reports only when the
    /// command runs: parsing an expression, or parsing a body's own text.
    ///
    /// tclsh reaches neither until execution does — `if {0} {expr {a}}` and
    /// `if {0} {puts "unterminated}` both run to completion there — so a
    /// refusal here has to become code inside the branch rather than a verdict
    /// on the script. Marking it at the point it is raised is what lets
    /// [`Compiler::command`] tell it apart from a refusal that is genuinely the
    /// script's shape, such as an unbalanced brace, which tclsh reports too.
    pub(crate) fn deferrable_err(&mut self, msg: impl Into<String>) -> CompileError {
        self.deferrable = true;
        self.err(msg)
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
        // A qualified name is never a local. `TclLookupSimpleVar` only consults
        // the frame's compiled locals for a name with no `::` in it; anything
        // qualified goes to `TclGetNamespaceForQualName` and names a namespace
        // variable — `::x` being the root namespace's. Handing one a frame slot
        // instead made `proc f {} {return $::a}` answer the empty string for a
        // global the script had set, with no error to show for it (tclsh 9.0.4
        // answers the value).
        if name.contains("::") {
            return None;
        }
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

    /// The names the enclosing procedure body gave to `global`, as a Tcl list —
    /// `None` at a script's top level, where there is no frame.
    ///
    /// A nested script sees the frame it runs in and the globals that frame
    /// linked, and nothing else. The frame's own names come from the chunk at
    /// run time; these do not, because `global` is a statement about the body
    /// rather than about a slot.
    pub(crate) fn declared_globals(&self) -> Option<String> {
        let scope = self.scope.as_ref()?;
        let mut names: Vec<&str> = scope.globals.iter().map(String::as_str).collect();
        names.sort_unstable();
        Some(crate::list::join(&names))
    }

    // ── variables whose name the script computes ─────────────────────────
    //
    // `set $n 1` names a variable this compiler cannot resolve: the name is a
    // value. The four helpers below lower such an access to the ops that
    // resolve it when they run, which is what a Tcl interpreter does with every
    // variable access anyway. Each pushes the body's `global` declarations
    // first — see [`Compiler::declared_globals`] for why the run time cannot
    // recover them.
    //
    // Nothing here is a fallback in the sense of being weaker. A literal name
    // still resolves while compiling and still lowers to the same one or two
    // ops it always did; these run only for the access whose name is a value,
    // and they give it the answers tclsh gives.

    /// Push the `global`/`variable` declarations the enclosing body made, as the
    /// first operand every computed-name op takes. Empty at a script's top
    /// level, where there is no frame and every name is a global anyway.
    fn push_declared(&mut self) {
        let declared = self.declared_globals().unwrap_or_default();
        self.push_str(&declared);
    }

    /// Read the variable a computed name spells. `absent` is what an unset one
    /// answers — see [`ext::DYN_GET`] for why the choice belongs to the caller.
    pub(crate) fn dyn_get(&mut self, name: &Word, absent: Absent) -> Result<(), CompileError> {
        self.push_declared();
        self.word(name)?;
        self.emit(Op::Extended(ext::DYN_GET, absent as u8), -1);
        Ok(())
    }

    /// Store the value already on the stack into the variable a computed name
    /// spells, leaving the stack as it was found — the same contract
    /// [`Compiler::emit_set_var`] has.
    ///
    /// `[value]` → `[value, declared, name]`, which is the order [`ext::DYN_SET`]
    /// reads, so nothing has to be swapped past the value.
    pub(crate) fn dyn_store(&mut self, name: &Word) -> Result<(), CompileError> {
        self.push_declared();
        self.word(name)?;
        self.emit(Op::Extended(ext::DYN_SET, 0), -3);
        Ok(())
    }

    /// Open a read-modify-store on a computed name: `[]` → `[name, value]`,
    /// having evaluated the name **once**.
    ///
    /// `append $n x` and `incr $n` read the variable and write it back, and the
    /// word that spells its name may be a command substitution — `append [pick]
    /// x` runs `pick` once in tclsh, as every command's words are substituted
    /// once. So the name is evaluated, kept on the stack under the value, and
    /// consumed by [`Compiler::dyn_write_back`], rather than compiled twice.
    pub(crate) fn dyn_read_modify(&mut self, name: &Word, absent: Absent) -> Result<(), CompileError> {
        self.word(name)?; //             [name]
        self.emit(Op::Dup, 1); //        [name, name]
        self.push_declared(); //         [name, name, declared]
        self.emit(Op::Swap, 0); //       [name, declared, name]
        self.emit(Op::Extended(ext::DYN_GET, absent as u8), -1); // [name, value]
        Ok(())
    }

    /// Close one: `[name, result]` → `[result]`, having stored the result in the
    /// variable, which is the value these commands yield.
    pub(crate) fn dyn_write_back(&mut self) {
        self.emit(Op::Dup, 1); //        [name, result, result]
        self.emit(Op::Rot, 0); //        [result, result, name]
        self.push_declared(); //         [result, result, name, declared]
        self.emit(Op::Swap, 0); //       [result, result, declared, name]
        self.emit(Op::Extended(ext::DYN_SET, 0), -3); // [result]
    }

    /// Unset the variable a computed name spells.
    pub(crate) fn dyn_unset(&mut self, name: &Word, complain: bool) -> Result<(), CompileError> {
        self.push_declared();
        self.word(name)?;
        self.emit(Op::Extended(ext::DYN_UNSET, u8::from(complain)), -2);
        Ok(())
    }

    /// Whether the variable a computed name spells is set.
    pub(crate) fn dyn_exists(&mut self, name: &Word) -> Result<(), CompileError> {
        self.push_declared();
        self.word(name)?;
        self.emit(Op::Extended(ext::DYN_EXISTS, 0), -1);
        Ok(())
    }

    /// Where a variable lives, for an op that reaches it itself rather than
    /// through `GetVar` / `SetVar` — [`crate::cmd_list`]'s `lappend` is the one
    /// that does, so that it can extend the list in place.
    pub(crate) fn var_place(&mut self, name: &str) -> Place {
        // `upvar #0 other local` binds `local` to the global `other` for the
        // rest of the body. Every command reaches a variable through here, so
        // the link is made once and `set`, `$`, `unset`, `incr`, `lappend` and
        // `info exists` all follow it — see [`crate::cmd_scope`].
        if let Some(target) = self
            .scope
            .as_ref()
            .and_then(|s| s.aliases.get(name))
            .cloned()
        {
            return Place::Global(self.b.add_name(&target));
        }
        // An `upvar` whose target only the running script knows binds the name
        // to a *slot holding a link*, and every access goes through it. Ahead of
        // `slot_of` so the name is not also handed a plain slot.
        if let Some(slot) = self.scope.as_ref().and_then(|s| s.links.get(name)).copied() {
            return Place::Link(slot);
        }
        match self.slot_of(name) {
            Some(slot) => Place::Slot(slot),
            // The one hook namespaces need in the variable path: a name that
            // reaches the global table is the *namespace's* variable when the
            // code being lowered belongs to one. At the root namespace the key
            // is the name unchanged, so nothing that compiled before namespaces
            // existed compiles differently now. See `crate::cmd_namespace`.
            None => {
                let key = crate::cmd_namespace::global_key(self, name);
                // Outside a procedure an `upvar #0` binding is between two
                // globals, so it is followed here rather than at the top of this
                // function: the name is resolved in its namespace first, and the
                // binding is on the resolved spelling.
                // The binding is between two *variables*, so it is looked up by
                // the table key rather than by the spelling the code used: after
                // `upvar #0 a b`, `$b` and `$::b` are both the alias.
                let key = match self.top_aliases.get(crate::cmd_namespace::store_key(&key)) {
                    Some(target) => target.clone(),
                    None => key,
                };
                Place::Global(self.b.add_name(&key))
            }
        }
    }

    /// Where a variable lives, as one integer operand: the index shifted up by
    /// one with the frame-slot bit at the bottom.
    ///
    /// An op that *assigns* to a variable named by the script takes it this
    /// way rather than as a value, so the operand count stays the arity —
    /// `regexp`'s match variables and `gets`'s line variable are the two.
    /// [`crate::runtime::place_at`] reads it back.
    pub(crate) fn place_operand(&mut self, name: &str) -> i64 {
        let place = self.var_place(name);
        (place.frame_operand() << 1) | i64::from(place.in_frame())
    }

    /// Read a variable onto the stack.
    pub(crate) fn emit_get_var(&mut self, name: &str) {
        match self.var_place(name) {
            Place::Slot(slot) => self.emit(Op::GetSlot(slot), 1),
            Place::Global(idx) => self.emit(Op::GetVar(idx), 1),
            // A link has no native op: the descriptor in the slot has to be
            // followed, which only the frontend can do. See
            // [`crate::cmd_scope::link_get`].
            Place::Link(slot) => {
                self.push_str(name);
                self.emit(Op::LoadInt(i64::from(slot)), 1);
                self.emit(Op::Extended(ext::LINK_GET, 0), -1)
            }
        };
    }

    /// Pop the top of the stack into a variable.
    pub(crate) fn emit_set_var(&mut self, name: &str) {
        match self.var_place(name) {
            Place::Slot(slot) => self.emit(Op::SetSlot(slot), -1),
            Place::Global(idx) => self.emit(Op::SetVar(idx), -1),
            Place::Link(slot) => {
                self.push_str(name);
                self.emit(Op::LoadInt(i64::from(slot)), 1);
                self.emit(Op::Extended(ext::LINK_SET, 0), -3)
            }
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
    ///
    /// A `{*}` word is refused here rather than expanded: what it expands into
    /// is a *number of arguments*, which only the command assembling them can
    /// act on — [`Compiler::command`] routes such a command to
    /// [`Compiler::call_expanded`] before any handler sees its words. Reaching
    /// this with one means a word was used somewhere expansion has no meaning.
    pub(crate) fn word(&mut self, word: &Word) -> Result<(), CompileError> {
        if word.expand {
            return self.error("{*} argument expansion is only meaningful in a command's words");
        }
        self.word_value(word)
    }

    /// The same, for the one caller that has already accounted for expansion:
    /// the value of the word's text, with the `{*}` prefix's meaning left to
    /// [`ext::EXPAND_CALL`].
    pub(crate) fn word_value(&mut self, word: &Word) -> Result<(), CompileError> {
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
    pub(crate) fn target_of(&self, word: &Word) -> Result<Target, CompileError> {
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

    /// The name an array element's variable would be reported under: the whole
    /// spelling the script wrote, `a(i)` and not `a`, which is the name tclsh
    /// quotes in `can't read "a(i)": no such variable`. The index is only known
    /// at compile time when it is literal; otherwise the array's own name stands,
    /// which is all a diagnostic can honestly say about it.
    pub(crate) fn elem_report_name(name: &str, index: &[Part]) -> String {
        match index {
            [] => format!("{name}()"),
            [Part::Lit(text)] => format!("{name}({text})"),
            _ => name.to_string(),
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
        "uplevel",
        "apply",
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
        "scan",
        "break",
        "continue",
        "proc",
        "return",
        "global",
        "catch",
        "error",
        "throw",
        "subst",
        "array",
        "dict",
        "unset",
        "coroutine",
        "yield",
        "yieldto",
        "info",
        // ── the namespace block's own names ──────────────────────────────
        "namespace",
        // `variable` is listed once, here. Both the namespace block and the
        // scope block lower it; the namespace one wins, because it is the same
        // command with the namespace case filled in — see `Compiler::variable`.
        "variable",
        "rename",
        "source",
        "tcl_findLibrary",
        // ── cmd_after / cmd_scope (the event loop and the scope commands) ──
        "after",
        "update",
        "vwait",
        "uplevel",
        "upvar",
        "apply",
        "package",
        // ── cmd_binary ────────────────────────────────────────────────────
        "binary",
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
        // A `{*}` anywhere in the command — including on the name — is decided
        // when the command runs, so nothing below this point applies: there is no
        // name to dispatch on and no argument count to check. See
        // [`Compiler::call_expanded`].
        if cmd.words.iter().any(|w| w.expand) {
            return self.call_expanded(&cmd.words);
        }
        let name = self.literal_of(first, "command name")?.to_string();
        let args = &cmd.words[1..];

        // A failure that the reference interpreter only reports when the
        // command *runs* is lowered as code that raises it, so a command in a
        // branch that is never taken costs the script nothing — `if {0} {incr}`
        // prints nothing and exits 0 in tclsh, where refusing here took the
        // whole script down. See [`Compiler::defer`].
        //
        // The handler is asked first and only its verdict is reinterpreted, so
        // the wording, the usage string and the line all stay exactly what they
        // were. `mark` guards the one thing that would corrupt the chunk: a
        // handler that emitted ops before failing has no rollback, so its error
        // stays where it is.
        let mark = self.b.current_pos();
        let depth = self.depth;
        // Fresh per command: a nested one may have marked and absorbed a
        // failure of its own, and that verdict is not this command's.
        self.deferrable = false;
        let outcome = self.dispatch(&name, args);
        let marked = std::mem::take(&mut self.deferrable);
        match outcome {
            Err(e) if (defers_to_run_time(&e.msg) || marked) && self.b.current_pos() == mark => {
                self.depth = depth;
                self.defer(&e.msg, args)
            }
            // The failure is one that belongs at run time, but this command had
            // already emitted for it, so it cannot become code here. Re-arm the
            // mark: an enclosing command that has emitted nothing yet can still
            // absorb it, which is how `if {0} {puts "unterminated}` defers even
            // though the body's parse fails inside `if`'s own handler.
            Err(e) if marked => {
                self.deferrable = true;
                Err(e)
            }
            outcome => outcome,
        }
    }

    /// Lower one command, given its name and arguments.
    fn dispatch(&mut self, name: &str, args: &[Word]) -> Result<(), CompileError> {
        match name {
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
            "string" | "append" | "format" => self.cmd_string_family(name, args),
            "scan" => crate::cmd_scan::compile(self, args),
            "break" => self.cmd_loop_exit(args, true),
            "continue" => self.cmd_loop_exit(args, false),
            // `ns_proc` and `ns_global` are one-line wrappers in
            // `crate::cmd_namespace` that record which namespace the definition
            // or the declaration belongs to and then call the handler below.
            "proc" => self.ns_proc(args),
            "return" => self.cmd_return(args),
            "global" => self.ns_global(args),
            "catch" => self.cmd_catch(args),
            "error" => self.cmd_error(args),
            "throw" => self.cmd_throw(args),
            "subst" => crate::cmd_subst::compile(self, args),
            "array" => self.cmd_array(args),
            "dict" => self.cmd_dict(args),
            "unset" => self.cmd_unset(args),
            "coroutine" => self.cmd_coroutine(args),
            "yield" => self.cmd_yield(args),
            "yieldto" => self.cmd_yieldto(args),
            // ── the event loop and the scope commands ───────────────────
            // One block, so that the modules behind it merge as one change.
            // `info` moved here from `crate::coro`, which owned it when
            // `info coroutine` was the only subcommand; `crate::cmd_info`
            // still lowers that one to `crate::coro`'s own op.
            "info" => self.cmd_info(args),
            "after" => self.cmd_event_op(ext::AFTER, "after", args),
            "update" => self.cmd_event_op(ext::UPDATE, "update", args),
            "vwait" => self.cmd_event_op(ext::VWAIT, "vwait", args),
            "uplevel" => self.cmd_uplevel(args),
            "upvar" => self.cmd_upvar(args),
            "apply" => self.cmd_apply(args),
            // ── end of the block ────────────────────────────────────────
            // Asked before the Tk arm below, so that a `--tk` session — where
            // every unmatched name is lowered as a run-time lookup — still
            // gets this frontend's own `package` rather than looking for one
            // Tk never registered.
            "package" => crate::cmd_package::compile(self, args),
            "regexp" | "regsub" => crate::regexp::compile(self, name, args),
            // The channel ensemble. Ahead of the namespace block below for the
            // same reason every other builtin above is: a builtin name wins
            // over a namespace procedure of that name in this frontend.
            name if crate::cmd_channel::COMMANDS.contains(&name) => {
                crate::cmd_channel::compile(self, name, args)
            }
            // ── clock and the filesystem commands ────────────────────────
            // One block, as `regexp` above is: the name is claimed here and
            // the whole of the lowering lives in the module named. Absent
            // from `BUILTINS` for the same reason `regexp` is — these are not
            // names `proc` refuses. Ahead of the namespace block below, like
            // every other builtin.
            "clock" => crate::cmd_clock::compile(self, args),
            "file" | "glob" | "pwd" | "cd" => crate::cmd_file::compile(self, name, args),
            // ── end of the clock/file block ──────────────────────────────
            // ── the encoding ensemble ────────────────────────────────────
            // One arm, as `clock` above is: the name is claimed here and the
            // whole of the lowering — argument parsing included, because which
            // argument is an option is decided by their count — lives in
            // [`crate::cmd_encoding`]. Ahead of the namespace block below,
            // like every other builtin.
            "encoding" => crate::cmd_encoding::compile(self, args),
            // ── end of the encoding ensemble ─────────────────────────────
            // ── the binary ensemble ──────────────────────────────────────
            // One arm, as `encoding` above is: the whole of the lowering —
            // including which words are options — lives in
            // [`crate::cmd_binary`]. Ahead of the namespace block below, like
            // every other builtin.
            "binary" => crate::cmd_binary::compile(self, args),
            // ── end of the binary ensemble ───────────────────────────────
            // ── namespaces, `rename` and `source` ────────────────────────
            // One block, so that the module owning them merges as one hunk.
            // It sits here — after every builtin, before the procedures —
            // because a name written inside a namespace resolves to that
            // namespace's procedure before it resolves to a global one, which
            // is `TclGetNamespaceForQualName`'s two-step search. See
            // `crate::cmd_namespace`.
            "namespace" => self.cmd_namespace(args),
            "variable" => self.cmd_variable(args),
            "rename" => self.cmd_rename(args),
            "source" => self.cmd_source(args),
            "tcl_findLibrary" => self.cmd_find_library(args),
            other if self.ns_resolves(other).is_some() => {
                crate::cmd_namespace::call(self, other, args)
            }
            // ── end of the namespace block ───────────────────────────────
            // The command an inline `rust { ... }` block was rewritten into.
            name if name == crate::rust_ffi::COMPILE_COMMAND => self.cmd_rust_compile(args),
            // A coroutine's context command. Its name is refused to `proc`, so
            // there is never both a procedure and a coroutine to choose from.
            other if self.coros.contains(other) => self.call_coro(other, args),
            // A name some `proc` outside the script's top level defines. It
            // answers only once that `proc` has run, so the call resolves in
            // the run-time command table — even when a top-level `proc` of the
            // same name also exists, because in tclsh the later definition
            // wins and only run time knows which ran last (measured: `proc f
            // {} {return one}` then `if {1} {proc f {} {return two}}` then `f`
            // answers `two`).
            other if self.runtime.contains(other) => self.call_runtime(other, args),
            // A procedure the script defines shadows nothing built in: the
            // names above are refused to `proc` at its definition.
            other if self.procs.contains_key(other) => self.call_proc(other, args),
            // A function an inline `rust { ... }` block exported. Asked after
            // the procedures, so a Tcl procedure of the same name still wins —
            // a script's own definition is never shadowed by a library it
            // loaded.
            other if crate::rust_ffi::is_exported(other) => self.call_ffi(other, args),
            // A name no module claims: it is looked up in the interpreter's
            // run-time command table when the command runs, because that is the
            // only moment it can be known.
            //
            // Two things register a name there after this compiler has finished
            // with the script. A `proc` — in another chunk, or in a branch of
            // this one — is one; Tk is the other, which registers `button`,
            // `pack`, `wm` and the rest during `Tk_Init`, long after a script
            // saying `button .b` was compiled (see `crate::tk::dispatch`).
            // Nothing registered under the name by then and the op raises the
            // same `invalid command name`, on the same line, that the arm below
            // would have deferred — which is why this needs no feature gate and
            // costs a script that calls no such command nothing.
            other if !crate::cmd_list::COMMANDS.contains(&other) => self.call_runtime(other, args),
            // The list commands own the tail of the dispatch. Reached by name
            // rather than by trying them, so that `llength` with three arguments
            // stays a `wrong # args` on `llength` instead of becoming a lookup
            // for a command called `llength`.
            other => crate::cmd_list::compile(self, other, args),
        }
    }

    fn cmd_set(&mut self, args: &[Word]) -> Result<(), CompileError> {
        match args.len() {
            // `set $n` and `set $n v`: the variable's name is a value, so both
            // the name and the whole resolution belong to run time. Tcl reads
            // every variable that way; this compiler resolves the literal case
            // ahead of time and falls back here for the rest.
            1 | 2 if assoc::target_of(&args[0]).is_none() => {
                if args.len() == 1 {
                    return self.dyn_get(&args[0], Absent::Refuse);
                }
                self.word(&args[1])?;
                // `set` yields the value it assigned.
                self.emit(Op::Dup, 1);
                self.dyn_store(&args[0])
            }
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
    /// locals are frame slots instead, so inside a procedure body the op runs
    /// the script against a *projection* of the frame: the names the chunk
    /// recorded for it (`fusevm::Chunk::sub_slot_names`, published by
    /// [`crate::procs`]) become the table the script sees, and are read back
    /// into the slots afterwards. See [`crate::runtime`]'s `run_in_frame`.
    fn cmd_eval(&mut self, args: &[Word]) -> Result<(), CompileError> {
        if args.is_empty() {
            return self.error("wrong # args: should be \"eval arg ?arg ...?\"");
        }
        // Inside a procedure the script runs against that procedure's frame, so
        // the op needs to know which names the body linked to globals — the one
        // fact about the frame that is not in the frame. See [`ext::EVAL_FRAME`].
        if let Some(declared) = self.declared_globals() {
            let count = u8::try_from(args.len() + 1)
                .map_err(|_| self.err("too many arguments for \"eval\"".to_string()))?;
            self.push_str(&declared);
            for arg in args {
                self.word(arg)?;
            }
            self.emit(Op::Extended(ext::EVAL_FRAME, count), -(args.len() as i32));
            return Ok(());
        }
        let count = u8::try_from(args.len())
            .map_err(|_| self.err("too many arguments for \"eval\"".to_string()))?;
        for arg in args {
            self.word(arg)?;
        }
        self.emit(Op::Extended(ext::EVAL, count), 1 - args.len() as i32);
        Ok(())
    }

    /// `uplevel ?level? arg ?arg ...?`.
    ///
    /// The level word is optional and is told from a script by the rule
    /// `Tcl_GetFrame` uses: a first argument that reads as a level *and* is not
    /// the only argument is the level. It travels as an operand rather than being
    /// resolved here, because `uplevel $n {…}` is ordinary.
    fn cmd_uplevel(&mut self, args: &[Word]) -> Result<(), CompileError> {
        if args.is_empty() {
            return self.error("wrong # args: should be \"uplevel ?level? command ?arg ...?\"");
        }
        let declared = self.declared_globals().unwrap_or_default();
        let count = u8::try_from(args.len() + 1)
            .map_err(|_| self.err("too many arguments for \"uplevel\"".to_string()))?;
        // The declared globals, then every word as the script wrote it. Which
        // word is the level is not decided here: `uplevel $n {…}` is ordinary
        // Tcl, and tclsh reads the level off the substituted word.
        self.push_str(&declared);
        for arg in args {
            self.word(arg)?;
        }
        self.emit(Op::Extended(ext::UPLEVEL, count), 1 - count as i32);
        Ok(())
    }

    /// `apply lambdaExpr ?arg ...?`.
    fn cmd_apply(&mut self, args: &[Word]) -> Result<(), CompileError> {
        if args.is_empty() {
            return self.error("wrong # args: should be \"apply lambdaExpr ?arg ...?\"");
        }
        let count = u8::try_from(args.len())
            .map_err(|_| self.err("too many arguments for \"apply\"".to_string()))?;
        for arg in args {
            self.word(arg)?;
        }
        self.emit(Op::Extended(ext::APPLY, count), 1 - args.len() as i32);
        Ok(())
    }

    fn cmd_puts(&mut self, args: &[Word]) -> Result<(), CompileError> {
        // The channel forms go to `cmd_channel`; the two without one keep the
        // lowering they had, so a script that never names a channel is
        // unchanged. tclsh 9 has no third form: `puts stdout hi nonewline` is
        // `wrong # args` there, not the 8.x legacy spelling.
        let (newline, value) = match args {
            [v] => (true, v),
            [flag, v] if flag.as_literal() == Some("-nonewline") => (false, v),
            [chan, v] => return crate::cmd_channel::compile_puts(self, chan, v, true),
            [flag, chan, v] if flag.as_literal() == Some("-nonewline") => {
                return crate::cmd_channel::compile_puts(self, chan, v, false)
            }
            _ => {
                return self.error("wrong # args: should be \"puts ?-nonewline? ?channel? string\"")
            }
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
        let parsed = expr::parse(&text).map_err(|e| self.deferrable_err(e.msg))?;
        // Arithmetic normalizes nothing: the result stays the value the VM
        // computed — an integer, a double, a boolean — and Tcl's string form is
        // applied wherever one is asked for (`ext::PUTS`, `ext::STR_CMP`, the
        // word concatenation above, `crate::runtime::tcl_str`). That is what
        // keeps a counted loop free of extension ops, which is what fusevm's
        // JIT and its ahead-of-time compiler need in order to lower one.
        //
        // An expression whose result could still be a *string* is the one that
        // pays, because `expr {$x}` answers the number `x` spells rather than
        // the text that spelt it. `yields_number` is the same static test the
        // boolean conversion uses, and it is false only where no arithmetic
        // happened.
        // tclsh will not answer with a NaN: `expr {nan}` is `domain error:
        // argument not in valid range`, the same message a *computed* NaN gets
        // from the arithmetic that produced it. A literal is the one spelling
        // that reaches the result without passing through an operation, so it
        // is the one the operations cannot catch.
        //
        // Raised when the expression runs rather than while it is read, because
        // a command inside a branch that never executes is not an error in
        // tclsh — the same rule the deferred failures follow.
        if matches!(&parsed, Expr::Float(v, _) if v.is_nan()) {
            self.push_str("domain error: argument not in valid range");
            self.emit(Op::Extended(ext::ERROR, 0), -1);
            // Control has left; the value keeps the depth arithmetic honest.
            self.push_empty();
            return Ok(());
        }
        self.expr(&parsed)?;
        if !Self::yields_number(&parsed) {
            self.emit(Op::Extended(ext::CANON, 0), 0);
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
        // `incr $v` resolves its variable when it runs, for the reason `set $v`
        // does. The read tolerates absence there too — `incr` on a variable
        // that does not exist creates it at zero.
        let Some(target) = assoc::target_of(name) else {
            self.dyn_read_modify(name, Absent::Zero)?;
            match by {
                Some(w) => self.word(w)?,
                None => {
                    self.emit(Op::LoadInt(1), 1);
                }
            }
            self.incr_sites.push(self.b.current_pos());
            self.emit(Op::Add, -1);
            self.dyn_write_back();
            return Ok(());
        };
        let name = match target {
            Target::Scalar(name) => name,
            Target::Elem { name, index } => return self.elem_incr(&name, &index, by),
        };
        // `incr` on a variable that does not exist creates it at zero, so this
        // read tolerates absence where `$x` refuses it. Recorded by site,
        // because the two are the same op on the same name; a guarded read
        // emits more than one op and owns its own diagnostic, so only the bare
        // single-op read is marked.
        if let Place::Link(slot) = self.var_place(&name) {
            // A linked name's read is not a single op, so the site-keyed
            // tolerance below cannot mark it: the op carries the flag instead.
            self.push_str(&name);
            self.emit(Op::LoadInt(i64::from(slot)), 1);
            self.emit(Op::Extended(ext::LINK_GET, 1), -1);
        } else {
            let read_at = self.b.current_pos();
            self.scalar_get(&name);
            if self.b.current_pos() == read_at + 1 {
                self.tolerant_reads.push(read_at);
            }
        }
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
        self.incr_sites.push(self.b.current_pos());
        self.emit(Op::Add, -1);
        self.emit(Op::Dup, 1);
        self.emit_set_var(&name);
        Ok(())
    }

    /// `if expr ?then? body ?elseif expr ?then? body ...? ?else? ?body?`.
    ///
    /// Ported from `Tcl_IfObjCmd`, whose grammar is looser than the synopsis in
    /// `if(n)` suggests in one way that matters: **the `else` keyword is
    /// optional**. What stands after the last body is the else script whatever
    /// it says, so `if {$x} {a} {b}` — a form ordinary Tcl is written in — is
    /// legal, and a stray word there is `extra words after "else" clause`
    /// rather than a complaint about the word itself. Both measured against
    /// tclsh 9.0.3.
    ///
    /// The two arity diagnostics quote a *word*, which is the one detail this
    /// cannot always reproduce: tclsh quotes the substituted value and the
    /// compiler has only the source. A literal word is quoted as written; a
    /// word that substitutes falls back to the keyword that introduced the
    /// clause. Recorded in BUGS.md.
    fn cmd_if(&mut self, args: &[Word]) -> Result<(), CompileError> {
        // Read the whole command before emitting anything, so an arity refusal
        // can be deferred the way tclsh's is: `Tcl_IfObjCmd` reaches every one
        // of them while running, so `catch {if {1} {a} else {b} extra}` is 1
        // there and an `if` nobody executes costs a script nothing.
        let plan = match parse_if(args) {
            Ok(plan) => plan,
            Err(msg) => return self.defer(&msg, args),
        };

        let mut end_jumps = Vec::new();
        let branch_depth = self.depth;
        for (cond, body) in &plan.branches {
            self.expr_word(cond)?;
            let jump_false = self.emit(Op::JumpIfFalse(usize::MAX), -1);
            self.body(body)?;
            end_jumps.push(self.emit(Op::Jump(usize::MAX), 0));
            let else_start = self.b.current_pos();
            self.b.patch_jump(jump_false, else_start);
            // Each branch is compiled at the same entry depth.
            self.depth = branch_depth;
        }
        match plan.otherwise {
            Some(body) => self.body(body)?,
            // No else: the value of a taken-nowhere `if` is empty.
            None => self.push_empty(),
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
        let script = self.body_of(body)?;
        self.rotated_loop(|c| c.emit_body(&script), |_| Ok(()), |c| c.expr_word(cond))?;
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
            names.extend(vars);
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
        let script = self.body_of(body)?;
        let taken: Vec<String> = names.iter().rev().cloned().collect();
        self.rotated_loop(
            |c| {
                c.emit(Op::Extended(ext::FOREACH_TAKE, width), i32::from(width));
                for name in &taken {
                    c.store_named(name)?;
                }
                c.emit_body(&script)
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
        let code = if is_break {
            crate::runtime::TCL_BREAK
        } else {
            crate::runtime::TCL_CONTINUE
        };
        // No loop in this chunk to jump to, or one outside a `catch` this exit
        // is written inside: either way the exit is a *return code* leaving the
        // command, which is what it is in Tcl anyway. The enclosing `catch`
        // reports it, a loop region absorbs it, and nothing at all leaves it to
        // be reported as `invoked "break" outside of a loop`.
        let ctx = self.loops.last();
        if ctx.is_none_or(|c| c.catch_depth != self.catch_depth) {
            self.push_empty();
            self.emit(Op::LoadInt(i64::from(code)), 1);
            self.emit(Op::LoadInt(0), 1);
            self.emit(Op::Extended(ext::RAISE, 0), -3);
            self.push_empty();
            return Ok(());
        }
        let ctx = ctx.expect("a loop context, checked above");
        // Discard whatever this iteration pushed before jumping, so the exit
        // point sees the depth it was compiled for.
        let before = self.depth;
        let surplus = before.saturating_sub(ctx.depth);
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
        // Those pops are a run-time effect of a path that leaves, so they must
        // not follow the enclosing command into the compiler's model. `break`
        // inside a word being built — `puts [list a [break]]` — sits above the
        // loop's entry depth, and charging its pops to the model would leave the
        // enclosing command short by exactly that much: its next negative delta
        // then underflows, which is the `rotated loop body is unbalanced`
        // assertion. Restore the depth the enclosing command was compiled
        // against, then give it the one value it is waiting for.
        self.depth = before;
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
        // Two trampolines the loop region resumes at, jumped over on the way
        // in. A `break` or `continue` written in this loop's own body is a
        // direct jump and never reaches them; one that arrives as a raised
        // return code — from a nested script, or from a procedure that
        // returned `-code break` — has only an op index to be sent to, and
        // these are the two indices that mean "leave" and "next iteration".
        // They are ordinary jumps, patched with the real targets below.
        let over_tramps = self.emit(Op::Jump(usize::MAX), 0);
        let brk_tramp = self.emit(Op::Jump(usize::MAX), 0);
        let cont_tramp = self.emit(Op::Jump(usize::MAX), 0);
        let region = self.b.current_pos();
        self.b.patch_jump(over_tramps, region);
        self.emit(Op::LoadInt(brk_tramp as i64), 1);
        self.emit(Op::LoadInt(cont_tramp as i64), 1);
        self.emit(Op::Extended(ext::LOOP_ENTER, 0), -2);

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
        // The region closes here, so the trampolines can point at the two
        // places a raised code should land: the loop's exit and its step.
        self.b.patch_jump(brk_tramp, end);
        self.b.patch_jump(cont_tramp, step_at);
        self.emit(Op::Extended(ext::LOOP_LEAVE, 0), 0);
        Ok(())
    }

    /// A control-flow body: braced text compiled in place.
    ///
    /// A body whose own text will not parse becomes a raise standing where its
    /// code would have stood, because that is where the reference interpreter
    /// reports it: `if {0} {puts "unterminated}` runs to completion in tclsh,
    /// and `if {[puts hi; expr 0]} {puts "unterminated}` prints `hi` first —
    /// the condition is evaluated, and only entering the body fails. Nothing of
    /// the body has been emitted when its parse fails, so the substitution is
    /// exact rather than a rollback.
    ///
    /// An unbalanced *brace* never reaches here: brace counting is how the
    /// enclosing script delimits this word at all, so tclsh reports that one
    /// eagerly too, and so does [`crate::parser`].
    pub(crate) fn body(&mut self, word: &Word) -> Result<(), CompileError> {
        match self.body_script(word) {
            Ok(script) => self.nested_value(&script),
            Err(e) if std::mem::take(&mut self.deferrable) => self.raise_at_run_time(&e.msg),
            Err(e) => Err(e),
        }
    }

    /// Lower a failure as the only thing a stretch of code does: push its
    /// message and raise it, located at the command it belongs to.
    ///
    /// The same shape [`Compiler::defer`] ends with, minus the arguments — a
    /// body has none to evaluate first.
    pub(crate) fn raise_at_run_time(&mut self, msg: &str) -> Result<(), CompileError> {
        self.push_str(msg);
        self.emit(Op::ExtendedWide(ext_wide::ERROR_AT, self.command_line), -1);
        self.push_empty();
        Ok(())
    }

    pub(crate) fn body_script(&mut self, word: &Word) -> Result<Script, CompileError> {
        let text = self.literal_of(word, "script body")?;
        crate::parser::parse(text).map_err(|e| self.deferrable_err(e.msg))
    }

    /// A body, held in whichever of its two states it is in: parsed, or known
    /// to fail the moment it is entered.
    ///
    /// A loop, a `switch` arm and a procedure all place their body somewhere
    /// control may never reach, so a body that will not parse cannot be allowed
    /// to fail the command that owns it — `while {0} {puts "unterminated}` runs
    /// to completion in tclsh. Carrying the failure this far and emitting it
    /// *as* the body is what puts the raise where the reference interpreter
    /// puts it.
    pub(crate) fn body_of(&mut self, word: &Word) -> Result<Body, CompileError> {
        match self.body_script(word) {
            Ok(script) => Ok(Body::Script(script)),
            Err(e) if std::mem::take(&mut self.deferrable) => Ok(Body::Deferred(e.msg)),
            Err(e) => Err(e),
        }
    }

    /// Emit a body for its effect, leaving the stack as it was found.
    pub(crate) fn emit_body(&mut self, body: &Body) -> Result<(), CompileError> {
        match body {
            Body::Script(script) => self.nested_effect(script),
            Body::Deferred(msg) => {
                let msg = msg.clone();
                self.raise_at_run_time(&msg)?;
                self.emit(Op::Pop, -1);
                Ok(())
            }
        }
    }

    /// Emit a body for its value.
    pub(crate) fn emit_body_value(&mut self, body: &Body) -> Result<(), CompileError> {
        match body {
            Body::Script(script) => self.nested_value(script),
            Body::Deferred(msg) => {
                let msg = msg.clone();
                self.raise_at_run_time(&msg)
            }
        }
    }

    /// A word used as a condition: its text is an expression, and its value has
    /// to be a Tcl boolean.
    pub(crate) fn expr_word(&mut self, word: &Word) -> Result<(), CompileError> {
        let text = self.literal_of(word, "condition")?.to_string();
        let parsed = expr::parse(&text).map_err(|e| self.deferrable_err(e.msg))?;
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
        if !Self::yields_number(e) || Self::can_be_nan(e) {
            self.emit(Op::Extended(ext::BOOL, 0), 0);
        }
        Ok(())
    }

    /// Whether this expression's value could be a NaN, which a condition has to
    /// refuse — `if {nan}` is `floating point value is Not a Number` in tclsh,
    /// not a taken branch.
    ///
    /// The VM's own truthiness has no opinion about NaN, so the check lives in
    /// [`ext::BOOL`], and an extension op in a loop's test would cost that loop
    /// its JIT trace. This is what keeps it off the loops that matter: an
    /// expression that yields an *integer* cannot be a NaN, and a counted
    /// loop's test is a comparison, which does. `while {$i < $n}` is therefore
    /// untouched, while `if {nan}` and `expr {nan ? 1 : 2}` reach the check.
    fn can_be_nan(e: &Expr) -> bool {
        if Self::yields_integer(e) {
            return false;
        }
        match e {
            // A double literal is a NaN only if it is spelled as one; `1.5` and
            // `inf` are ordinary values.
            Expr::Float(v, _) => v.is_nan(),
            // Sign does not make a NaN, and does not unmake one.
            Expr::Unary(UnOp::Plus | UnOp::Neg, operand) => Self::can_be_nan(operand),
            Expr::Ternary(_, then, other) => Self::can_be_nan(then) || Self::can_be_nan(other),
            // Arithmetic can reach `inf - inf` from operands that are not NaN
            // themselves, a substituted operand could be anything, and a math
            // function could answer with one.
            _ => true,
        }
    }

    /// Whether this expression's value is necessarily a number, whatever the
    /// variables in it hold — which is what decides whether a condition needs
    /// [`ext::BOOL`] at all.
    ///
    /// Every operator lowers to an op that answers with an `Int`, a `Float` or a
    /// `Bool`; the exceptions are an operand that is substituted text
    /// ([`Expr::Subst`]) and unary `+`, which is the identity and so passes its
    /// operand's value straight through.
    /// An operand of an *arithmetic* operator.
    ///
    /// Ordinarily this is [`Compiler::expr`], which pushes a double literal as
    /// `Op::LoadFloat` — the number, with the spelling dropped. Tcl quotes a
    /// refused operand by its string representation, and for a literal that is
    /// what the script wrote: `expr {1e10 % 3}` names `1e10`, not the
    /// `10000000000.0` the number prints as, and `expr {nan + 1}` names `nan`,
    /// not `NaN`. An operand read from a variable is already a string and was
    /// always quoted right; only a literal was not.
    ///
    /// So a double literal whose spelling the formatter would not reproduce is
    /// pushed as that spelling instead, and the numeric hook parses it back on
    /// the way into the operation. The cost is paid only where it cannot be
    /// seen: a literal already in canonical form — `1.5`, `0.5`, `-0.0`, which
    /// is almost every one a script writes — is still `Op::LoadFloat` and still
    /// native, and a non-finite one is refused by the operator anyway. The test
    /// is the formatter itself rather than a list of shapes, because the shapes
    /// are not obvious: `1.0e-7` looks canonical and is not, since
    /// `format_double` gives `1e-7` for it.
    fn numeric_operand(&mut self, e: &Expr) -> Result<(), CompileError> {
        if let Expr::Float(v, text) = e {
            if !v.is_finite() || crate::runtime::format_double(*v) != **text {
                self.push_str(text);
                return Ok(());
            }
        }
        self.expr(e)
    }

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
            // Every math function answers with a number — an integer of some
            // width, a double, or the 1/0 of a classification.
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
    /// Whether this expression can hand an *infinity* to the operator above it,
    /// which is the only way `+`, `-` or `*` can make a NaN: adding, subtracting
    /// or multiplying two finite doubles overflows to ±Inf at worst, never to a
    /// NaN. tclsh refuses a NaN *result* (`expr {inf-inf}` is `domain error:
    /// argument not in valid range`), so where this answers true the operation
    /// is followed by [`ext::CANON`], which carries that refusal.
    ///
    /// Only a literal is counted. A script that spells `inf` or `nan` is caught;
    /// an infinity a script *computed* earlier and stored in a variable is not,
    /// and reusing one is the documented hole in BUGS.md. Counting substitutions
    /// would put an extension op in every arithmetic loop, which costs the
    /// tracing JIT the whole loop — measured, not assumed.
    fn may_be_non_finite(e: &Expr) -> bool {
        match e {
            Expr::Float(f, _) => !f.is_finite(),
            Expr::Int(_, _) | Expr::Subst(_) => false,
            Expr::Unary(_, operand) => Self::may_be_non_finite(operand),
            Expr::Binary(BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div, a, b) => {
                Self::may_be_non_finite(a) || Self::may_be_non_finite(b)
            }
            // Everything else answers a number of its own — a comparison is 1 or
            // 0, a shift and the bitwise operators are integers — or is refused.
            Expr::Binary(_, _, _) => false,
            Expr::Ternary(_, then, other) => {
                Self::may_be_non_finite(then) || Self::may_be_non_finite(other)
            }
            // A math function answers with whatever the C library did, and an
            // infinity is one of the answers: `expr {pow(10,400) * 0}` is
            // `domain error: argument not in valid range` in tclsh 9.0.4,
            // which is [`ext::CANON`]'s refusal and needs this to be true.
            Expr::Call(_, _) => true,
        }
    }

    /// Whether this expression's value provably fits a machine `i64`, which is
    /// a stronger question than [`Self::yields_integer`] and the one the VM's
    /// native bitwise ops need answered.
    ///
    /// Since integers promote, "is an integer" no longer means "is an
    /// `i64`": `1 << 100` is an integer and `expr {(1 << 100) & (1 << 100)}` is
    /// exact in tclsh, but fusevm's `Op::BitAnd` reads a promoted value through
    /// `Value::to_int` and would answer 0. So the native op is emitted only for
    /// operands that cannot leave the word, and everything else goes to the
    /// extension op that carries the wide arithmetic.
    ///
    /// What cannot leave the word: a literal that already fits, a comparison or
    /// a logical operator (1 or 0), `& | ^` of two such (no bit is created),
    /// and `>>` of one (a right shift only shrinks). `+ - * **` and `<<` all
    /// grow, `/` grows on `i64::MIN / -1`, and unary `-` grows on `i64::MIN`.
    fn fits_machine_int(e: &Expr) -> bool {
        match e {
            Expr::Int(_, _) => true,
            Expr::Float(_, _) | Expr::Subst(_) => false,
            Expr::Unary(UnOp::Not, _) => true,
            Expr::Unary(_, _) => false,
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
                | BinOp::Or,
                _,
                _,
            ) => true,
            Expr::Binary(BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor, a, b) => {
                Self::fits_machine_int(a) && Self::fits_machine_int(b)
            }
            Expr::Binary(BinOp::Shr, a, _) => Self::fits_machine_int(a),
            Expr::Binary(_, _, _) => false,
            Expr::Ternary(_, then, other) => {
                Self::fits_machine_int(then) && Self::fits_machine_int(other)
            }
            Expr::Call(_, _) => false,
        }
    }

    fn yields_integer(e: &Expr) -> bool {
        match e {
            Expr::Int(_, _) => true,
            Expr::Float(_, _) | Expr::Subst(_) => false,
            // `!` and the comparisons answer 1 or 0 whatever they are given.
            Expr::Unary(UnOp::Not, _) => true,
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
                | BinOp::Or,
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

    // ── expressions ──────────────────────────────────────────────────────

    pub(crate) fn expr(&mut self, e: &Expr) -> Result<(), CompileError> {
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
            Expr::Unary(UnOp::Not, operand)
                if !Self::yields_number(operand) || Self::can_be_nan(operand) =>
            {
                // `numeric_operand` rather than `expr`: `!` refuses a NaN by
                // naming it, and tclsh names an operand by what the script
                // wrote — `!nan` says `"nan"`, not the `NaN` the number prints
                // as. A literal the formatter reproduces exactly is unaffected
                // and stays a native `LoadFloat`.
                self.numeric_operand(operand)?;
                self.emit(Op::Extended(ext::BOOL, 1), 0);
                Ok(())
            }
            // `~` wants an integer, and fusevm's `Op::BitNot` takes anything —
            // `expr {~1.5}` answered -2 where tclsh refuses the operand. Native
            // only when the operand is provably an integer.
            Expr::Unary(UnOp::BitNot, operand) if !Self::fits_machine_int(operand) => {
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
                // Sign refuses a NaN by naming the operand the script wrote —
                // `expr {-nan}` is `cannot use non-numeric floating-point value
                // "nan" as operand of "-"` — so the operand carries its
                // spelling for the same reason an arithmetic one does.
                self.numeric_operand(operand)?;
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
                let integral = Self::fits_machine_int(a) && Self::fits_machine_int(b);
                self.numeric_operand(a)?;
                self.numeric_operand(b)?;
                let native = match op {
                    BinOp::Add => Some(Op::Add),
                    BinOp::Sub => Some(Op::Sub),
                    BinOp::Mul => Some(Op::Mul),
                    // No `integral` case for the shifts: fusevm masks the
                    // distance to six bits, so `1 << 64` answered 0 where tclsh
                    // promotes, and `1 << -1` answered 0 where tclsh reports
                    // "negative shift argument". Neither the distance nor the
                    // overflow is knowable from the operands' shapes, so both
                    // shifts are always the extension op.
                    BinOp::BitAnd if integral => Some(Op::BitAnd),
                    BinOp::BitOr if integral => Some(Op::BitOr),
                    BinOp::BitXor if integral => Some(Op::BitXor),
                    BinOp::Lt => Some(Op::NumLt),
                    BinOp::Gt => Some(Op::NumGt),
                    BinOp::Le => Some(Op::NumLe),
                    BinOp::Ge => Some(Op::NumGe),
                    BinOp::Eq => Some(Op::NumEq),
                    BinOp::Ne => Some(Op::NumNe),
                    // `StrLt` … `StrNe` are absent on purpose: the always-string
                    // comparisons are lowered by the arm above, which compares
                    // Tcl's string form of each operand rather than the VM's.
                    _ => None,
                };
                match native {
                    Some(op) => {
                        // `inf - inf` is a NaN, and tclsh refuses one. The
                        // native op cannot, so an operation a script spelt an
                        // infinity into is followed by the op that does.
                        let checks_nan = matches!(op, Op::Add | Op::Sub | Op::Mul)
                            && (Self::may_be_non_finite(a) || Self::may_be_non_finite(b));
                        self.emit(op, -1);
                        if checks_nan {
                            self.emit(Op::Extended(ext::CANON, 0), 0);
                        }
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
            Expr::Call(name, args) => crate::expr_math::compile(self, name, args),
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
/// Whether a word has the shape of an `uplevel` / `upvar` level: `#n`, or a
/// bare integer. Only the shape — whether the level *exists* is a run-time
/// question, and answering it here would refuse `uplevel 2 …` while compiling a
/// procedure that is only ever called two deep.
pub(crate) fn looks_like_a_level(word: &str) -> bool {
    match word.strip_prefix('#') {
        Some(rest) => !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()),
        // `Tcl_GetIntFromObj`, which is what `TclObjGetFrame` reads a bare level
        // word with, accepts a sign: `uplevel +1 {…}` is level 1 and
        // `uplevel -1 {…}` is `bad level "-1"` — a level word that resolves to
        // nothing, not a script. `1.5` is neither, and runs as a script.
        None => {
            let digits = word.strip_prefix(['+', '-']).unwrap_or(word);
            !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
        }
    }
}

pub(crate) fn literal_value(text: &str) -> Value {
    if let Ok(i) = text.parse::<i64>() {
        if i.to_string() == text {
            return Value::Int(i);
        }
    }
    Value::Str(std::sync::Arc::new(text.to_string()))
}
