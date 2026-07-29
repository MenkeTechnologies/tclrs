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

use std::collections::HashMap;

use fusevm::Op;

use crate::compiler::{ext, CompileError, Compiler, Scope};
use crate::list;
use crate::parser::{Script, Word};

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

impl Compiler {
    /// `proc name args body`.
    pub(crate) fn cmd_proc(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let [name_w, spec_w, body_w] = args else {
            return self.error("wrong # args: should be \"proc name args body\"");
        };
        if !self.top_level {
            // A nested `proc` defines its procedure only when the enclosing
            // code runs, whereas this compiler would register it whether that
            // code is reached or not.
            return self.error("\"proc\" is only supported at the top level of a script");
        }
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
        if !self.defined.insert(name.clone()) {
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
        self.procs.insert(name.clone(), sig.clone());
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
        let name_idx = self.b.add_name(&name);
        self.b.add_sub_entry(name_idx, entry);
        // `proc` itself evaluates to the empty string.
        self.push_empty();
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
