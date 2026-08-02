//! `proc`, `return` and `global` — procedures and their scope.
//!
//! ## How a Tcl procedure maps onto fusevm's calling convention
//!
//! A procedure's body is compiled into the enclosing chunk's op stream, behind
//! a `Op::Jump` that steps over it, and registered with
//! `ChunkBuilder::add_sub_entry(name_idx, entry_ip)`. `Op::Call(name_idx, n)`
//! resolves that entry through `Chunk::find_sub`, pushes a `Frame` whose
//! `stack_base` is `stack.len() - n`, and jumps to it; the `n` argument values
//! the call site pushed are therefore sitting at the base of the new frame.
//! The prologue moves them into the frame's slots with `Op::SetSlot(n-1)` down
//! to `Op::SetSlot(0)` — reverse order, since the last argument is on top.
//! `Op::ReturnValue` pops the frame, truncates the stack back to `stack_base`
//! (which discards anything the body left behind) and pushes the result, so a
//! call is a net `1 - n` on the caller's stack depth.
//!
//! Frame slots are what give a procedure its own variables: fusevm allocates a
//! fresh `Vec` of them per call, so locals neither leak into the globals nor
//! collide with an outer activation of the same recursive procedure. Only
//! names declared with `global` bypass the slots, compiling to
//! `Op::GetVar`/`Op::SetVar`, which address the VM's global table directly.
//!
//! Because the count of formal parameters is fixed but a call may pass fewer,
//! the *call site* does the adapting: it pushes a constant for each defaulted
//! parameter the caller omitted and folds any surplus arguments into the
//! variadic `args` list. The callee then always receives exactly one value per
//! formal parameter, which is what makes the fixed prologue above correct.
//!
//! ## A `proc` that is not at the script's top level
//!
//! Everything above needs the callee's name and signature while the *caller* is
//! being compiled. A `proc` inside an `if`, a loop or another procedure's body
//! does not supply them: in tclsh the command starts answering when the
//! defining code runs, and not before. Measured against tclsh 9.0.4:
//!
//! ```text
//! if {0} {proc f {} {}} ; f       → invalid command name "f"
//! if {1} {proc f {} {}} ; f       → runs
//! proc f {} {return one}
//! if {1} {proc f {} {return two}} ; f   → two
//! if {1} {proc f {a b} {}} ; f 1   → wrong # args: should be "f a b"
//! proc outer {} {proc inner {} {…}}     → `inner` exists only after `outer` ran
//! ```
//!
//! So the body is compiled where it stands — behind a jump, with the same
//! prologue, reaching the same tiers — and the *name* is bound separately, by
//! [`ext::PROC_DEFINE`], when control reaches the `proc` command. A call to a
//! name bound that way is [`ext::DYN_CALL`], which resolves in the run-time
//! command table and does the argument adapting the compiler would have done.
//!
//! Compile-time resolution is untouched by any of this. A `proc` at the
//! script's top level still registers a `Chunk` sub-entry, its calls are still
//! `Op::Call(name_idx, n)` with the actuals arranged by the call site, and
//! `bench/counted_loop_proc.tcl` still trace-compiles. The run-time path is
//! reached only for a name whose definition is conditional, which the compiler
//! learns in its first pass (see [`crate::compiler::compile`]).

use std::collections::HashMap;

use fusevm::{Frame, Op, Value, VM};

use crate::compiler::{ext, literal_value, CompileError, Compiler, Scope};
use crate::list;
use crate::parser::{Script, Word};
use crate::runtime::{to_tcl_string, Shared, TclError};

/// One formal parameter of a procedure.
#[derive(Clone, PartialEq, Eq)]
pub struct Param {
    pub name: String,
    /// The value used when the caller omits this argument.
    pub default: Option<String>,
}

/// A procedure's formal argument list.
#[derive(Clone, PartialEq, Eq)]
pub struct Signature {
    /// Every formal parameter, including the trailing `args` when `variadic`.
    pub params: Vec<Param>,
    /// The last formal is named `args`, so surplus actuals collect into it.
    pub variadic: bool,
    /// The fewest actual arguments the procedure accepts. `proc(n)`: a
    /// defaulted parameter followed by a non-defaulted one is required all the
    /// same, so this is one past the last parameter without a default.
    pub required: usize,
}

