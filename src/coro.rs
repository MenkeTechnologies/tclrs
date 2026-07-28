//! `coroutine`, `yield`, `yieldto` and `info coroutine`.
//!
//! ## How a Tcl coroutine maps onto fusevm's scheduler
//!
//! fusevm's `sched` module is a cooperative green-thread scheduler built for
//! Go: each goroutine is its own [`VM`] over the shared program `Chunk`, and a
//! goroutine op raises a `SchedReq`, halts its VM, and lets the driver service
//! the request and resume whichever VM should run next. The op has already
//! advanced `ip` when it halts, so a resumed VM continues *past* it and the
//! driver delivers the op's result by pushing onto that VM's stack.
//!
//! A Tcl coroutine is the same shape, and this module reuses that mechanism
//! rather than the `Scheduler` type built on it:
//!
//! * **A coroutine is a VM.** `coroutine name cmd ?arg…?` builds a fresh `VM`
//!   over the same chunk and positions it at `cmd`'s sub entry with the actual
//!   arguments below a frame whose `return_ip` is one past the last op — the
//!   same positioning `Scheduler::position_goroutine` performs — so the body
//!   returning ends that VM's `run()`.
//! * **Suspending is halting.** [`ext::CORO_YIELD`] stashes a [`Request`] and
//!   calls `VM::request_halt`, exactly as `Op::ChanRecv` stashes a `SchedReq`.
//!   The driver in [`crate::runtime`] reads the request after `run()` returns,
//!   pushes the yielded value onto the *resumer's* stack, and runs the resumer.
//! * **Resuming is pushing a value and calling `run()` again.** The suspended
//!   VM's `ip` already points past its `yield`, so `clear_halt()` plus a value
//!   on its stack continues the body with that value as the `yield`'s result.
//!
//! Two things Tcl needs are not in the Go model and are supplied here.
//!
//! * **Control transfer is symmetric, not a run queue.** Go picks the next
//!   goroutine off a FIFO; Tcl names the successor. Each context records the
//!   `resumer` to return to, `yield` hands control back to it, and `yieldto`
//!   *donates* it to the target — which is why the value of a `[c]` that ends
//!   in `yieldto d` is whatever `d` eventually produces.
//! * **One global table, not one per VM.** Goroutines share a heap through the
//!   frontend, but `VM::globals` is per-VM, and Tcl coroutines see the
//!   interpreter's globals (and the arrays that live in them). The driver owns
//!   the table and moves it into whichever VM is about to run; because exactly
//!   one VM runs at a time, that is not a copy and not a race.
//!
//! ## What is refused
//!
//! `coroutine` is a compile-time command here: the name it creates has to be
//! known to every call site, and the body has to be a procedure this script
//! defines, because fusevm resolves `Op::Call` through the chunk's sub table.
//! So the name and the command are literals, the command is one of the
//! script's own procedures, and the `coroutine` command itself appears where
//! [`prescan`] can see it. `yieldto` cedes control to a *coroutine* of this
//! script; ceding it to an arbitrary command would have to evaluate that
//! command in the resumer's context, which this frontend cannot do, so it is
//! refused rather than approximated.

use std::collections::HashSet;
use std::sync::Arc;

use fusevm::{Op, Value, VM};

use crate::compiler::{ext, CompileError, Compiler};
use crate::parser::{Part, Script, Word};
use crate::runtime::to_tcl_string;

/// What a coroutine op asks the driver to do. The op stashes one of these and
/// halts; [`crate::runtime`] services it and decides which VM runs next.
#[derive(Debug)]
pub(crate) enum Request {
    /// `coroutine name command ?arg…?` — create the context and enter it.
    Create {
        name: String,
        command: String,
        args: Vec<Value>,
    },
    /// `name ?arg…?` — resume a suspended coroutine.
    Resume { name: String, args: Vec<Value> },
    /// `yield ?value?` — suspend, handing `value` back to the resumer.
    Yield(Value),
    /// `yieldto name ?arg…?` — suspend, giving the resumer to `name`.
    YieldTo { name: String, args: Vec<Value> },
}

