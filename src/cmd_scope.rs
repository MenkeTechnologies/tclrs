//! `uplevel`, `upvar`, `variable` and `apply` — reaching another scope.
//!
//! # How a level-relative name would reach a frame slot, and what it costs
//!
//! A procedure's variables here are `fusevm` frame slots, and a slot is an
//! *index the compiler assigned*: `Op::GetSlot(3)` says nothing about which name
//! the script wrote, and `Frame { slots: Vec<Value> }` carries no name table.
//! Nothing in a built chunk maps a name onto a slot, and nothing can be added at
//! run time, because by then the mapping is gone.
//!
//! Reaching level *N* by name therefore needs one of two things:
//!
//! 1. **A per-procedure slot-name table, emitted while the body is lowered and
//!    keyed so a live frame can find it.** A frame can be attributed to its
//!    procedure without any new machinery — `Op::Call` records `return_ip`, and
//!    `chunk.ops[return_ip - 1]` is the `Op::Call` that pushed the frame, whose
//!    name index names the callee — so the missing half is only the table, which
//!    the `proc` lowering would have to publish at the point it discards the
//!    scope. That is a change to how procedures are compiled, which is where
//!    this stops: `uplevel` is not allowed to reshape `proc`.
//! 2. **A run-time variable table addressable by name at any level**, which is
//!    what the reference interpreter has and what BUGS.md names as the real fix.
//!    It costs procedure locals their slots — and with them the block and
//!    tracing JIT tiers, which read slots out of a flat `i64` buffer
//!    (`fusevm`'s `refresh_slot_buffers`) and cannot see a hash table.
//!
//! So what is implemented is the half that needs neither: **the level is
//! resolved when the command runs**, against `vm.frames.len()`, and a target
//! that turns out to be the global level is served exactly. That is the
//! distinction BUGS.md said could not be drawn — "whether `uplevel 1` means 'the
//! global table' or 'another procedure's slots' is a run-time property of the
//! call stack" — and resolving it at run time is precisely how it is drawn. A
//! target that is a procedure activation is refused there and then, by number,
//! rather than being served against the wrong variables.
//!
//! What that leaves working, and it is not nothing: `uplevel #0`, which is how
//! a library evaluates at the global level; `uplevel 1` from a procedure the
//! script's own top level called, which is the common one-deep case; and
//! `upvar #0`, the global alias.
//!
//! # `upvar` is a compile-time alias
//!
//! `upvar #0 other local` makes `local` another spelling of the global `other`
//! for the rest of the procedure body. Since the target is the global table, the
//! link needs no run-time indirection at all: the compiler binds the name, and
//! every read, write, `unset` and `info exists` of `local` lowers to the ops the
//! global `other` would have got — through `Compiler::var_place`, which every
//! command in this crate reaches a variable through.
//!
//! The price is that the level and both names have to be literals, because the
//! binding is made while the script is read. `upvar #0 $win data` — Tk's own
//! idiom — is refused rather than approximated.

use std::collections::HashMap;

use fusevm::{Op, VM};

use crate::compiler::{ext, CompileError, Compiler, Scope};
use crate::parser::Word;
use crate::procs::{parse_signature, Signature};
use crate::runtime::{to_tcl_string, Shared, TclError};

// ── compiling ────────────────────────────────────────────────────────────

impl Compiler {
    /// `uplevel ?level? arg ?arg ...?`.
    ///
    /// Everything is decided when the command runs — which level the first word
    /// names, whether that level is the global one, and what script the
    /// remaining words concatenate to — because all three are properties of the
    /// call stack and of values the script computed.
    pub(crate) fn cmd_uplevel(&mut self, args: &[Word]) -> Result<(), CompileError> {
        if args.is_empty() {
            return self.error("wrong # args: should be \"uplevel ?level? command ?arg ...?\"");
        }
        let count = u8::try_from(args.len())
            .map_err(|_| self.err("too many arguments for \"uplevel\"".to_string()))?;
        for arg in args {
            self.word(arg)?;
        }
        self.emit(Op::Extended(ext::UPLEVEL, count), 1 - args.len() as i32);
        Ok(())
    }

