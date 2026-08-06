//! `info` — interpreter introspection.
//!
//! Every subcommand here answers what tclsh 9.0.4 answers, measured rather than
//! read from the manual. Two carry most of the weight in real scripts:
//!
//! * **`info exists`** is how a script asks about a variable *without* reading
//!   it. That distinction is load-bearing now that an unset read raises
//!   (`can't read "x": no such variable`, through fusevm's undef hook), so this
//!   reaches the variable's place and *peeks* — the same non-growing read
//!   `array exists` uses, because asking whether a variable is set must not
//!   create it.
//! * **`info complete`** is how a REPL knows a command has ended. tclsh answers
//!   0 only for input that is genuinely unterminated — an open brace, bracket or
//!   quote. Input that is *malformed but closed* is complete: `puts }extra`,
//!   `{a}x` and `set x "a"b` are all 1, and so is a trailing backslash. That
//!   maps exactly onto which parse errors mean "more input would help".
//!
//! What a procedure's signature answers — `info args`, `info body`, `info
//! default` — comes from the table the compiler already builds to check arity,
//! published by [`crate::runtime::note_procs`] so a computed name works as well
//! as a literal. The body text rides on the same table: `info body $n` is a
//! computed name too.
//!
//! `locals` and `vars` inside a procedure are answered in two halves, because
//! neither half can answer alone. Which names the frame *has* is a property of
//! the body, known only while compiling — a local is a slot, and a slot's name
//! is not in the built chunk's variable table. Which of them are *set* is a
//! property of the running frame. So the compiler bakes the candidates and their
//! places into the chunk and [`ext::NAMES`] keeps the ones that are set, which is
//! what tclsh answers (measured: `proc p {} {set l 1; info locals}` is `l`, and
//! `proc p {} {info locals}` is empty).
//!
//! `info frame`, TclOO's queries, `constant`/`consts`, `loaded`, `cmdcount`,
//! `cmdtype` and `errorstack` are refused by name — see [`why_refused`] and
//! BUGS.md.

use std::sync::Arc;

use fusevm::{Op, Value, VM};

use crate::assoc::{element_map, place_of};
use crate::compiler::{CompileError, Compiler, Place};
use crate::parser::Word;
use crate::runtime::to_tcl_string;

/// Extension opcode ids owned by this module. The base is declared with every
/// other module's in [`crate::compiler::ext`]; [`crate::runtime`] dispatches by
/// range from the highest base down, so this arm sits above `REGEXP_BASE`.
pub mod ext {
    pub use crate::compiler::ext::INFO_BASE as BASE;
    /// `[place]`, or `[place, index]` when `arg` is 1 — whether the variable is
    /// set, without reading it.
    pub const EXISTS: u16 = BASE;
    /// `[script]` → 1 unless the text is unterminated.
    pub const COMPLETE: u16 = BASE + 1;
    /// `[pattern, given]` → the matching names, sorted. `arg` selects the set:
    /// 0 commands, 1 procs, 2 globals, 3 vars.
    pub const NAMES: u16 = BASE + 2;
    /// `[procName]` → its formal parameters.
    pub const ARGS: u16 = BASE + 3;
    /// `[procName, argName, place]` → 1 when that parameter has a default, which
    /// it stores in the variable; 0 otherwise, storing the empty string.
    pub const DEFAULT: u16 = BASE + 4;
    /// One string about the interpreter; `arg` selects which — see
    /// [`super::describe`].
    pub const ABOUT: u16 = BASE + 5;
    pub const SET_SCRIPT: u16 = BASE + 6;
    /// `info level` → the number of procedure activations on the stack.
    pub const LEVEL: u16 = BASE + 7;
    /// `[procName]` → its body's source text.
    pub const BODY: u16 = BASE + 8;
    /// `expr`'s math functions, for `info functions`. A constant of the build,
    /// but the list lives in [`crate::expr_math`] and is read from there rather
    /// than copied, so it cannot fall behind that table.
    pub const FUNCTIONS: u16 = BASE + 9;
}

