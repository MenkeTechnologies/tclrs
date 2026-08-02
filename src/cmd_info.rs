//! `info` — what the interpreter knows about itself.
//!
//! Almost every subcommand is answered while compiling, because almost every
//! question `info` asks has a compile-time answer in this frontend: the command
//! set is fixed by the dispatcher, a procedure's formal arguments and body come
//! from the `proc` that defined it, and the version numbers are constants. Three
//! questions are genuinely about the running program and are lowered to ops —
//! whether a variable is set ([`ext::INFO_EXISTS`]), which names exist right now
//! ([`ext::INFO_NAMES`]) and how deep the call stack is
//! ([`ext::INFO_LEVEL`]).
//!
//! # `info body` and the body table
//!
//! `info body p` answers with the text `proc p` was given, which the compiler
//! does not keep: `Compiler::procs` holds a [`Signature`] and the body is
//! lowered into the op stream and discarded. [`prescan`] walks the script's own
//! commands before anything is emitted — the same pass
//! [`crate::procs::prescan`] and [`crate::coro::prescan`] make, for the same
//! reason — and keeps the body text against the procedure's name.
//!
//! The table is per compilation and lives in a thread-local, because it is read
//! only while the script that filled it is being lowered: `info body` becomes a
//! string constant in the chunk, so nothing consults the table once the chunk
//! is built.
//!
//! # What `info` refuses
//!
//! * `info level N` — the *value* of a level is the command and arguments that
//!   entered it, and no call site records those here: `Op::Call` pushes the
//!   actual arguments and nothing else. `info level` with no argument answers
//!   exactly, from the VM's frame count.
//! * `info frame`, `info errorstack`, `info cmdcount`, `info class`,
//!   `info object`, `info consts`, `info constant`, `info cmdtype`,
//!   `info functions`, `info library`, `info loaded`,
//!   `info sharedlibextension` — each names machinery this frontend does not
//!   have (a stack-frame record, an object system, `tcl_library`, a package
//!   loader). They are refused by name rather than approximated.
//! * A subcommand that is not one of Tcl's at all gets Tcl's own wording, with
//!   Tcl's own list, so the diagnostic is the reference interpreter's.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use fusevm::{Op, Value, VM};

use crate::assoc::Target;
use crate::compiler::{ext, CompileError, Compiler, Place};
use crate::parser::{Script, Word};
use crate::runtime::{to_tcl_string, Shared};

/// The Tcl language level this frontend implements, which is what
/// `info patchlevel`, `info tclversion` and `$tcl_version` report.
///
/// Not the crate's own version: a script asking `info tclversion` is asking
/// which Tcl it is running on so it can decide whether a Tcl 9 spelling is
/// available, and the answer to that question is 9.0. tclsh 9.0.4 is the
/// interpreter every behaviour here is measured against.
pub const TCL_VERSION: &str = "9.0";
/// The same, to patch level.
pub const TCL_PATCHLEVEL: &str = "9.0.4";

/// Every `info` subcommand tclsh 9.0.4 has, in the order its own error message
/// lists them (`generic/tclCmdIL.c`, the `infoCmds` table). The order is part of
/// the diagnostic, so it is preserved rather than sorted here.
const SUBCOMMANDS: &[&str] = &[
    "args",
    "body",
    "class",
    "cmdcount",
    "cmdtype",
    "commands",
    "complete",
    "constant",
    "consts",
    "coroutine",
    "default",
    "errorstack",
    "exists",
    "frame",
    "functions",
    "globals",
    "hostname",
    "level",
    "library",
    "loaded",
    "locals",
    "nameofexecutable",
    "object",
    "patchlevel",
    "procs",
    "script",
    "sharedlibextension",
    "tclversion",
    "vars",
];

