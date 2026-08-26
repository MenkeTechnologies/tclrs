//! `subst` — the substitution rules of `Tcl(n)` applied to a *value* rather
//! than to source text.
//!
//! Two things make this command different from every other one this frontend
//! lowers, and both are why it is an op rather than codegen.
//!
//! * **Its input is a value.** `subst $x` substitutes text the script never
//!   wrote, so the parse cannot happen while compiling — the reference
//!   interpreter does not do it then either: `TclNRSubstObjCmd` hands the value
//!   to `Tcl_NRSubstObj`, which compiles it *at that moment*
//!   (`generic/tclCompile.c:1283-1294`).
//! * **It reads the calling frame.** A `$name` inside the value is that frame's
//!   variable, and a `[cmd]` inside it runs there. Running either against the
//!   interpreter's globals instead would quietly read and write the wrong
//!   variables inside a procedure, so the whole substitution happens inside the
//!   projection `crate::runtime::in_frame` opens — the same one `uplevel` and
//!   an `eval` in a body run inside.
//!
//! The token semantics are `TclSubstCompile`'s (`generic/tclCompCmdsSZ.c:
//! 1511-1758`), which is what tclsh actually runs for both the literal and the
//! computed form. A command substitution is guarded: an error propagates, a
//! `break` ends the substitution and keeps what it has, a `continue` drops that
//! one substitution's text, and a `return` — or any other code — contributes its
//! result. A plain variable read is *not* guarded, so a missing variable is an
//! error rather than an empty string.

use std::sync::Arc;

use fusevm::{Op, Value, VM};

use crate::compiler::{ext, CompileError, Compiler};
use crate::parser::{SubstFlags, SubstParse, SubstPart, Word};
use crate::runtime::{
    in_frame, levels, read_global, run_source, to_tcl_string, Shared, TclError, TCL_BREAK,
    TCL_CONTINUE, TCL_ERROR,
};

/// `subst ?-nobackslashes? ?-nocommands? ?-novariables? string`.
///
/// Nothing is decided here. Which words are options, whether there is a string
/// at all, and whether the string parses are all questions the reference
/// interpreter answers when the command runs, so every word rides on the stack
/// with the two facts the op cannot recover for itself: the names the enclosing
/// body linked with `global`, and the namespace it was written in.
pub(crate) fn compile(c: &mut Compiler, args: &[Word]) -> Result<(), CompileError> {
    let count = u8::try_from(args.len() + 2)
        .map_err(|_| c.err("too many arguments for \"subst\"".to_string()))?;
    let declared = c.declared_globals().unwrap_or_default();
    c.push_str(&declared);
    let here = c.ns.current.clone();
    c.push_str(&here);
    for arg in args {
        c.word(arg)?;
    }
    c.emit(Op::Extended(ext::SUBST, count), 1 - count as i32);
    Ok(())
}

const USAGE: &str = "wrong # args: should be \
                     \"subst ?-nobackslashes? ?-nocommands? ?-novariables? string\"";

/// Read the option words, as `TclSubstOptions` reads them
/// (`generic/tclCmdMZ.c:3340-3366`): every one clears the substitution it names,
/// and an abbreviation of one resolves the way `Tcl_GetIndexFromObj` resolves it.
fn options(words: &[String]) -> Result<(SubstFlags, &String), TclError> {
    let Some((text, opts)) = words.split_last() else {
        return Err(TclError::plain(USAGE));
    };
    let mut flags = SubstFlags::default();
    for word in opts {
        match resolve(word) {
            Some("-nobackslashes") => flags.backslashes = false,
            Some("-nocommands") => flags.commands = false,
            Some("-novariables") => flags.variables = false,
            _ => {
                return Err(TclError::plain(format!(
                    "bad option \"{word}\": must be -nobackslashes, -nocommands, or -novariables"
                )))
            }
        }
    }
    Ok((flags, text))
}

/// The option a word names, allowing the unambiguous prefixes tclsh allows.
fn resolve(word: &str) -> Option<&'static str> {
    const NAMES: [&str; 3] = ["-nobackslashes", "-nocommands", "-novariables"];
    if word.is_empty() {
        return None;
    }
    let hits: Vec<&'static str> = NAMES
        .iter()
        .copied()
        .filter(|name| name.starts_with(word))
        .collect();
    match hits.as_slice() {
        [only] => Some(only),
        _ => None,
    }
}

/// The `subst` op: `[declared, namespace, arg …]`.
pub(crate) fn subst_op(interp: &Shared, vm: &mut VM, argc: u8) -> Result<(), TclError> {
    let mut args: Vec<String> = (0..argc).map(|_| to_tcl_string(&vm.pop())).collect();
    args.reverse();
    let declared = args.remove(0);
    let here = args.remove(0);
    let (flags, text) = options(&args)?;
    let parse = crate::parser::subst_parts(text, flags);

    // The frame the command was written in, which is the innermost procedure
    // call — not the innermost VM frame, which a scope or a side exit may have
    // pushed inside it. `eval` in a body finds its frame the same way.
    let up = levels(vm).first().copied().unwrap_or(0);
    let ns = Namespace::of(&here);
    let value = in_frame(interp, vm, up, &declared, |interp| {
        substitute(interp, &ns, &parse)
    })?;
    vm.push(Value::Str(Arc::new(value)));
    Ok(())
}

