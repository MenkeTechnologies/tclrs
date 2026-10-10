//! `trace`: variable, command and execution traces.
//!
//! The command is `Tcl_TraceObjCmd` (`generic/tclTrace.c`), and what it does is
//! keep a registry: `trace add` appends an entry, `trace remove` deletes the one
//! that matches, `trace info` lists them newest first. Whether anything *fires*
//! is the other half, and this frontend resolves variables and commands while
//! compiling, so a traced name has to be known then:
//!
//! * **Variables.** A `trace add variable NAME …` written out in a script makes
//!   `NAME` a traced name for that whole script, and for every script compiled
//!   later while the interpreter still holds a trace on it. An access to a
//!   traced global is lowered through the computed-name ops
//!   ([`crate::compiler::ext::DYN_GET`] and its kin), which already resolve the
//!   variable when they run, and those fire the traces: `read` before the value
//!   is taken, `write` after it is stored, `unset` after it is gone — which also
//!   deletes the variable's traces, as tclsh does. A callback runs at the global
//!   level with `name1 name2 op` appended to the command prefix, and an error it
//!   raises is the error of the access: `can't set "x": message`.
//! * **Commands.** `rename` queues a `rename` or `delete` event for each trace
//!   on the command, and the callbacks run once the rename is done, with the old
//!   and new names fully qualified.
//! * **Execution.** A command named by a `trace add execution` is called through
//!   the run-time dispatch instead of directly, and `enter` and `leave` callbacks
//!   run around it with the command string, and the code and result on leave.
//!
//! Not modelled: `array` traces on the `array` command, traces on a procedure's
//! own locals, `enterstep` and `leavestep`, and a trace through an `upvar`
//! alias.

use std::sync::Arc;

use fusevm::{Op, Value, VM};

use crate::compiler::{CompileError, Compiler};
use crate::parser::Word;
use crate::runtime::{to_tcl_string, Shared, TclError};

/// Extension opcode ids owned by this module.
pub mod ext {
    pub use crate::compiler::ext::TRACE_BASE as BASE;
    /// `[arg …]` → the command's value. The inline operand is the word count.
    pub const TRACE: u16 = BASE;
}

/// The command names this module claims.
pub const COMMANDS: &[&str] = &["trace"];

/// Whether an id belongs to this module's block.
pub(crate) fn is_op(id: u16) -> bool {
    (ext::BASE..crate::compiler::ext::TRACE_END).contains(&id)
}

/// One trace on a variable.
#[derive(Clone, Debug)]
pub(crate) struct VarTrace {
    /// The variable's name as the global table keeps it, or `name(index)`.
    pub name: String,
    pub ops: Vec<String>,
    /// The command prefix, as a list.
    pub script: String,
}

/// One trace on a command, or on a command's execution.
#[derive(Clone, Debug)]
pub(crate) struct CmdTrace {
    /// Fully qualified, with its leading `::`.
    pub name: String,
    pub ops: Vec<String>,
    pub script: String,
}

/// A callback queued for when the command that caused it has finished.
#[derive(Clone, Debug)]
pub(crate) struct Event {
    pub script: String,
    pub args: Vec<String>,
}

/// Every trace the interpreter holds.
#[derive(Default)]
pub(crate) struct Traces {
    pub vars: Vec<VarTrace>,
    pub commands: Vec<CmdTrace>,
    pub execs: Vec<CmdTrace>,
    /// Callbacks a command left to be run once it is done and the interpreter
    /// is free: a `rename` cannot run scripts while it holds the registry.
    pub events: Vec<Event>,
    /// The variables whose traces are running now. A trace that touches its own
    /// variable does not fire itself again.
    active: Vec<String>,
}

impl Traces {
    /// The names the registry makes `trace`-aware for a script that mentions
    /// them: variables, then commands under execution traces.
    pub(crate) fn variable_names(&self) -> Vec<String> {
        self.vars
            .iter()
            .map(|t| t.name.split('(').next().unwrap_or(&t.name).to_string())
            .collect()
    }

    pub(crate) fn execution_names(&self) -> Vec<String> {
        self.execs.iter().map(|t| t.name.clone()).collect()
    }
}

const VARIABLE_OPS: &[&str] = &["array", "read", "unset", "write"];
const COMMAND_OPS: &[&str] = &["delete", "rename"];
const EXECUTION_OPS: &[&str] = &["enter", "leave", "enterstep", "leavestep"];

// ── compiling ────────────────────────────────────────────────────────────