impl Signature {
    /// Formal parameters that take one actual argument each — everything but
    /// the trailing `args`.
    pub fn fixed(&self) -> usize {
        self.params.len() - usize::from(self.variadic)
    }

    /// The usage line Tcl reports for a call with the wrong argument count.
    pub fn usage(&self, name: &str) -> String {
        let mut out = name.to_string();
        for (i, p) in self.params.iter().enumerate() {
            out.push(' ');
            if self.variadic && i + 1 == self.params.len() {
                out.push_str("?arg ...?");
            } else if p.default.is_some() {
                out.push('?');
                out.push_str(&p.name);
                out.push('?');
            } else {
                out.push_str(&p.name);
            }
        }
        out
    }
}

/// Parse a `proc` argument specifier: a list whose elements are either a
/// parameter name or a two-element `{name default}` list.
pub fn parse_signature(proc_name: &str, spec: &str) -> Result<Signature, String> {
    let mut params: Vec<Param> = Vec::new();
    for element in list::split(spec)? {
        let fields = list::split(&element)?;
        let param = match fields.as_slice() {
            [name] => Param {
                name: name.clone(),
                default: None,
            },
            [name, default] => Param {
                name: name.clone(),
                default: Some(default.clone()),
            },
            [] => return Err("argument with no name".to_string()),
            _ => {
                return Err(format!(
                    "too many fields in argument specifier \"{element}\""
                ))
            }
        };
        if param.name.is_empty() {
            return Err("argument with no name".to_string());
        }
        if param.name.ends_with(')') && param.name.contains('(') {
            return Err(format!(
                "formal parameter \"{}\" is an array element",
                param.name
            ));
        }
        if params.iter().any(|p| p.name == param.name) {
            return Err(format!(
                "procedure \"{proc_name}\" has argument \"{}\" defined twice",
                param.name
            ));
        }
        params.push(param);
    }

    let variadic = params.last().is_some_and(|p| p.name == "args");
    let fixed = params.len() - usize::from(variadic);
    let required = params[..fixed]
        .iter()
        .rposition(|p| p.default.is_none())
        .map_or(0, |i| i + 1);

    Ok(Signature {
        params,
        variadic,
        required,
    })
}

/// Collect the signature of every procedure the script's own commands define,
/// before any of them is compiled. A procedure body may then call one defined
/// further down — which Tcl allows, since the name is only looked up when the
/// call runs. Malformed definitions are skipped here and reported when the
/// `proc` command itself is compiled.
pub fn prescan(procs: &mut HashMap<String, Signature>, script: &Script) {
    for cmd in &script.commands {
        let [head, name, spec, _body] = cmd.words.as_slice() else {
            continue;
        };
        if head.as_literal() != Some("proc") {
            continue;
        }
        let (Some(name), Some(spec)) = (name.as_literal(), spec.as_literal()) else {
            continue;
        };
        if let Ok(sig) = parse_signature(name, spec) {
            procs.insert(name.to_string(), sig);
        }
    }
}

// ── the run-time command table ───────────────────────────────────────────

/// A procedure whose name was bound while the script was running.
///
/// The entry point is an op index, which only means anything inside the chunk
/// it was taken from — so the chunk it came from is recorded with it. A nested
/// `eval` runs a chunk of its own; a procedure it defined has an entry point
/// that indexes nothing here, and jumping to it would run whichever op happens
/// to sit at that index. `Chunk::op_hash` is `#[serde(skip)]`, so it is 0 for
/// every chunk an ahead-of-time binary deserialized: the op count is kept
/// alongside it so that identity is not resting on one field that can be zero.
#[derive(Clone)]
pub(crate) struct RuntimeProc {
    chunk: ChunkKey,
    entry: usize,
    sig: Signature,
}

type ChunkKey = (u64, usize);

fn chunk_key(chunk: &fusevm::Chunk) -> ChunkKey {
    (chunk.op_hash, chunk.ops.len())
}