    /// `upvar ?level? otherVar localVar ?otherVar localVar ...?`.
    pub(crate) fn cmd_upvar(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let mut rest = args;
        // A first word that names a level is consumed; anything else is the
        // first `otherVar`, which is how `upvar x y` works with no level.
        if let Some(text) = args.first().and_then(|w| w.as_literal()) {
            if !args.len().is_multiple_of(2) && parse_level(text).is_some() {
                if text != "#0" {
                    return self.error(format!(
                        "\"upvar {text}\" is not supported: only \"#0\" — a link to a global — \
                         can be bound while the script is read, and a link to a procedure's \
                         frame slots cannot be expressed at all"
                    ));
                }
                rest = &args[1..];
            }
        }
        if rest.is_empty() || !rest.len().is_multiple_of(2) {
            return self.error(
                "wrong # args: should be \"upvar ?level? otherVar localVar ?otherVar localVar ...?\"",
            );
        }
        if rest.len() == args.len() {
            // No level word: the default is 1, which is the caller's frame.
            return self.error(
                "\"upvar\" with no level is not supported: the default level 1 is the caller's \
                 frame, whose variables are slots this frontend cannot address by name",
            );
        }
        if self.scope.is_none() {
            return self.error(
                "\"upvar\" outside a procedure is not supported: there is no local scope for the \
                 link to live in",
            );
        }
        for pair in rest.chunks(2) {
            let other = self.var_name_of(&pair[0])?;
            let local = self.var_name_of(&pair[1])?;
            self.bind_alias(&local, &other)?;
        }
        self.push_empty();
        Ok(())
    }

    /// `variable ?name value ...? name ?value?`.
    ///
    /// With no namespaces in this frontend every namespace variable is a global,
    /// so inside a procedure this is `global` with an optional initial value, and
    /// at the script's own level it is an assignment. Both match tclsh 9.0.4 in
    /// the `::` namespace, which is the only one a script here is ever in:
    /// `variable v 7` sets the global and `variable v` alone leaves it unset.
    pub(crate) fn cmd_variable(&mut self, args: &[Word]) -> Result<(), CompileError> {
        // `variable` with no arguments is legal and does nothing — measured
        // against tclsh 9.0.4, which answers with the empty string.
        let mut i = 0;
        while i < args.len() {
            let name = self.var_name_of(&args[i])?;
            if let Some(scope) = self.scope.as_mut() {
                if scope.locals.contains_key(&name) {
                    return self.error(format!("variable \"{name}\" already exists"));
                }
                scope.globals.insert(name.clone());
            }
            if let Some(value) = args.get(i + 1) {
                self.word(value)?;
                self.emit_set_var(&name);
            }
            i += 2;
        }
        self.push_empty();
        Ok(())
    }

    /// `apply lambda ?arg ...?`.
    ///
    /// A lambda whose text is written out is compiled exactly as a procedure
    /// body is: its own frame, its own slots, entered by `Op::Call`. So an
    /// applied lambda costs a call and nothing else — its locals are frame slots
    /// like any procedure's, and a loop inside one is as JIT-eligible as a loop
    /// inside a `proc`.
    ///
    /// A lambda that is a *value* is refused. Its body would have to be compiled
    /// when the command runs, as a chunk of its own, and a chunk of its own
    /// reaches only globals — so its parameters could not be its own. That is
    /// the same wall `eval` inside a procedure hits, and it is refused for the
    /// same reason rather than run against the wrong variables.
    pub(crate) fn cmd_apply(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let Some((lambda_w, actuals)) = args.split_first() else {
            return self.error("wrong # args: should be \"apply lambdaExpr ?arg ...?\"");
        };
        let Some(text) = lambda_w.as_literal() else {
            return self.error(
                "\"apply\" of a computed lambda is not supported: the body would be compiled as \
                 a chunk of its own, which cannot reach frame slots",
            );
        };
        let text = text.to_string();
        // A lambda that is not a two- or three-element list is reported by
        // tclsh when `apply` runs, not while the script is read, so the refusal
        // is marked deferrable and becomes a raise standing where the command's
        // code would have stood.
        let parts = crate::list::split(&text).map_err(|_| {
            self.deferrable_err(format!("can't interpret \"{text}\" as a lambda expression"))
        })?;
        let (spec, body) = match parts.as_slice() {
            [spec, body] => (spec, body),
            // The third element names the namespace the body runs in. `::` is
            // the only namespace here, so it is accepted and any other is not.
            [spec, body, ns] if ns == "::" || ns.is_empty() => (spec, body),
            [_, _, ns] => {
                return self.error(format!(
                    "\"apply\" into the namespace \"{ns}\" is not supported: this frontend has \
                     only the global namespace"
                ))
            }
            _ => {
                return Err(self
                    .deferrable_err(format!("can't interpret \"{text}\" as a lambda expression")))
            }
        };
        let name = format!("\u{0}apply\u{0}{}", self.b.current_pos());
        let sig = match parse_signature(&name, spec) {
            Ok(sig) => sig,
            Err(msg) => return self.error(msg),
        };
        self.emit_lambda(&name, &sig, body)?;
        self.procs.insert(name.clone(), sig);
        // The call site adapts to the signature exactly as a `proc` call does:
        // defaults are pushed here and surplus actuals collected into `args`.
        self.call_proc(&name, actuals)
    }