/// Where an unqualified name inside the substituted value is looked for.
///
/// `TclLookupSimpleVar` searches the current namespace and then the global one,
/// and the compiler resolves that search while it reads a script — which it
/// cannot do for a name that is itself a value. The namespace the command was
/// *written* in is the one fact needed to redo the search here, so the compiler
/// pushes it.
struct Namespace {
    prefix: Option<String>,
}

impl Namespace {
    fn of(current: &str) -> Namespace {
        let key = crate::cmd_namespace::store_key(current);
        Namespace {
            prefix: (!key.is_empty()).then(|| format!("{key}::")),
        }
    }

    /// The table keys to try for `name`, in the reference interpreter's order.
    fn keys(&self, name: &str) -> Vec<String> {
        let key = crate::cmd_namespace::store_key(name);
        // A name written `::x` names exactly one variable, and names it as the
        // *interpreter's* rather than as anything a frame could hold — so the
        // prefix is kept, which is what tells a projection in effect not to
        // answer for it. `crate::runtime::read_global` strips it again.
        if name.starts_with("::") {
            return vec![crate::cmd_namespace::chunk_key(name)];
        }
        // A qualified name is already one variable and already carries `::`.
        if key.contains("::") {
            return vec![key.to_string()];
        }
        match &self.prefix {
            Some(prefix) => vec![format!("{prefix}{key}"), key.to_string()],
            None => vec![key.to_string()],
        }
    }
}

/// Substitute one parsed value: [`crate::parser::subst_parts`]' parts, in order,
/// with `TclSubstCompile`'s handling of what each of them can raise.
fn substitute(interp: &Shared, ns: &Namespace, parse: &SubstParse) -> Result<String, TclError> {
    let mut out = String::new();
    for part in &parse.parts {
        // A command substitution, and a variable whose index runs one, are the
        // parts compiled inside a `catch` range; everything else raises straight
        // out of the command.
        let guarded = match part {
            SubstPart::Script(_) => true,
            SubstPart::Elem { index, .. } => {
                index.iter().any(|p| matches!(p, SubstPart::Script(_)))
            }
            _ => false,
        };
        let piece = one(interp, ns, part);
        match piece {
            Ok(text) => out.push_str(&text),
            Err(e) if !guarded => return Err(e),
            Err(e) => match e.visible_code() {
                TCL_ERROR => return Err(e),
                // The substitution ends here and keeps what it has — including
                // when a syntax error was waiting to be reported, which the
                // BREAK arm of the compiled form jumps clean past.
                TCL_BREAK => return Ok(out),
                // This one substitution contributes nothing; the rest still run.
                TCL_CONTINUE => {}
                // `return`, and every code above it, contribute their result.
                _ => out.push_str(&e.msg),
            },
        }
    }
    match &parse.error {
        Some(e) => Err(TclError::plain(e.msg.clone())),
        None => Ok(out),
    }
}

/// One part's text.
fn one(interp: &Shared, ns: &Namespace, part: &SubstPart) -> Result<String, TclError> {
    match part {
        SubstPart::Lit(text) => Ok(text.clone()),
        SubstPart::Var(name) => read(interp, ns, name, None),
        SubstPart::Elem { name, index } => {
            let index = eval_parts(interp, ns, index)?;
            read(interp, ns, name, Some(&index))
        }
        SubstPart::Script(script) => run(interp, script),
    }
}

/// A whole run of parts as one string, with every failure propagated. What an
/// array index inside a substituted value is: a word of its own.
fn eval_parts(interp: &Shared, ns: &Namespace, parts: &[SubstPart]) -> Result<String, TclError> {
    let mut out = String::new();
    for part in parts {
        out.push_str(&one(interp, ns, part)?);
    }
    Ok(out)
}

/// Run a command substitution's script against the projection already in place.
fn run(interp: &Shared, src: &str) -> Result<String, TclError> {
    run_source(interp, src).map(|v| to_tcl_string(&v))
}

/// Read a variable, or an element of one, in the reference interpreter's
/// wording for each way that can fail.
fn read(
    interp: &Shared,
    ns: &Namespace,
    name: &str,
    index: Option<&str>,
) -> Result<String, TclError> {
    let spelled = match index {
        Some(i) => format!("{name}({i})"),
        None => name.to_string(),
    };
    let value = ns
        .keys(name)
        .into_iter()
        .find_map(|key| read_global(interp, &key));
    match (value, index) {
        (None, _) | (Some(Value::Undef), _) => Err(TclError::plain(format!(
            "can't read \"{spelled}\": no such variable"
        ))),
        (Some(Value::Hash(map)), Some(index)) => match map.get(index) {
            Some(v) => Ok(to_tcl_string(v)),
            None => Err(TclError::plain(format!(
                "can't read \"{spelled}\": no such element in array"
            ))),
        },
        (Some(_), Some(_)) => Err(TclError::plain(format!(
            "can't read \"{spelled}\": variable isn't array"
        ))),
        (Some(Value::Hash(_)), None) => Err(TclError::plain(format!(
            "can't read \"{spelled}\": variable is array"
        ))),
        (Some(v), None) => Ok(to_tcl_string(&v)),
    }
}