/// Whether `id` is one of this module's extension ops.
pub(crate) fn is_op(id: u16) -> bool {
    (ext::CORO_CREATE..=ext::CORO_INFO).contains(&id)
}

/// Run a coroutine op. `current` is the name of the coroutine whose VM is
/// running, which only `info coroutine` needs; every other op returns the
/// request the driver is to service, and the caller halts the VM.
pub(crate) fn extension(vm: &mut VM, id: u16, arg: u8, current: Option<&str>) -> Option<Request> {
    match id {
        ext::CORO_INFO => {
            let name = match current {
                Some(name) => format!("::{name}"),
                None => String::new(),
            };
            vm.push(Value::Str(Arc::new(name)));
            None
        }
        ext::CORO_CREATE => {
            let command = to_tcl_string(&vm.pop());
            let name = to_tcl_string(&vm.pop());
            Some(Request::Create {
                name,
                command,
                args: pop_args(vm, arg),
            })
        }
        ext::CORO_RESUME => {
            let name = to_tcl_string(&vm.pop());
            Some(Request::Resume {
                name,
                args: pop_args(vm, arg),
            })
        }
        ext::CORO_YIELD => Some(Request::Yield(vm.pop())),
        // The target is pushed first here, since a `yieldto` may name its
        // target with a word to evaluate and Tcl evaluates words left to right.
        _ => {
            let args = pop_args(vm, arg);
            Some(Request::YieldTo {
                name: to_tcl_string(&vm.pop()),
                args,
            })
        }
    }
}

/// Pop `count` arguments, restoring the order the call site pushed them in.
fn pop_args(vm: &mut VM, count: u8) -> Vec<Value> {
    let mut args = Vec::with_capacity(count as usize);
    for _ in 0..count {
        args.push(vm.pop());
    }
    args.reverse();
    args
}

/// Collect the names of every coroutine the script creates, before anything is
/// compiled, so a call to one may appear before the `coroutine` command that
/// creates it — a procedure defined earlier may resume a coroutine created
/// later, which is the ordinary generator shape.
///
/// The walk covers the script's own commands and the command substitutions
/// inside them, at any depth: `coroutine j1 j [coroutine j2 j …]` creates both.
/// It deliberately does not descend into bodies, which may run any number of
/// times; the compiler refuses a `coroutine` command there for the same reason.
pub fn prescan(coros: &mut HashSet<String>, script: &Script) {
    for cmd in &script.commands {
        if cmd.words.first().and_then(Word::as_literal) == Some("coroutine") {
            if let Some(name) = cmd.words.get(1).and_then(Word::as_literal) {
                coros.insert(name.to_string());
            }
        }
        for word in &cmd.words {
            for part in &word.parts {
                if let Part::Script(nested) = part {
                    prescan(coros, nested);
                }
            }
        }
    }
}

impl Compiler {
    /// `coroutine name command ?arg ...?`.
    pub(crate) fn cmd_coroutine(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let [name_w, command_w, actuals @ ..] = args else {
            return self.error("wrong # args: should be \"coroutine name cmd ?arg ...?\"");
        };
        if !self.static_ctx {
            return self.error(
                "\"coroutine\" is only supported at the top level of a script, or in a command \
                 substitution in one",
            );
        }
        let name = self.literal_of(name_w, "coroutine name")?.to_string();
        let command = self.literal_of(command_w, "coroutine command")?.to_string();
        if Compiler::BUILTINS.contains(&name.as_str()) {
            return self.error(format!(
                "redefining the built-in command \"{name}\" is not supported"
            ));
        }
        if self.procs.contains_key(&name) {
            return self.error(format!(
                "coroutine \"{name}\" collides with a procedure of the same name, which is not \
                 supported"
            ));
        }
        if Compiler::BUILTINS.contains(&command.as_str()) {
            return self.error(format!(
                "a coroutine of the built-in command \"{command}\" is not supported; its body \
                 must be a procedure this script defines"
            ));
        }
        if !self.procs.contains_key(&command) {
            return self.error(format!("invalid command name \"{command}\""));
        }

        let slots = self.push_actuals(&command, actuals)?;
        let count = u8::try_from(slots).map_err(|_| {
            self.err(format!(
                "procedure \"{command}\" has more than 255 formal parameters"
            ))
        })?;
        self.push_str(&name);
        self.push_str(&command);
        self.emit(Op::Extended(ext::CORO_CREATE, count), -(slots as i32) - 1);
        // The name is only registered by the prescan, which has seen every
        // position this command is allowed to appear in.
        Ok(())
    }