/// [`ext::PROC_DEFINE`]: bind `name` to the body at `entry`, as the `proc`
/// command that is running says to.
///
/// The argument list arrives as the text the script wrote, and is parsed here
/// rather than carried as a structure, because this is the one place a
/// signature is needed at run time and the text is already a chunk constant.
/// It parsed once at compile time, which is where a malformed one is reported;
/// the refusal is repeated rather than assumed away.
pub(crate) fn define_op(interp: &Shared, vm: &mut VM) -> Result<(), TclError> {
    let entry = vm.pop();
    let spec = to_tcl_string(&vm.pop());
    let name = to_tcl_string(&vm.pop());
    let entry = match entry {
        Value::Int(n) if n >= 0 && (n as usize) < vm.chunk.ops.len() => n as usize,
        other => {
            return Err(TclError::plain(format!(
                "procedure \"{name}\" has no body at {}",
                to_tcl_string(&other)
            )))
        }
    };
    let sig = parse_signature(&name, &spec).map_err(TclError::plain)?;
    let defined = RuntimeProc {
        chunk: chunk_key(&vm.chunk),
        entry,
        sig,
    };
    interp
        .lock()
        .expect("interpreter lock")
        .commands
        .insert(name, defined);
    // `proc` itself evaluates to the empty string.
    vm.push(Value::Str(std::sync::Arc::new(String::new())));
    Ok(())
}

/// [`ext::DYN_CALL`]: the operands are the script line, the command name and
/// then the arguments, in the order the compiler pushed them.
///
/// The line rides on the stack because the failures this op can produce are the
/// ones the compiler used to decide, and those are *located* — dropping the line
/// would turn `(file "x.tcl" line 7)` into a message with no place, which a
/// differential test against tclsh would notice.
pub(crate) fn call_op(interp: &Shared, vm: &mut VM, argc: u8) -> Result<(), TclError> {
    let mut values = Vec::with_capacity(argc as usize);
    for _ in 0..argc {
        values.push(vm.pop());
    }
    values.reverse();
    let line = match values.first() {
        Some(Value::Int(n)) => *n as usize,
        _ => 0,
    };
    let name = to_tcl_string(&values[1]);
    let args = &values[2..];

    // The lock is released before control moves: entering the body runs
    // arbitrary Tcl, which may define another procedure or `eval` a script,
    // and both of those want this same lock.
    let defined = {
        let state = interp.lock().expect("interpreter lock");
        state.commands.get(&name).cloned()
    };
    let outcome = match defined {
        // A procedure the script defined shadows a foreign command of the same
        // name, which is the order tclsh resolves in.
        Some(p) if p.chunk == chunk_key(&vm.chunk) => enter(vm, &name, &p, args),
        // A registered Tk command is the only thing a chunk hands control to
        // that can read or write the interpreter's variables behind its back,
        // so this is where the running slot vector and the interpreter's map
        // are brought into agreement and the traces that costs are fired. The
        // sync sits on *this* arm rather than around the whole op because the
        // arm above it is an ordinary procedure call, which cannot do that and
        // should not pay for it. See `crate::runtime::sync_out`; when nothing
        // is traced each side is one atomic load.
        _ => foreign(interp, vm, &name, args),
    };
    outcome.map_err(|msg| TclError {
        msg,
        line: Some(line),
    })
}

/// Enter a procedure's body, having arranged the actual arguments the way its
/// prologue expects them.
///
/// This is [`Compiler::push_actuals`] at run time and it makes the same three
/// decisions: refuse an argument count the signature does not admit, supply a
/// default for each omitted parameter, and fold the surplus into `args`. The
/// frame is the one `Op::Call` would have pushed — `stack_base` beneath the
/// formals, so `Op::ReturnValue` truncates back past them — and `vm.ip` is
/// already the op after this one, which is where the body returns to.
fn enter(vm: &mut VM, name: &str, p: &RuntimeProc, args: &[Value]) -> Result<(), String> {
    let sig = &p.sig;
    let fixed = sig.fixed();
    if args.len() < sig.required || (!sig.variadic && args.len() > fixed) {
        return Err(format!("wrong # args: should be \"{}\"", sig.usage(name)));
    }
    let base = vm.stack.len();
    for i in 0..fixed {
        let value = match args.get(i) {
            Some(v) => v.clone(),
            // `required` guarantees the omitted parameters have defaults.
            None => {
                let default = sig.params[i]
                    .default
                    .as_deref()
                    .expect("defaulted parameter");
                literal_value(default)
            }
        };
        vm.push(value);
    }
    if sig.variadic {
        let rest: Vec<String> = args[fixed.min(args.len())..]
            .iter()
            .map(to_tcl_string)
            .collect();
        vm.push(Value::Str(std::sync::Arc::new(list::join(&rest))));
    }
    vm.frames.push(Frame {
        return_ip: vm.ip,
        stack_base: base,
        slots: Vec::new(),
    });
    vm.ip = p.entry;
    Ok(())
}