pub(crate) fn compile(c: &mut Compiler, args: &[Word]) -> Result<(), CompileError> {
    let Ok(argc) = u8::try_from(args.len()) else {
        return c.error("too many arguments for \"trace\"");
    };
    // `trace add variable NAME …` written out makes the name traced for the whole
    // script, which is what lets the accesses above it be lowered through the
    // ops that fire.
    let literal = |i: usize| args.get(i).and_then(|w| w.as_literal());
    if matches!(literal(0), Some("add" | "remove")) {
        match (literal(1), literal(2)) {
            (Some("variable"), Some(name)) => c.note_traced_variable(name),
            (Some("execution"), Some(name)) => c.note_traced_command(name),
            _ => {}
        }
    }
    for w in args {
        c.word(w)?;
    }
    c.emit(Op::Extended(ext::TRACE, argc), 1 - i32::from(argc));
    Ok(())
}

// ── running ──────────────────────────────────────────────────────────────

/// `trace ?option? …` — `Tcl_TraceObjCmd`.
pub(crate) fn run(interp: &Shared, vm: &mut VM, argc: u8) -> Result<(), TclError> {
    let mut words = Vec::with_capacity(argc as usize);
    for _ in 0..argc {
        words.push(to_tcl_string(&vm.pop()));
    }
    words.reverse();
    let result = trace(interp, &words).map_err(TclError::plain)?;
    vm.push(Value::Str(Arc::new(result)));
    Ok(())
}