/// Which set of names [`ext::NAMES`] reports.
pub(crate) const COMMANDS: u8 = 0;
pub(crate) const PROCS: u8 = 1;
pub(crate) const GLOBALS: u8 = 2;
pub(crate) const VARS: u8 = 3;
/// The candidates the compiler pushed, kept only where the variable each names
/// is set right now — `info locals`, and `info vars` inside a procedure body.
pub(crate) const SET_OF: u8 = 4;

/// The place operand [`SET_OF`] uses for a candidate whose *existence* is not a
/// question: a name `global`, `variable` or `upvar` bound into the frame is
/// visible whether or not it holds anything yet.
///
/// `i64::MIN` cannot collide with a real place: the most negative one a slot
/// encodes is `-65536`, and a link's is that less `Place::LINK_BIAS`.
pub(crate) const ALWAYS: i64 = i64::MIN;

/// Which string [`ext::ABOUT`] reports.
const SCRIPT: u8 = 0;
const EXECUTABLE: u8 = 1;
const HOSTNAME: u8 = 2;
const LIBRARY: u8 = 3;
const SHAREDLIBEXTENSION: u8 = 4;

/// The Tcl language version this frontend implements.
///
/// Not tclrs's own version — `tclrs --version` is that. A script branches on
/// `info tclversion` to decide whether a *language* feature is available, so
/// answering with the crate version would make every such test wrong.
const TCL_VERSION: &str = "9.0";
const TCL_PATCHLEVEL: &str = "9.0.4";