/// A name no procedure answers to: a command Tk registered, or nothing.
///
/// Without the `tk` feature there is no second table to consult, and the answer
/// is the `invalid command name` the compiler used to defer — same wording,
/// same line.
fn foreign(interp: &Shared, vm: &mut VM, name: &str, args: &[Value]) -> Result<(), String> {
    #[cfg(feature = "tk")]
    {
        crate::runtime::sync_out(interp, vm)?;
        let outcome = crate::tk::dispatch::invoke(name, args);
        // Even a command that failed may have written a variable before it
        // failed, exactly as a failing script's `set` still counts, so taking
        // the interpreter's values back up is not on the success path only.
        crate::runtime::sync_in(interp, vm);
        vm.push(Value::Str(std::sync::Arc::new(outcome?)));
        Ok(())
    }
    #[cfg(not(feature = "tk"))]
    {
        let _ = (interp, vm, args);
        Err(format!("invalid command name \"{name}\""))
    }
}

impl Compiler {
    /// `proc name args body`.
    pub(crate) fn cmd_proc(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let [name_w, spec_w, body_w] = args else {
            return self.error("wrong # args: should be \"proc name args body\"");
        };
        // A `proc` the script's own text runs exactly once binds its name while
        // this compiler is running, so its calls can be `Op::Call`. Anywhere
        // else the binding is an event at run time, and both halves of that —
        // the definition and every call — go through the run-time table.
        let at_top = self.top_level;
        let name = self.literal_of(name_w, "procedure name")?.to_string();
        if Compiler::BUILTINS.contains(&name.as_str()) {
            return self.error(format!(
                "redefining the built-in command \"{name}\" is not supported"
            ));
        }
        if self.coros.contains(&name) {
            return self.error(format!(
                "procedure \"{name}\" collides with a coroutine of the same name, which is \
                 not supported"
            ));
        }
        let spec = self.literal_of(spec_w, "argument list")?.to_string();
        let sig = match parse_signature(&name, &spec) {
            Ok(sig) => sig,
            Err(msg) => return self.error(msg),
        };
        if !at_top {
            // Recorded for the next pass, which is what turns every call to
            // this name — including the ones already compiled above it — into a
            // run-time lookup. A definition in a branch that is never taken is
            // recorded too: the name being *conditional* is the fact that
            // matters, not whether the condition holds.
            self.seen_runtime.insert(name.clone());
        } else if !self.defined.insert(name.clone()) {
            // Only a top-level definition claims the name at compile time, so
            // only a second top-level one is a redefinition this compiler
            // cannot represent. A conditional one is not: it replaces the
            // command when it runs, which the run-time table does model.
            return self.error(format!(
                "procedure \"{name}\" is redefined, which is not supported"
            ));
        }
        let slots = u8::try_from(sig.params.len())
            .map_err(|_| {
                self.err(format!(
                    "procedure \"{name}\" has more than 255 formal parameters"
                ))
            })?
            .into();
        if at_top {
            // The signature a call site needs in order to arrange the actuals
            // itself. A conditional definition supplies it at run time instead,
            // out of the argument-list text [`ext::PROC_DEFINE`] carries.
            self.procs.insert(name.clone(), sig.clone());
        }
        // A body that will not parse is still a definition: tclsh compiles a
        // procedure's body when it is first called, so `proc p {} {puts "x}`
        // with `p` never called runs to completion there. The failure becomes
        // the body's only instruction, which is where calling it finds it.
        let body = self.body_of(body_w)?;

        let skip = self.emit(Op::Jump(usize::MAX), 0);
        let entry = self.b.current_pos();

        // The body compiles in its own frame: a fresh slot scope, no enclosing
        // loop to break out of, and no enclosing `catch`.
        let outer_depth = std::mem::replace(&mut self.depth, slots);
        let outer_loops = std::mem::take(&mut self.loops);
        let outer_catch = std::mem::replace(&mut self.catch_depth, 0);
        let outer_scope = self.scope.replace(scope_for(&sig));
        let outer_top = std::mem::replace(&mut self.top_level, false);
        let outer_static = std::mem::replace(&mut self.static_ctx, false);

        for slot in (0..slots).rev() {
            self.emit(Op::SetSlot(slot as u16), -1);
        }
        let compiled = match &body {
            crate::compiler::Body::Script(script) => self.script_value(script),
            crate::compiler::Body::Deferred(msg) => {
                let msg = msg.clone();
                self.raise_at_run_time(&msg)
            }
        };
        // A body that falls off its end returns the value of its last command.
        self.emit(Op::ReturnValue, -1);

        self.depth = outer_depth;
        self.loops = outer_loops;
        self.catch_depth = outer_catch;
        self.scope = outer_scope;
        self.top_level = outer_top;
        self.static_ctx = outer_static;
        compiled?;

        let after = self.b.current_pos();
        self.b.patch_jump(skip, after);
        if at_top {
            // The address book `Op::Call` and `coroutine` resolve through. Only
            // a top-level definition earns one: two conditional definitions may
            // share a name, and `Chunk::find_sub` answers with the first entry
            // registered under it, which would send both calls to one body.
            let name_idx = self.b.add_name(&name);
            self.b.add_sub_entry(name_idx, entry);
        }
        // A top-level definition needs a run-time binding too when some *other*,
        // conditional definition claims the same name: every call to that name
        // has become a lookup, so the table has to be able to answer with this
        // body — and to answer with it only from the moment this command runs.
        let bind_when_it_runs = !at_top || self.runtime.contains(&name);
        if !bind_when_it_runs {
            // `proc` itself evaluates to the empty string.
            self.push_empty();
            return Ok(());
        }
        self.push_str(&name);
        self.push_str(&spec);
        self.push_value(Value::Int(entry as i64));
        // Three operands off the stack for `proc`'s own empty result on.
        self.emit(Op::Extended(ext::PROC_DEFINE, 3), -2);
        Ok(())
    }