fn choose<'a>(word: &str, from: &'a [&'a str], what: &str) -> Result<&'a str, String> {
    if let Some(exact) = from.iter().find(|c| **c == word) {
        return Ok(exact);
    }
    let prefix: Vec<&&str> = from.iter().filter(|c| c.starts_with(word)).collect();
    if let ([only], false) = (prefix.as_slice(), word.is_empty()) {
        return Ok(only);
    }
    let kind = if prefix.is_empty() {
        "bad"
    } else {
        "ambiguous"
    };
    let (head, last) = from.split_at(from.len() - 1);
    Err(format!(
        "{kind} {what} \"{word}\": must be {}, or {}",
        head.join(", "),
        last[0]
    ))
}

fn trace(interp: &Shared, words: &[String]) -> Result<String, String> {
    let Some(option) = words.first() else {
        return Err("wrong # args: should be \"trace option ?arg ...?\"".to_string());
    };
    let option = choose(option, &["add", "info", "remove"], "option")?;
    match option {
        "add" | "remove" => {
            if words.len() < 3 {
                return Err(format!(
                    "wrong # args: should be \"trace {option} type ?arg ...?\""
                ));
            }
            let kind = choose(&words[1], &["execution", "command", "variable"], "option")?;
            if words.len() != 5 {
                return Err(format!(
                    "wrong # args: should be \"trace {option} {kind} name opList command\""
                ));
            }
            let (name, oplist, script) = (&words[2], &words[3], &words[4]);
            let ops = parse_ops(oplist, kind)?;
            let mut state = interp.lock().expect("interpreter lock");
            match kind {
                "variable" => {
                    if option == "add" {
                        if let Some(at) = name.rfind("::") {
                            let parent = &name[..at];
                            let parent = if parent.is_empty() { "::" } else { parent };
                            if !state.ns.exists(parent) {
                                return Err(format!(
                                    "can't trace \"{name}\": parent namespace doesn't exist"
                                ));
                            }
                        }
                    }
                    let key = crate::cmd_namespace::store_key(name).to_string();
                    let key = if name.starts_with("::") {
                        key
                    } else {
                        name.clone()
                    };
                    if option == "add" {
                        state.traces.vars.push(VarTrace {
                            name: key,
                            ops,
                            script: script.clone(),
                        });
                    } else if let Some(at) = state
                        .traces
                        .vars
                        .iter()
                        .rposition(|t| t.name == key && t.ops == ops && t.script == *script)
                    {
                        state.traces.vars.remove(at);
                    }
                }
                _ => {
                    let qualified = qualified(name);
                    let known = state.ns.command(&qualified).is_some()
                        || state
                            .commands
                            .contains_key(crate::cmd_namespace::store_key(&qualified))
                        || crate::names::is_command(name);
                    if !known {
                        return Err(format!("unknown command \"{name}\""));
                    }
                    let list = if kind == "command" {
                        &mut state.traces.commands
                    } else {
                        &mut state.traces.execs
                    };
                    if option == "add" {
                        list.push(CmdTrace {
                            name: qualified,
                            ops,
                            script: script.clone(),
                        });
                    } else if let Some(at) = list
                        .iter()
                        .rposition(|t| t.name == qualified && t.ops == ops && t.script == *script)
                    {
                        list.remove(at);
                    }
                }
            }
            Ok(String::new())
        }
        _ => {
            if words.len() < 2 {
                return Err("wrong # args: should be \"trace info type name\"".to_string());
            }
            let kind = choose(&words[1], &["execution", "command", "variable"], "option")?;
            if words.len() != 3 {
                return Err(format!(
                    "wrong # args: should be \"trace info {kind} name\""
                ));
            }
            let name = &words[2];
            let state = interp.lock().expect("interpreter lock");
            let pairs: Vec<String> = match kind {
                "variable" => {
                    let key = crate::cmd_namespace::store_key(name).to_string();
                    let key = if name.starts_with("::") {
                        key
                    } else {
                        name.clone()
                    };
                    state
                        .traces
                        .vars
                        .iter()
                        .rev()
                        .filter(|t| t.name == key)
                        .map(|t| pair(&t.ops, &t.script))
                        .collect()
                }
                _ => {
                    let qualified = qualified(name);
                    let known = state.ns.command(&qualified).is_some()
                        || state
                            .commands
                            .contains_key(crate::cmd_namespace::store_key(&qualified))
                        || crate::names::is_command(name);
                    if !known {
                        return Err(format!("unknown command \"{name}\""));
                    }
                    let list = if kind == "command" {
                        &state.traces.commands
                    } else {
                        &state.traces.execs
                    };
                    list.iter()
                        .rev()
                        .filter(|t| t.name == qualified)
                        .map(|t| pair(&t.ops, &t.script))
                        .collect()
                }
            };
            Ok(crate::list::join(&pairs))
        }
    }
}

fn pair(ops: &[String], script: &str) -> String {
    crate::list::join(&[crate::list::join(ops), script.to_string()])
}

fn qualified(name: &str) -> String {
    if name.starts_with("::") {
        name.to_string()
    } else {
        format!("::{name}")
    }
}

/// `opList` of one trace type: a non-empty list of its operation names, each
/// spelled in full.
fn parse_ops(list: &str, kind: &str) -> Result<Vec<String>, String> {
    let (from, expected) = match kind {
        "variable" => (VARIABLE_OPS, "array, read, unset, or write"),
        "command" => (COMMAND_OPS, "delete or rename"),
        _ => (EXECUTION_OPS, "enter, leave, enterstep, or leavestep"),
    };
    let items = crate::list::split(list)?;
    if items.is_empty() {
        let shape = match kind {
            "variable" => "must be one or more of array, read, unset, or write",
            "command" => "must be one or more of delete or rename",
            _ => "must be one or more of enter, leave, enterstep, or leavestep",
        };
        return Err(format!("bad operation list \"{list}\": {shape}"));
    }
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        if !from.contains(&item.as_str()) {
            return Err(format!("bad operation \"{item}\": must be {expected}"));
        }
        out.push(item);
    }
    Ok(out)
}

// ── firing ───────────────────────────────────────────────────────────────

/// The command to evaluate for a callback: the registered prefix, a list, with
/// the arguments appended as further elements.
fn callback(script: &str, args: &[String]) -> String {
    let mut words = crate::list::split(script).unwrap_or_else(|_| vec![script.to_string()]);
    words.extend(args.iter().cloned());
    crate::list::join(&words)
}