    /// A coroutine's context command: `name ?arg ...?` resumes it.
    ///
    /// How many arguments it takes depends on how the coroutine suspended —
    /// one at most after `yield`, any number after `yieldto` — so the count is
    /// checked by the driver, which knows, and not here.
    pub(crate) fn call_coro(&mut self, name: &str, args: &[Word]) -> Result<(), CompileError> {
        let count = u8::try_from(args.len()).map_err(|_| {
            self.err(format!(
                "more than 255 arguments to the coroutine \"{name}\""
            ))
        })?;
        for w in args {
            self.word(w)?;
        }
        self.push_str(name);
        self.emit(Op::Extended(ext::CORO_RESUME, count), -(args.len() as i32));
        Ok(())
    }

    /// `yield ?value?`. Whether this runs inside a coroutine is a property of
    /// the call, not of the code — the same procedure may be called both ways —
    /// so the driver raises Tcl's error for the loose case at run time.
    pub(crate) fn cmd_yield(&mut self, args: &[Word]) -> Result<(), CompileError> {
        match args {
            [] => self.push_empty(),
            [value] => self.word(value)?,
            _ => return self.error("wrong # args: should be \"yield ?value?\""),
        }
        // One value in, one value out: the resumption value replaces it.
        self.emit(Op::Extended(ext::CORO_YIELD, 0), 0);
        Ok(())
    }

    /// `yieldto name ?arg ...?`, where `name` is a coroutine of this script.
    ///
    /// A literal target is checked here; one that has to be evaluated — the
    /// documented three-way juggler passes the next coroutine's name in a
    /// variable — is checked by the driver when the transfer happens.
    pub(crate) fn cmd_yieldto(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let [target_w, actuals @ ..] = args else {
            return self.error("wrong # args: should be \"yieldto command ?arg ...?\"");
        };
        if let Some(target) = target_w.as_literal() {
            if !self.coros.contains(target) {
                return self.error(format!(
                    "\"yieldto {target}\": ceding control to a command that is not a coroutine \
                     of this script is not supported"
                ));
            }
        }
        let count = u8::try_from(actuals.len())
            .map_err(|_| self.err("more than 255 arguments to \"yieldto\"".to_string()))?;
        self.word(target_w)?;
        for w in actuals {
            self.word(w)?;
        }
        self.emit(
            Op::Extended(ext::CORO_YIELDTO, count),
            -(actuals.len() as i32),
        );
        Ok(())
    }

    /// `info coroutine` — the one `info` subcommand this frontend has.
    pub(crate) fn cmd_info(&mut self, args: &[Word]) -> Result<(), CompileError> {
        match args {
            [w] if w.as_literal() == Some("coroutine") => {
                self.emit(Op::Extended(ext::CORO_INFO, 0), 1);
                Ok(())
            }
            [w, ..] if w.as_literal() == Some("coroutine") => {
                self.error("wrong # args: should be \"info coroutine\"")
            }
            [] => self.error("wrong # args: should be \"info subcommand ?argument ...?\""),
            [w, ..] => self.error(format!(
                "unknown or unsupported subcommand \"{}\": only \"info coroutine\" is supported",
                w.as_literal().unwrap_or_default()
            )),
        }
    }
}