/// Every `info` subcommand tclsh 9.0.4 has, in the order it lists them when it
/// refuses one. Listed whole even where this frontend refuses the subcommand,
/// because the *message* is what a script sees and it should be tclsh's.
pub(crate) const SUBCOMMANDS: &[&str] = &[
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

// ── compiling ────────────────────────────────────────────────────────────

impl Compiler {
    /// `info subcommand ?argument ...?`.
    ///
    /// The subcommand must be a literal, as it is for every ensemble here: the
    /// lowering differs per subcommand, so a computed one has no single shape.
    pub(crate) fn cmd_info(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let Some((head, rest)) = args.split_first() else {
            return self.error("wrong # args: should be \"info subcommand ?arg ...?\"");
        };
        let Some(sub) = head.as_literal() else {
            return self.error("the subcommand of \"info\" must be a literal in this phase");
        };
        let sub = resolve(sub).map_err(|msg| self.err(msg))?;

        match sub {
            "coroutine" => self.info_coroutine(rest),
            "exists" => self.info_exists(rest),
            "complete" => self.info_one(rest, ext::COMPLETE, "info complete command"),
            "args" => self.info_one(rest, ext::ARGS, "info args procname"),
            "body" => self.info_one(rest, ext::BODY, "info body procname"),
            "default" => self.info_default(rest),
            "commands" => self.info_names(rest, COMMANDS, "info commands ?pattern?"),
            "procs" => self.info_names(rest, PROCS, "info procs ?pattern?"),
            "globals" => self.info_names(rest, GLOBALS, "info globals ?pattern?"),
            // Inside a procedure the answer is what the *frame* can see: its
            // locals and the names `global`, `variable` and `upvar` bound into
            // it. At the script's own level it is every variable the interpreter
            // holds, which is `info globals`.
            "vars" if self.scope.is_some() => {
                let visible = self.visible_names();
                self.info_candidates(rest, visible, "info vars ?pattern?")
            }
            "vars" => self.info_names(rest, VARS, "info vars ?pattern?"),
            "locals" => {
                let locals = self.local_names();
                self.info_candidates(rest, locals, "info locals ?pattern?")
            }
            "level" => self.info_level(rest),
            "functions" => self.info_about_list(rest, ext::FUNCTIONS, "info functions ?pattern?"),
            "tclversion" => self.info_literal(rest, TCL_VERSION, "info tclversion"),
            "patchlevel" => self.info_literal(rest, TCL_PATCHLEVEL, "info patchlevel"),
            "script" => match rest {
                [] => self.info_about(rest, SCRIPT, "info script ?filename?"),
                [name] => {
                    self.word(name)?;
                    self.emit(Op::Extended(ext::SET_SCRIPT, 0), 0);
                    Ok(())
                }
                _ => self.error("wrong # args: should be \"info script ?filename?\""),
            },
            "nameofexecutable" => self.info_about(rest, EXECUTABLE, "info nameofexecutable"),
            "hostname" => self.info_about(rest, HOSTNAME, "info hostname"),
            "library" => self.info_about(rest, LIBRARY, "info library"),
            "sharedlibextension" => {
                self.info_about(rest, SHAREDLIBEXTENSION, "info sharedlibextension")
            }
            // Present in the message above, absent here, and refused by name
            // rather than mis-answered.
            other => self.error(format!(
                "info {other} is not supported yet: {}",
                why_refused(other)
            )),
        }
    }

    /// `info exists varName` — the one subcommand that must not read.
    fn info_exists(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let [target] = args else {
            return self.error("wrong # args: should be \"info exists varName\"");
        };
        match self.target_of(target)? {
            crate::assoc::Target::Scalar(name) => {
                let place = self.var_place_operand(&name);
                self.emit(Op::LoadInt(place), 1);
                self.emit(Op::Extended(ext::EXISTS, 0), 0);
            }
            crate::assoc::Target::Elem { name, index } => {
                let place = self.var_place_operand(&name);
                self.emit(Op::LoadInt(place), 1);
                self.index_value(&index)?;
                self.emit(Op::Extended(ext::EXISTS, 1), -1);
            }
        }
        Ok(())
    }

    /// A subcommand taking exactly one word, evaluated and handed to `id`.
    fn info_one(&mut self, args: &[Word], id: u16, usage: &str) -> Result<(), CompileError> {
        let [only] = args else {
            return self.error(format!("wrong # args: should be \"{usage}\""));
        };
        self.word(only)?;
        self.emit(Op::Extended(id, 0), 0);
        Ok(())
    }

    /// `info default procname arg varname`.
    fn info_default(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let [proc_w, arg_w, var_w] = args else {
            return self.error("wrong # args: should be \"info default procname arg varname\"");
        };
        // The default is *written* into the third argument, so it names a
        // variable, and an array element is a variable: `info default p a arr(k)`
        // is what a script inspecting a whole signature writes.
        let target = self.target_of(var_w)?;
        self.word(proc_w)?;
        self.word(arg_w)?;
        match target {
            crate::assoc::Target::Scalar(name) => {
                let place = self.var_place_operand(&name);
                self.emit(Op::LoadInt(place), 1);
                self.emit(Op::Extended(ext::DEFAULT, 0), -2);
            }
            crate::assoc::Target::Elem { name, index } => {
                self.index_value(&index)?;
                let place = self.var_place_operand(&name);
                self.emit(Op::LoadInt(place), 1);
                self.emit(Op::Extended(ext::DEFAULT, 1), -3);
            }
        }
        Ok(())
    }

    /// `info level` — the number of procedure activations on the stack.
    ///
    /// `info level N` answers with the *command* that entered level N. A call
    /// site pushes the actual arguments and nothing that names the command, so
    /// the record does not exist to be read back.
    fn info_level(&mut self, args: &[Word]) -> Result<(), CompileError> {
        if !args.is_empty() {
            return self.error(
                "\"info level\" with a level number is not supported: no record of the command \
                 that entered a level is kept",
            );
        }
        self.emit(Op::Extended(ext::LEVEL, 0), 1);
        Ok(())
    }

    /// `info functions ?pattern?` — a list this build knows, filtered at run
    /// time so the pattern may be computed.
    fn info_about_list(&mut self, args: &[Word], id: u16, usage: &str) -> Result<(), CompileError> {
        match args {
            [] => self.push_str("*"),
            [pattern] => self.word(pattern)?,
            _ => return self.error(format!("wrong # args: should be \"{usage}\"")),
        }
        self.emit(Op::Extended(id, 0), 0);
        Ok(())
    }

    /// The locals of the body being lowered, for `info locals`.
    ///
    /// A local is allocated a slot the first time the body *mentions* it, so a
    /// local whose only mention stands after this `info locals` is not listed —
    /// tclsh lists it only once it is set, which is the same set for every body
    /// that assigns before it asks. Recorded in BUGS.md.
    fn local_names(&self) -> Vec<String> {
        let Some(scope) = self.scope.as_ref() else {
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

    /// Every name the running body can reach: its locals, plus the ones
    /// `global`, `variable` and `upvar` bound into the frame.
    fn visible_names(&self) -> Vec<String> {
        let Some(scope) = self.scope.as_ref() else {
            return Vec::new();
        };
        let mut names = self.local_names();
        names.extend(scope.globals.iter().cloned());
        names.extend(scope.aliases.keys().cloned());
        names.extend(scope.links.keys().cloned());
        names.sort();
        names.dedup();
        names
    }

    /// `info locals ?pattern?` and `info vars ?pattern?` inside a body: the
    /// candidate names and where each lives, then the pattern.
    fn info_candidates(
        &mut self,
        args: &[Word],
        candidates: Vec<String>,
        usage: &str,
    ) -> Result<(), CompileError> {
        let declared = self.declared_names();
        let places: Vec<String> = candidates
            .iter()
            .map(|name| {
                if declared.contains(name) {
                    ALWAYS.to_string()
                } else {
                    self.var_place(name).encode().to_string()
                }
            })
            .collect();
        self.push_str(&crate::list::join(&candidates));
        self.push_str(&crate::list::join(&places));
        match args {
            [] => self.push_str("*"),
            [pattern] => self.word(pattern)?,
            _ => return self.error(format!("wrong # args: should be \"{usage}\"")),
        }
        self.emit(Op::Extended(ext::NAMES, SET_OF), -2);
        Ok(())
    }

    /// The names the body *declared* rather than allocated a slot for. Their
    /// visibility is not a question about a slot's contents.
    fn declared_names(&self) -> std::collections::HashSet<String> {
        let Some(scope) = self.scope.as_ref() else {
            return std::collections::HashSet::new();
        };
        scope
            .globals
            .iter()
            .cloned()
            .chain(scope.aliases.keys().cloned())
            .chain(scope.links.keys().cloned())
            .collect()
    }

    /// `info commands|procs|globals|vars ?pattern?`.
    fn info_names(&mut self, args: &[Word], which: u8, usage: &str) -> Result<(), CompileError> {
        match args {
            [] => {
                self.push_empty();
                self.emit(Op::LoadInt(0), 1);
            }
            [pattern] => {
                self.word(pattern)?;
                self.emit(Op::LoadInt(1), 1);
            }
            _ => return self.error(format!("wrong # args: should be \"{usage}\"")),
        }
        self.emit(Op::Extended(ext::NAMES, which), -1);
        Ok(())
    }

    /// A subcommand whose answer is a constant of this build.
    fn info_literal(&mut self, args: &[Word], text: &str, usage: &str) -> Result<(), CompileError> {
        if !args.is_empty() {
            return self.error(format!("wrong # args: should be \"{usage}\""));
        }
        self.push_str(text);
        Ok(())
    }

    /// A subcommand whose answer the run knows and the compiler does not.
    fn info_about(&mut self, args: &[Word], which: u8, usage: &str) -> Result<(), CompileError> {
        if !args.is_empty() {
            return self.error(format!("wrong # args: should be \"{usage}\""));
        }
        self.emit(Op::Extended(ext::ABOUT, which), 1);
        Ok(())
    }
}

/// Resolve a possibly-abbreviated subcommand, refusing an ambiguous prefix the
/// way the reference interpreter does.
///
/// The list is tclsh's whole set, so the refusal names what tclsh names even for
/// a subcommand this frontend goes on to decline — a script reading the message
/// should not be told the language is smaller than it is.
fn resolve(given: &str) -> Result<&'static str, String> {
    if let Some(exact) = SUBCOMMANDS.iter().find(|s| **s == given) {
        return Ok(exact);
    }
    let hits: Vec<&&str> = SUBCOMMANDS
        .iter()
        .filter(|s| s.starts_with(given))
        .collect();
    match hits.as_slice() {
        [one] => Ok(**one),
        // Worded as the ensemble machinery in `assoc` words it, down to the
        // Oxford comma before the last name, because a script may match on it.
        _ => Err(format!(
            "unknown or ambiguous subcommand \"{given}\": must be {}, or {}",
            SUBCOMMANDS[..SUBCOMMANDS.len() - 1].join(", "),
            SUBCOMMANDS[SUBCOMMANDS.len() - 1]
        )),
    }
}

/// Why a subcommand tclsh has is refused here. Named individually so the
/// message says what is missing rather than that something is.
fn why_refused(sub: &str) -> &'static str {
    match sub {
        "frame" => {
            "it reports on the stack of *commands*, and only the stack of call frames is kept"
        }
        "class" | "object" => "TclOO is not implemented",
        "constant" | "consts" => "constant variables are not implemented",
        "loaded" => "loadable extensions are not implemented",
        "cmdcount" | "cmdtype" | "errorstack" => "the interpreter does not keep it",
        _ => "not built yet",
    }
}

// ── running ──────────────────────────────────────────────────────────────

/// Execute one of this module's ops.
pub(crate) fn extension(vm: &mut VM, id: u16, arg: u8) -> Result<(), String> {
    match id {
        ext::EXISTS => {
            let index = if arg == 1 {
                Some(to_tcl_string(&vm.pop()))
            } else {
                None
            };
            let operand = vm.pop();
            let place = place_of_operand(&operand)?;
            let set = is_set(vm, place, index.as_deref());
            vm.push(Value::Int(i64::from(set)));
            Ok(())
        }
        ext::COMPLETE => {
            let text = to_tcl_string(&vm.pop());
            vm.push(Value::Int(i64::from(is_complete(&text))));
            Ok(())
        }
        // `ext::NAMES` never arrives here: `crate::runtime`'s extension handler
        // takes it, because `info globals` can only be answered from the
        // interpreter's own variable table and this function has just a VM.
        ext::NAMES => Err("info names op reached the module handler".to_string()),
        ext::ARGS => {
            let name = to_tcl_string(&vm.pop());
            let params = crate::runtime::proc_params(vm, &name)
                .ok_or_else(|| format!("\"{name}\" isn't a procedure"))?;
            let names: Vec<String> = params.params.into_iter().map(|(n, _)| n).collect();
            vm.push(Value::Str(Arc::new(crate::list::join(&names))));
            Ok(())
        }
        ext::BODY => {
            let name = to_tcl_string(&vm.pop());
            // A procedure with no recorded body is one whose body the script
            // computed (`proc p {} $b`) or one defined inside a `namespace eval`
            // block, where the signature is prescanned and the text is not. Both
            // report the same thing tclsh reports for a name that is no
            // procedure at all, because from here they are the same absence.
            let body = crate::runtime::proc_body(vm, &name)
                .ok_or_else(|| format!("\"{name}\" isn't a procedure"))?;
            vm.push(Value::Str(Arc::new(body)));
            Ok(())
        }
        ext::LEVEL => {
            vm.push(Value::Int(crate::runtime::current_level(vm)));
            Ok(())
        }
        ext::FUNCTIONS => {
            let pattern = to_tcl_string(&vm.pop());
            let mut names: Vec<String> = crate::expr_math::names()
                .iter()
                .filter(|name| crate::list::glob_match(&pattern, name))
                .map(|name| (*name).to_string())
                .collect();
            names.sort();
            vm.push(Value::Str(Arc::new(crate::list::join(&names))));
            Ok(())
        }
        ext::DEFAULT => {
            let place = place_of(vm);
            let index = (arg == 1).then(|| to_tcl_string(&vm.pop()));
            let arg_name = to_tcl_string(&vm.pop());
            let proc_name = to_tcl_string(&vm.pop());
            let params = crate::runtime::proc_params(vm, &proc_name)
                .ok_or_else(|| format!("\"{proc_name}\" isn't a procedure"))?;
            let found = params
                .params
                .into_iter()
                .find(|(n, _)| *n == arg_name)
                .ok_or_else(|| {
                    format!("procedure \"{proc_name}\" doesn't have an argument \"{arg_name}\"")
                })?;
            let (has, text) = match found.1 {
                Some(d) => (1, d),
                None => (0, String::new()),
            };
            let written = Value::Str(Arc::new(text));
            match index {
                Some(index) => {
                    element_map(vm, place)
                        .ok_or_else(|| format!("can't set \"{index}\": variable isn't array"))?
                        .insert(index, written);
                }
                None => {
                    if let Some(cell) = crate::runtime::var_cell(vm, place) {
                        *cell = written;
                    }
                }
            }
            vm.push(Value::Int(has));
            Ok(())
        }
        ext::SET_SCRIPT => {
            // Returns the name it was given, as tclsh does, so
            // `set old [info script $new]` is a swap.
            let name = to_tcl_string(&vm.pop());
            crate::runtime::note_script(&name);
            vm.push(Value::Str(Arc::new(name)));
            Ok(())
        }
        ext::ABOUT => {
            let text = describe(arg)?;
            vm.push(Value::Str(Arc::new(text)));
            Ok(())
        }
        other => Err(format!("unknown info op {other}")),
    }
}

/// [`place_of`] for an operand already off the stack.
fn place_of_operand(operand: &Value) -> Result<Place, String> {
    match operand {
        Value::Int(raw) => Ok(Place::decode(*raw)),
        other => Err(format!("not a variable place: {other:?}")),
    }
}

/// Whether the variable at `place` — or its element `index` — is set.
///
/// A *peek*, never a read: `Value::Undef` is how an unset variable arrives, and
/// growing the storage to look would make the question create its own answer.
fn is_set(vm: &VM, place: Place, index: Option<&str>) -> bool {
    let Some(index) = index else {
        // The whole variable, which is the same peek every other op that asks
        // makes — including the one that follows an `upvar` link.
        return crate::runtime::var_is_set(vm, place);
    };
    // One element, through the same peek `array exists` and `array names` make —
    // which follows an `upvar` link, so `upvar 1 a b` followed by
    // `info exists b(k)` asks about the caller's array.
    crate::assoc::element_is_set(vm, place, index)
}

/// Whether `text` is a complete command — tclsh's rule, measured.
///
/// 0 only when more input could finish it: an open brace, bracket or quote.
/// Text that is closed but malformed is complete — `puts }extra`, `{a}x` and
/// `set x "a"b` are all 1 in tclsh, and so is a trailing backslash.
fn is_complete(text: &str) -> bool {
    match crate::parser::parse(text) {
        Ok(_) => true,
        Err(e) => !unterminated(&e.msg),
    }
}

/// Whether a parse error means the input simply stopped early.
fn unterminated(msg: &str) -> bool {
    msg.starts_with("missing close-brace")
        || msg.starts_with("missing close-bracket")
        || msg.starts_with("missing \"")
}

/// The one string [`ext::ABOUT`] was asked for.
fn describe(which: u8) -> Result<String, String> {
    Ok(match which {
        SCRIPT => crate::runtime::current_script(),
        EXECUTABLE => std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        HOSTNAME => hostname(),
        // tclsh answers with the directory of its script library, which is
        // whatever `tcl_library` holds; with that variable gone it raises this
        // exact message. tclrs has no script library at all — nothing here reads
        // `init.tcl` — so it is permanently in the state tclsh describes this
        // way, and raising what tclsh raises beats inventing a path that holds
        // nothing or an empty string tclsh never returns.
        // The suffix a loadable library has on this platform. Both engines are
        // asked on the same platform, so this is the language's answer and not
        // the machine's.
        SHAREDLIBEXTENSION => {
            if cfg!(target_os = "macos") {
                ".dylib".to_string()
            } else if cfg!(target_os = "windows") {
                ".dll".to_string()
            } else {
                ".so".to_string()
            }
        }
        _ => return Err("no library has been specified for Tcl".to_string()),
    })
}

fn hostname() -> String {
    // `gethostname` through libc, which is already a dependency: no crate for
    // one call, and `hostname(1)` would be a process per invocation.
    // `c_char` rather than a fixed `i8`: it is signed on x86_64/aarch64-darwin
    // but unsigned on aarch64-linux, so a hardcoded `i8` fails to match
    // `gethostname`'s `*mut c_char` there.
    let mut buf = [0 as libc::c_char; 256];
    // SAFETY: `buf` is 256 bytes and the length passed matches it; the call
    // writes a NUL-terminated name or fails, and a failure leaves `buf` as it
    // was, which is all zeroes.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr(), buf.len() - 1) };
    if rc != 0 {
        return String::new();
    }
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}