/// Run the traces of variable `name` (`a` or `a(i)`) for `op`, newest first.
///
/// `verb` is what the access was doing — `set`, `read` or `unset` — which is how
/// a callback's error is worded: `can't set "x": message`.
pub(crate) fn fire_variable(
    interp: &Shared,
    vm: &mut VM,
    name: &str,
    op: &str,
) -> Result<(), TclError> {
    let (base, index) = match name.strip_suffix(')').and_then(|n| n.split_once('(')) {
        Some((b, i)) => (b.to_string(), Some(i.to_string())),
        None => (name.to_string(), None),
    };
    let scripts: Vec<String> = {
        let mut state = interp.lock().expect("interpreter lock");
        if state.traces.vars.is_empty() || state.traces.active.contains(&base) {
            return Ok(());
        }
        let key = crate::cmd_namespace::store_key(&base).to_string();
        let matching: Vec<String> = state
            .traces
            .vars
            .iter()
            .rev()
            .filter(|t| {
                (t.name == key || t.name == base || Some(&t.name) == Some(&name.to_string()))
                    && t.ops.iter().any(|o| o == op)
            })
            .map(|t| t.script.clone())
            .collect();
        if matching.is_empty() {
            return Ok(());
        }
        state.traces.active.push(base.clone());
        matching
    };
    let mut outcome = Ok(());
    for script in scripts {
        // An element's `unset` reports the variable as `a(i)`, which is what
        // tclsh's own trace call receives.
        let first = match (&index, op) {
            (Some(_), "unset") => name.to_string(),
            _ => base.clone(),
        };
        let args = [first, index.clone().unwrap_or_default(), op.to_string()];
        let src = callback(&script, &args);
        let ran = crate::runtime::with_written_back(interp, vm, |i| {
            crate::runtime::run_source(i, &src).map(drop)
        });
        if let Err(e) = ran {
            outcome = Err(e);
            break;
        }
    }
    interp
        .lock()
        .expect("interpreter lock")
        .traces
        .active
        .retain(|n| *n != base);
    match outcome {
        // The refusal reaches the access in the access's own words.
        Err(e) if op != "unset" => {
            let verb = match op {
                "read" => "read",
                _ => "set",
            };
            Err(TclError {
                msg: format!("can't {verb} \"{name}\": {}", e.msg),
                ..TclError::plain(String::new())
            })
        }
        _ => Ok(()),
    }
}

/// An `unset` of the whole variable ends its traces, once they have run.
pub(crate) fn forget_variable(interp: &Shared, name: &str) {
    if name.contains('(') {
        return;
    }
    let key = crate::cmd_namespace::store_key(name).to_string();
    interp
        .lock()
        .expect("interpreter lock")
        .traces
        .vars
        .retain(|t| t.name != key && t.name != name);
}

/// Queue the callbacks of every trace of command `old` for `op` — `rename` or
/// `delete` — with `new` as the second argument (empty for a delete).
pub(crate) fn queue_command_events(traces: &mut Traces, old: &str, new: &str, op: &str) {
    let old = qualified(old);
    let new = if new.is_empty() {
        String::new()
    } else {
        qualified(new)
    };
    let hits: Vec<String> = traces
        .commands
        .iter()
        .rev()
        .filter(|t| t.name == old && t.ops.iter().any(|o| o == op))
        .map(|t| t.script.clone())
        .collect();
    for script in hits {
        traces.events.push(Event {
            script,
            args: vec![old.clone(), new.clone(), op.to_string()],
        });
    }
    // The traces belong to the command: they go where it goes, and with it.
    for list in [&mut traces.commands, &mut traces.execs] {
        if new.is_empty() {
            list.retain(|t| t.name != old);
        } else {
            for t in list.iter_mut().filter(|t| t.name == old) {
                t.name = new.clone();
            }
        }
    }
}

/// The callbacks of the execution traces on `name` for `op`, newest first.
pub(crate) fn execution_scripts(interp: &Shared, name: &str, op: &str) -> Vec<String> {
    let state = interp.lock().expect("interpreter lock");
    if state.traces.execs.is_empty() {
        return Vec::new();
    }
    let name = qualified(name);
    state
        .traces
        .execs
        .iter()
        .rev()
        .filter(|t| t.name == name && t.ops.iter().any(|o| o == op))
        .map(|t| t.script.clone())
        .collect()
}

/// Run `scripts` with `args` appended, at the global level.
pub(crate) fn run_scripts(
    interp: &Shared,
    vm: &mut VM,
    scripts: &[String],
    args: &[String],
) -> Result<(), TclError> {
    for script in scripts {
        let src = callback(script, args);
        crate::runtime::with_written_back(interp, vm, |i| {
            crate::runtime::run_source(i, &src).map(drop)
        })?;
    }
    Ok(())
}

/// Run the callbacks a command queued, now that it has finished.
pub(crate) fn run_events(interp: &Shared, vm: &mut VM) -> Result<(), TclError> {
    loop {
        let event = {
            let mut state = interp.lock().expect("interpreter lock");
            if state.traces.events.is_empty() {
                return Ok(());
            }
            state.traces.events.remove(0)
        };
        let src = callback(&event.script, &event.args);
        crate::runtime::with_written_back(interp, vm, |i| {
            crate::runtime::run_source(i, &src).map(drop)
        })?;
    }
}