    /// Emit a lambda body as a sub of the enclosing chunk, behind a jump that
    /// steps over it — the shape `crate::procs::cmd_proc` gives a procedure, so
    /// an applied lambda is entered by the same `Op::Call` a procedure is.
    fn emit_lambda(&mut self, name: &str, sig: &Signature, body: &str) -> Result<(), CompileError> {
        let slots = u8::try_from(sig.params.len())
            .map_err(|_| self.err("a lambda with more than 255 formal parameters".to_string()))?
            as usize;
        let script = crate::parser::parse(body).map_err(|e| self.deferrable_err(e.msg))?;

        let skip = self.emit(Op::Jump(usize::MAX), 0);
        let entry = self.b.current_pos();

        let outer_depth = std::mem::replace(&mut self.depth, slots);
        let outer_loops = std::mem::take(&mut self.loops);
        let outer_catch = std::mem::replace(&mut self.catch_depth, 0);
        let outer_scope = self.scope.replace(lambda_scope(sig));
        let outer_top = std::mem::replace(&mut self.top_level, false);
        let outer_static = std::mem::replace(&mut self.static_ctx, false);

        for slot in (0..slots).rev() {
            self.emit(Op::SetSlot(slot as u16), -1);
        }
        let compiled = self.script_value(&script);
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
        let name_idx = self.b.add_name(name);
        self.b.add_sub_entry(name_idx, entry);
        Ok(())
    }

    /// Bind `local` to the global `other` for the rest of the procedure body.
    fn bind_alias(&mut self, local: &str, other: &str) -> Result<(), CompileError> {
        let scope = self.scope.as_mut().expect("checked by the caller");
        if scope.locals.contains_key(local) {
            return Err(CompileError {
                msg: format!("variable \"{local}\" already exists"),
                line: self.command_line,
            });
        }
        scope.aliases.insert(local.to_string(), other.to_string());
        Ok(())
    }
}

/// The slot scope a lambda body starts with: one slot per formal parameter, in
/// declaration order, matching the prologue emitted above.
fn lambda_scope(sig: &Signature) -> Scope {
    let mut scope = Scope::default();
    for (i, p) in sig.params.iter().enumerate() {
        scope.locals.insert(p.name.clone(), i as u16);
    }
    scope.next_slot = sig.params.len() as u16;
    scope
}

/// What a level word names, if it names one at all.
///
/// `TclObjGetFrame` (`generic/tclBasic.c`): `#N` is an absolute level and a bare
/// integer is a number of levels *up* from the current one. Anything else is not
/// a level, which is what makes `uplevel {set x 1}` a script and not a level
/// followed by nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Absolute(i64),
    Up(i64),
}

fn parse_level(text: &str) -> Option<Level> {
    if let Some(digits) = text.strip_prefix('#') {
        return digits.parse::<i64>().ok().map(Level::Absolute);
    }
    text.parse::<i64>().ok().map(Level::Up)
}