    /// A call to a command whose name is resolved when the call runs: a
    /// procedure some conditional `proc` defines, or a command Tk registered.
    ///
    /// The line, the name, then the arguments, then the op that pops all of
    /// them. The arguments are lowered exactly as any other command's are, so a
    /// command substitution inside one still runs before the dispatch — and
    /// still runs even when the dispatch then fails, which is the order Tcl
    /// substitutes in and the order [`Compiler::defer`] already preserved.
    pub(crate) fn call_runtime(&mut self, name: &str, args: &[Word]) -> Result<(), CompileError> {
        let count = u8::try_from(args.len() + 2)
            .map_err(|_| self.err(format!("more than 253 arguments to the command \"{name}\"")))?;
        self.push_value(Value::Int(self.command_line as i64));
        self.push_str(name);
        for arg in args {
            self.word(arg)?;
        }
        // The op consumes the line, the name and every argument, and leaves the
        // command's value: `count` off the stack for one on. The two operands
        // the compiler synthesised are part of that count, so the depth this
        // reports is one deeper than a call with the name alone would be.
        self.emit(Op::Extended(ext::DYN_CALL, count), 1 - count as i32);
        Ok(())
    }

    /// A call to a procedure this script defines.
    pub(crate) fn call_proc(&mut self, name: &str, args: &[Word]) -> Result<(), CompileError> {
        let slots = self.push_actuals(name, args)?;
        let name_idx = self.b.add_name(name);
        self.emit(Op::Call(name_idx, slots as u8), 1 - slots as i32);
        Ok(())
    }