/// Which set of names [`ext::INFO_NAMES`] is being asked for. The operand the
/// compiler pushes; [`names_op`] reads it back.
pub(crate) mod names_kind {
    /// A list the compiler already knows, filtered by the pattern alone —
    /// `info commands` and `info procs`.
    pub const GIVEN: u8 = 0;
    /// Every variable the interpreter holds, which only the interpreter knows.
    /// `info globals`, and `info vars` at the script's own level, where the two
    /// are the same question.
    pub const GLOBALS: u8 = 1;
    /// The candidates the compiler pushed, kept only where the variable each
    /// names is set right now. `info locals`, and `info vars` inside a
    /// procedure — where the answer is the *frame's* variables and not every
    /// global, which is what tclsh answers (measured: `set g 1; proc p {} {set
    /// l 1; info vars}` is `l` there, and `gvar l` once the body says
    /// `global gvar`).
    pub const SET_OF: u8 = 2;
}

// ── the body table ───────────────────────────────────────────────────────

thread_local! {
    /// The body text of every procedure the script being compiled defines.
    /// Replaced wholesale by [`prescan`], which runs once per compilation pass.
    static BODIES: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

/// Record the body text of every `proc` among the script's own commands.
///
/// Called from `Compiler::run` beside the other two prescans. A malformed
/// definition is skipped, exactly as [`crate::procs::prescan`] skips it: the
/// `proc` command itself reports that when it is lowered.
pub fn prescan(script: &Script) {
    let mut bodies = HashMap::new();
    for cmd in &script.commands {
        let [head, name, _spec, body] = cmd.words.as_slice() else {
            continue;
        };
        if head.as_literal() != Some("proc") {
            continue;
        }
        let (Some(name), Some(body)) = (name.as_literal(), body.as_literal()) else {
            continue;
        };
        bodies.insert(name.to_string(), body.to_string());
    }
    BODIES.with(|b| *b.borrow_mut() = bodies);
}

fn body_of(name: &str) -> Option<String> {
    BODIES.with(|b| b.borrow().get(name).cloned())
}

// ── compiling ────────────────────────────────────────────────────────────

/// Lower one `info` command.
pub(crate) fn compile(c: &mut Compiler, args: &[Word]) -> Result<(), CompileError> {
    let Some(first) = args.first() else {
        return c.error("wrong # args: should be \"info subcommand ?argument ...?\"");
    };
    let given = c.literal_of(first, "subcommand")?.to_string();
    let Some(sub) = crate::cmd_string::resolve(&given, SUBCOMMANDS) else {
        return c.error(format!(
            "unknown or ambiguous subcommand \"{given}\": must be {}",
            crate::cmd_string::listing(SUBCOMMANDS)
        ));
    };
    let rest = &args[1..];

    match sub {
        "args" => proc_query(c, sub, rest, |sig, _| {
            let names: Vec<String> = sig.params.iter().map(|p| p.name.clone()).collect();
            Ok(crate::list::join(&names))
        }),
        "body" => proc_query(c, sub, rest, |_, name| {
            body_of(name).ok_or_else(|| format!("\"{name}\" isn't a procedure"))
        }),
        "default" => cmd_default(c, rest),
        "commands" => {
            let commands = command_names(c);
            name_list(c, sub, rest, names_kind::GIVEN, commands)
        }
        "procs" => {
            let mut procs: Vec<String> = c.procs.keys().cloned().collect();
            procs.sort();
            name_list(c, sub, rest, names_kind::GIVEN, procs)
        }
        "complete" => cmd_complete(c, rest),
        "coroutine" => {
            if !rest.is_empty() {
                return c.error("wrong # args: should be \"info coroutine\"");
            }
            c.emit(Op::Extended(ext::CORO_INFO, 0), 1);
            Ok(())
        }
        "exists" => cmd_exists(c, rest),
        "globals" => name_list(c, sub, rest, names_kind::GLOBALS, Vec::new()),
        "hostname" => constant(c, sub, rest, hostname()),
        "level" => cmd_level(c, rest),
        "locals" => {
            let locals = local_names(c);
            name_list(c, sub, rest, names_kind::SET_OF, locals)
        }
        "nameofexecutable" => constant(c, sub, rest, executable()),
        "patchlevel" => constant(c, sub, rest, TCL_PATCHLEVEL.to_string()),
        "script" => cmd_script(c, rest),
        "tclversion" => constant(c, sub, rest, TCL_VERSION.to_string()),
        // Inside a procedure the answer is what the *frame* can see: its locals
        // and the names `global`, `variable` and `upvar` bound into it. At the
        // script's own level it is every variable the interpreter holds, which
        // is `info globals`.
        "vars" if c.scope.is_some() => {
            let visible = visible_names(c);
            name_list(c, sub, rest, names_kind::SET_OF, visible)
        }
        "vars" => name_list(c, sub, rest, names_kind::GLOBALS, Vec::new()),
        other => c.error(format!("info {other} is not supported yet")),
    }
}

/// `info args p` and `info body p`: both take one literal procedure name and
/// answer from what the `proc` command left behind.
fn proc_query(
    c: &mut Compiler,
    sub: &str,
    args: &[Word],
    answer: impl FnOnce(&crate::procs::Signature, &str) -> Result<String, String>,
) -> Result<(), CompileError> {
    let [name_w] = args else {
        return c.error(format!("wrong # args: should be \"info {sub} procname\""));
    };
    let name = c.literal_of(name_w, "procedure name")?.to_string();
    let Some(sig) = c.procs.get(&name).cloned() else {
        // tclsh reports this when the command runs — `if {0} {info body nope}`
        // runs to completion there — so it is marked deferrable and becomes a
        // raise standing where the command's code would have stood.
        return Err(c.deferrable_err(format!("\"{name}\" isn't a procedure")));
    };
    match answer(&sig, &name) {
        Ok(text) => {
            c.push_str(&text);
            Ok(())
        }
        Err(msg) => Err(c.deferrable_err(msg)),
    }
}

/// `info default procname arg varname` — 1 when the formal has a default, with
/// the default stored in `varname`, and 0 when it does not.
fn cmd_default(c: &mut Compiler, args: &[Word]) -> Result<(), CompileError> {
    let [proc_w, arg_w, var_w] = args else {
        return c.error("wrong # args: should be \"info default procname arg varname\"");
    };
    let name = c.literal_of(proc_w, "procedure name")?.to_string();
    let Some(sig) = c.procs.get(&name).cloned() else {
        return Err(c.deferrable_err(format!("\"{name}\" isn't a procedure")));
    };
    let arg = c.literal_of(arg_w, "argument name")?.to_string();
    let Some(param) = sig.params.iter().find(|p| p.name == arg) else {
        // Deferred for the same reason: tclsh looks the formal up when the
        // command runs, so an `info default` in a branch never taken is not an
        // error there.
        return Err(c.deferrable_err(format!(
            "procedure \"{name}\" doesn't have an argument \"{arg}\""
        )));
    };
    let var = c.var_name_of(var_w)?;
    // The variable is written either way: a formal with no default stores the
    // empty string, which is what tclsh does before answering 0.
    let default = param.default.clone();
    c.push_str(default.as_deref().unwrap_or(""));
    c.emit_set_var(&var);
    c.push_value(Value::Int(i64::from(default.is_some())));
    Ok(())
}

/// `info complete script` — whether the text is a whole script, or has a
/// construct still open at its end.
///
/// The same question the REPL asks before deciding whether to keep reading, and
/// the same answer: [`crate::parser::incomplete`] is true only for the parser's
/// five "ran out of text" messages. A script that parses is complete, and so is
/// one that fails for any *other* reason — `info complete "}"` is 1 in tclsh
/// even though the text is not a runnable script.
fn cmd_complete(c: &mut Compiler, args: &[Word]) -> Result<(), CompileError> {
    let [text_w] = args else {
        return c.error("wrong # args: should be \"info complete command\"");
    };
    // A literal is answered while compiling; anything else is a value only the
    // running script has.
    match text_w.as_literal() {
        Some(text) => {
            let complete = !crate::parser::incomplete(text);
            c.push_value(Value::Int(i64::from(complete)));
            Ok(())
        }
        None => {
            c.word(text_w)?;
            c.emit(Op::Extended(ext::INFO_COMPLETE, 0), 0);
            Ok(())
        }
    }
}

/// `info exists varName` — whether the variable is set right now.
fn cmd_exists(c: &mut Compiler, args: &[Word]) -> Result<(), CompileError> {
    let [name_w] = args else {
        return c.error("wrong # args: should be \"info exists varName\"");
    };
    match crate::assoc::target_of(name_w) {
        Some(Target::Scalar(name)) => {
            let place = match c.var_place(&name) {
                Place::Global(idx) => i64::from(idx),
                Place::Slot(slot) => -i64::from(slot) - 1,
            };
            c.emit(Op::LoadInt(place), 1);
            c.emit(Op::Extended(ext::INFO_EXISTS, 0), 0);
            Ok(())
        }
        // An element of an array: `array exists` answers about the array, and
        // this answers about one of its elements, so it takes the array's place
        // and the index rather than a scalar's place.
        Some(Target::Elem { name, index }) => {
            let place = c.array_place(&name);
            c.index_value(&index)?;
            c.emit(Op::LoadInt(place), 1);
            c.emit(Op::Extended(ext::INFO_EXISTS, 1), -1);
            Ok(())
        }
        None => c.error("variable name must be a literal in this phase"),
    }
}

/// `info level` — how many procedure activations are on the stack.
fn cmd_level(c: &mut Compiler, args: &[Word]) -> Result<(), CompileError> {
    if !args.is_empty() {
        // `info level N` answers with the command that entered level N. A call
        // site here pushes the actual arguments and nothing that names the
        // command, so the record does not exist to be read back.
        return c.error(
            "\"info level\" with a level number is not supported: no record of the command \
             that entered a level is kept",
        );
    }
    c.emit(Op::Extended(ext::INFO_LEVEL, 0), 1);
    Ok(())
}

/// `info script ?filename?` — the file the running script came from.
fn cmd_script(c: &mut Compiler, args: &[Word]) -> Result<(), CompileError> {
    if !args.is_empty() {
        return c.error("\"info script\" does not take a filename in this phase");
    }
    c.push_str(&script_name());
    Ok(())
}

/// A subcommand whose answer is fixed for the whole run.
fn constant(c: &mut Compiler, sub: &str, args: &[Word], text: String) -> Result<(), CompileError> {
    if !args.is_empty() {
        return c.error(format!("wrong # args: should be \"info {sub}\""));
    }
    c.push_str(&text);
    Ok(())
}

/// The five subcommands that answer with a list of names: `info commands`,
/// `procs`, `globals`, `locals` and `vars`.
///
/// One shape for all of them — an optional glob pattern over a set of
/// candidates — because that is what they are; `kind` is the only thing that
/// differs, and it says where the set comes from.
fn name_list(
    c: &mut Compiler,
    sub: &str,
    args: &[Word],
    kind: u8,
    candidates: Vec<String>,
) -> Result<(), CompileError> {
    let pattern = match args {
        [] => None,
        [p] => Some(p),
        _ => return c.error(format!("wrong # args: should be \"info {sub} ?pattern?\"")),
    };
    // The candidates ride as one list value rather than as one stack operand
    // each, so the op takes a fixed three and the count is not bounded by an
    // inline operand.
    let joined = crate::list::join(&candidates);
    // For the kind that asks whether each name is set, the candidates carry
    // where each lives alongside.
    let places = place_list(c, kind, &candidates);
    c.push_str(&joined);
    c.push_str(&places);
    match pattern {
        Some(p) => c.word(p)?,
        None => c.push_str("*"),
    }
    c.emit(Op::Extended(ext::INFO_NAMES, kind), -2);
    Ok(())
}

/// Where each candidate lives, as a parallel list, for the kind that has to ask
/// whether the variable is set. Empty for the other two, which do not consult a
/// place.
///
/// The encoding is [`crate::assoc`]'s, the one every `array` and element op
/// takes: a name index in the VM's global table, or a frame slot as
/// `-(slot + 1)`. So a `global`-declared name inside a procedure is asked about
/// where it actually lives.
///
/// A name a *declaration* bound into the frame is not asked about at all: it is
/// listed whether or not the variable it links to is set, because the link is
/// itself the frame entry and it exists from the declaration on. Measured
/// against tclsh 9.0.4: `proc p {} {global zunset; info vars}` answers `zunset`
/// there while `info exists zunset` in the same body answers 0. A local that was
/// never assigned has no entry at all and is not listed, which is the ordinary
/// "is it set" question.
fn place_list(c: &mut Compiler, kind: u8, candidates: &[String]) -> String {
    if kind != names_kind::SET_OF {
        return String::new();
    }
    let bound = declared_names(c);
    let places: Vec<String> = candidates
        .iter()
        .map(|name| {
            if bound.contains(name) {
                // A declared name is listed unconditionally; see the note at
                // the `vars` arm above.
                return ALWAYS.to_string();
            }
            match c.var_place(name) {
                Place::Slot(slot) => (-i64::from(slot) - 1).to_string(),
                Place::Global(idx) => i64::from(idx).to_string(),
            }
        })
        .collect();
    crate::list::join(&places)
}

/// The place operand that means "listed whatever the variable holds".
///
/// `i64::MIN` cannot collide with either half of the encoding: a name index is
/// non-negative and a frame slot is `-(slot + 1)` for a `u16` slot, so the
/// most negative real place is `-65536`.
const ALWAYS: i64 = i64::MIN;

/// The names a declaration bound into the procedure body being compiled —
/// `global`, `variable` and `upvar #0`.
fn declared_names(c: &Compiler) -> std::collections::HashSet<String> {
    let Some(scope) = c.scope.as_ref() else {
        return std::collections::HashSet::new();
    };
    scope
        .globals
        .iter()
        .cloned()
        .chain(scope.aliases.keys().cloned())
        .collect()
}

/// Every name the procedure body being compiled can see: its locals, and the
/// names `global`, `variable` and `upvar #0` bound into its frame.
fn visible_names(c: &Compiler) -> Vec<String> {
    let Some(scope) = c.scope.as_ref() else {
        return Vec::new();
    };
    let mut names = local_names(c);
    names.extend(scope.globals.iter().cloned());
    names.extend(scope.aliases.keys().cloned());
    names.sort();
    names.dedup();
    names
}

/// The names of the locals the compiler has allocated a slot for so far in the
/// procedure body being compiled.
///
/// "So far" is the whole of the answer's precision, and it is the compiler's
/// allocation order rather than a guess: a slot is created the first time a
/// name is lowered, and lowering follows the body's text. A local whose only
/// mention is *after* the `info locals` — reachable only by a loop carrying
/// control backwards over it — is therefore not listed. Recorded in BUGS.md.
fn local_names(c: &Compiler) -> Vec<String> {
    let Some(scope) = c.scope.as_ref() else {
        return Vec::new();
    };
    let mut names: Vec<String> = scope
        .locals
        .keys()
        .filter(|n| !n.starts_with('\u{0}'))
        .cloned()
        .collect();
    names.sort();
    names
}

/// Every command name this frontend answers to, for `info commands`.
///
/// Assembled from the same tables the dispatcher consults, so a command added
/// to one of them is listed here without a second list being maintained: the
/// built-ins, the list commands, the two regular-expression commands, the
/// commands this module's own dispatch block adds, and the script's own
/// procedures and coroutines.
fn command_names(c: &Compiler) -> Vec<String> {
    let mut names: Vec<String> = Compiler::BUILTINS
        .iter()
        .chain(crate::cmd_list::COMMANDS)
        .map(|s| s.to_string())
        .collect();
    names.extend(["regexp".to_string(), "regsub".to_string()]);
    names.extend(c.procs.keys().cloned());
    names.extend(c.coros.iter().cloned());
    names.sort();
    names.dedup();
    names
}

/// The host's name, as `gethostname` reports it.
fn hostname() -> String {
    // 256 is the POSIX ceiling for `HOST_NAME_MAX` plus the terminator, and the
    // call truncates rather than failing, so one buffer answers for every host.
    let mut buf = [0 as libc::c_char; 256];
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr(), buf.len() - 1) };
    if rc != 0 {
        return String::new();
    }
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|b| **b != 0)
        .map(|b| *b as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The path of the running binary, for `info nameofexecutable`.
fn executable() -> String {
    std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

thread_local! {
    /// What `info script` answers. Empty unless a host set it — the library's
    /// `Interp::eval` is handed a string, not a file.
    static SCRIPT: RefCell<String> = const { RefCell::new(String::new()) };
}

/// Tell `info script` which file the script being run came from. The binary
/// calls this before evaluating a script file; a script evaluated from a string
/// keeps the empty answer tclsh gives for `tclsh -c`.
pub fn set_script(path: &str) {
    SCRIPT.with(|s| *s.borrow_mut() = path.to_string());
}

fn script_name() -> String {
    SCRIPT.with(|s| s.borrow().clone())
}

// ── running ──────────────────────────────────────────────────────────────

/// [`ext::INFO_EXISTS`]: `arg` 0 for a scalar at the place on the stack, 1 for
/// an element of the array at that place.
pub(crate) fn exists_op(vm: &mut VM, arg: u8) -> Result<(), String> {
    let place = crate::runtime::place_of_encoded(vm)?;
    let found = if arg == 1 {
        let index = to_tcl_string(&vm.pop());
        crate::assoc::element_is_set(vm, place, &index)
    } else {
        crate::runtime::var_is_set(vm, place)
    };
    vm.push(Value::Int(i64::from(found)));
    Ok(())
}

/// [`ext::INFO_LEVEL`]: how many procedure activations are on the stack.
pub(crate) fn level_op(vm: &mut VM) {
    vm.push(Value::Int(current_level(vm)));
}

/// Tcl's level number for the code that is running: 0 at the script's own level
/// and one more per procedure activation.
///
/// `VM::new` pushes a base frame before the first op runs (`fusevm`'s
/// `vm.rs:445-452`), so the frame count is one ahead of the level throughout —
/// including inside a coroutine, whose VM is built the same way and then given
/// the body's frame, which is level 1 in tclsh too.
pub(crate) fn current_level(vm: &VM) -> i64 {
    vm.frames.len() as i64 - 1
}

/// [`ext::INFO_COMPLETE`] on a value the script computed.
pub(crate) fn complete_op(vm: &mut VM) {
    let text = to_tcl_string(&vm.pop());
    vm.push(Value::Int(i64::from(!crate::parser::incomplete(&text))));
}

/// [`ext::INFO_NAMES`]: `[candidates, slots, pattern]` → the matching names, as
/// a list.
pub(crate) fn names_op(interp: &Shared, vm: &mut VM, kind: u8) -> Result<(), String> {
    let pattern = to_tcl_string(&vm.pop());
    let places = crate::list::split(&to_tcl_string(&vm.pop()))?;
    let candidates = crate::list::split(&to_tcl_string(&vm.pop()))?;

    let mut names: Vec<String> = match kind {
        names_kind::GIVEN => candidates,
        names_kind::SET_OF => candidates
            .iter()
            .zip(places.iter())
            .filter(|(_, place)| {
                let Ok(raw) = place.parse::<i64>() else {
                    return false;
                };
                if raw == ALWAYS {
                    return true;
                }
                let place = if raw < 0 {
                    Place::Slot((-raw - 1) as u16)
                } else {
                    Place::Global(raw as u16)
                };
                crate::runtime::var_is_set(vm, place)
            })
            .map(|(name, _)| name.clone())
            .collect(),
        // The interpreter's map is the authority, not the chunk's slot vector:
        // a variable set by an earlier evaluation is in the map and may not be
        // in this chunk's name table at all. The running chunk's own writes are
        // not in the map yet, so they are read out of the VM.
        _ => crate::runtime::global_names_of(interp, vm),
    };
    names.sort();
    names.dedup();
    names.retain(|name| crate::list::glob_match(&pattern, name));
    vm.push(Value::Str(Arc::new(crate::list::join(&names))));
    Ok(())
}