// ── running ──────────────────────────────────────────────────────────────

/// `uplevel` ([`ext::UPLEVEL`]): `[arg …]` with the count in the inline operand.
pub(crate) fn uplevel_op(interp: &Shared, vm: &mut VM, argc: u8) -> Result<(), TclError> {
    let mut args: Vec<String> = (0..argc).map(|_| to_tcl_string(&vm.pop())).collect();
    args.reverse();

    // Level 0 is the script's own and each procedure activation is one more;
    // `crate::cmd_info::current_level` is the same count `info level` answers
    // with, so `uplevel` and `info level` cannot disagree about where the code
    // calling them is.
    let current = crate::cmd_info::current_level(vm);
    let explicit = args.first().and_then(|w| parse_level(w));
    // A level word is only a level when a script follows it: `uplevel {set x 1}`
    // is a script, and so is `uplevel 1` on its own — which is why tclsh reports
    // the argument count for the latter rather than a missing script.
    let has_script = args.len() > 1;
    let (target, script_from) = match explicit {
        Some(Level::Absolute(n)) if has_script => (n, 1),
        Some(Level::Up(n)) if has_script => (current - n, 1),
        _ => (current - 1, 0),
    };
    if script_from >= args.len() {
        return Err(TclError::plain(
            "wrong # args: should be \"uplevel ?level? command ?arg ...?\"",
        ));
    }
    if target < 0 || target > current {
        // The level tclsh quotes is the word the script wrote, or `1` when the
        // level was left out and defaulted.
        let named = if script_from == 1 {
            args[0].clone()
        } else {
            "1".to_string()
        };
        return Err(TclError::plain(format!("bad level \"{named}\"")));
    }
    if target > 0 {
        return Err(TclError::plain(format!(
            "\"uplevel\" to level {target} is not supported: that level is a procedure \
             activation, and a procedure's variables are frame slots no name reaches once the \
             chunk is built"
        )));
    }

    let rest = &args[script_from..];
    let src = match rest {
        [one] => one.clone(),
        many => crate::cmd_list::concat(many),
    };
    crate::runtime::flush_globals(vm, interp);
    let result = crate::runtime::run_source(interp, &src);
    crate::runtime::reseed_globals(vm, interp);
    vm.push(result?);
    Ok(())
}

/// The aliases a scope carries, for [`Compiler::var_place`] to consult. A
/// separate type so the map's meaning — a *local* name bound to a *global* one —
/// is stated where it is declared rather than at every use.
pub(crate) type Aliases = HashMap<String, String>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_level_word_is_told_apart_from_a_script() {
        assert_eq!(parse_level("#0"), Some(Level::Absolute(0)));
        assert_eq!(parse_level("#3"), Some(Level::Absolute(3)));
        assert_eq!(parse_level("1"), Some(Level::Up(1)));
        assert_eq!(parse_level("-1"), Some(Level::Up(-1)));
        // A script is not a level, however it is spelled.
        assert_eq!(parse_level("set x 1"), None);
        assert_eq!(parse_level("#"), None);
        assert_eq!(parse_level(""), None);
    }

    #[test]
    fn a_lambda_scope_matches_the_prologue_it_is_emitted_with() {
        let sig = parse_signature("l", "a {b 2} args").expect("a valid signature");
        let scope = lambda_scope(&sig);
        assert_eq!(scope.locals.get("a"), Some(&0));
        assert_eq!(scope.locals.get("b"), Some(&1));
        assert_eq!(scope.locals.get("args"), Some(&2));
        assert_eq!(scope.next_slot, 3);
    }

    /// A parameter list the lambda shares with `proc`, so the two cannot drift.
    #[test]
    fn a_lambda_uses_the_procedure_signature_parser() {
        let sig: Signature = parse_signature("l", "x").expect("a valid signature");
        assert_eq!(sig.params.len(), 1);
        let first = &sig.params[0];
        assert_eq!(first.name, "x");
        assert_eq!(first.default, None);
    }
}