    /// Push exactly one value per formal parameter of the procedure `name`,
    /// which is what its fixed prologue expects, and answer how many. This is
    /// where a call adapts to the signature: an omitted parameter's default is
    /// pushed here, and surplus arguments are collected into `args` here.
    ///
    /// `coroutine` uses it too — the body of a coroutine is entered with the
    /// same convention as a call, only from a fresh VM the driver positions.
    pub(crate) fn push_actuals(
        &mut self,
        name: &str,
        args: &[Word],
    ) -> Result<usize, CompileError> {
        let sig = self.procs.get(name).cloned().expect("known procedure");
        let fixed = sig.fixed();
        if args.len() < sig.required || (!sig.variadic && args.len() > fixed) {
            return self.error(format!("wrong # args: should be \"{}\"", sig.usage(name)));
        }

        for i in 0..fixed {
            match args.get(i) {
                Some(w) => self.word(w)?,
                // `required` guarantees the omitted parameters have defaults.
                None => {
                    let default = sig.params[i].default.clone().expect("defaulted parameter");
                    self.push_text(&default);
                }
            }
        }
        if sig.variadic {
            let extra = &args[fixed.min(args.len())..];
            let count = u8::try_from(extra.len()).map_err(|_| {
                self.err(format!(
                    "more than 255 arguments collected into \"args\" of \"{name}\""
                ))
            })?;
            for w in extra {
                self.word(w)?;
            }
            self.emit(Op::Extended(ext::LIST, count), 1 - extra.len() as i32);
        }
        Ok(sig.params.len())
    }

    /// `return ?-code code? ?result?`.
    pub(crate) fn cmd_return(&mut self, args: &[Word]) -> Result<(), CompileError> {
        if self.scope.is_none() {
            return self.error("\"return\" outside of a procedure is not supported");
        }
        if self.catch_depth > 0 {
            // `catch {return x}` reports return code 2 rather than returning
            // from the procedure, which this frontend does not model.
            return self.error("\"return\" out of a \"catch\" script is not supported");
        }

        let mut rest = args;
        let mut code = "ok";
        if let [first, value, tail @ ..] = args {
            if first.as_literal() == Some("-code") {
                code = self.literal_of(value, "return code")?;
                rest = tail;
            }
        }
        if let Some(w) = rest.first() {
            if w.as_literal().is_some_and(|t| t.starts_with('-')) && rest.len() > 1 {
                return self.error(format!(
                    "return option \"{}\" is not supported",
                    w.as_literal().unwrap_or_default()
                ));
            }
        }
        let result = match rest {
            [] => None,
            [v] => Some(v),
            _ => return self.error("wrong # args: should be \"return ?-code code? ?result?\""),
        };

        match code {
            "ok" | "0" => {
                match result {
                    Some(w) => self.word(w)?,
                    None => self.push_empty(),
                }
                self.emit(Op::ReturnValue, -1);
            }
            "error" | "1" => {
                match result {
                    Some(w) => self.word(w)?,
                    None => self.push_empty(),
                }
                self.emit(Op::Extended(ext::ERROR, 0), -1);
            }
            other => {
                return self.error(format!(
                    "return -code \"{other}\" is not supported; only \"ok\" and \"error\" are"
                ))
            }
        }
        // Control has left; the value keeps the depth arithmetic honest.
        self.push_empty();
        Ok(())
    }

    /// `global ?varname ...?` — no effect outside a procedure body.
    pub(crate) fn cmd_global(&mut self, args: &[Word]) -> Result<(), CompileError> {
        for w in args {
            let name = self.var_name_of(w)?;
            let Some(scope) = self.scope.as_mut() else {
                continue;
            };
            if scope.locals.contains_key(&name) {
                return self.error(format!("variable \"{name}\" already exists"));
            }
            scope.globals.insert(name);
        }
        self.push_empty();
        Ok(())
    }
}

/// The slot scope a procedure body starts with: one slot per formal parameter,
/// in declaration order, matching the prologue's `Op::SetSlot` sequence.
fn scope_for(sig: &Signature) -> Scope {
    let mut scope = Scope::default();
    for (i, p) in sig.params.iter().enumerate() {
        scope.locals.insert(p.name.clone(), i as u16);
    }
    scope.next_slot = sig.params.len() as u16;
    scope
}
