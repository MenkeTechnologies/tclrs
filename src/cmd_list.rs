//! The list commands.
//!
//! Each one lowers to a single frontend extension op: the compiler pushes the
//! command's arguments and emits `Extended(id, argc)`, and `run` pops that
//! many values, computes the result and pushes it. Nothing about a list is
//! resolved at compile time, because the arguments are ordinary words and may
//! be substitutions.
//!
//! `lappend` is the one command that also names a variable, so it reads the
//! variable before the op and writes it back after; everything else is a pure
//! function of its arguments.
//!
//! The semantics are ported from tclsh 9.0.4: `lsort` reproduces the reference
//! merge sort element for element, because with `-unique` the algorithm decides
//! which of two equal elements survives, and `lsearch` reproduces its option
//! parsing, including that the data-type options only bite in `-exact` mode.
//! Options that are not built yet are refused rather than ignored.

use std::cell::RefCell;
use std::sync::Arc;

use fusevm::{Op, Value, VM};

use crate::assoc::Target;
use crate::compiler::{ext, CompileError, Compiler, Place};
use crate::list;
use crate::parser::Word;
use crate::runtime::{place_at, place_of, take_var, to_tcl_string, var_cell};

// ── compiling ────────────────────────────────────────────────────────────

/// The names `compile` accepts. The match below is the authority; this list
/// exists so the REPL can offer the names for completion, and
/// `every_listed_command_compiles` fails if the two ever disagree.
pub const COMMANDS: &[&str] = &[
    "list", "llength", "lindex", "lrange", "lreverse", "linsert", "lreplace", "lsearch", "lsort",
    "join", "split", "concat", "lappend", "lassign", "lset", "lpop", "ledit", "lrepeat", "lremove",
    "lseq", "lmap",
];

/// Compile one of the list commands. Every command name the compiler does not
/// handle itself arrives here, so an unknown one is rejected here too.
pub(crate) fn compile(c: &mut Compiler, name: &str, args: &[Word]) -> Result<(), CompileError> {
    let (id, usage, min, max) = match name {
        "list" => (ext::LIST, "list ?arg ...?", 0, usize::MAX),
        "llength" => (ext::LLENGTH, "llength list", 1, 1),
        "lindex" => (ext::LINDEX, "lindex list ?index ...?", 1, usize::MAX),
        "lrange" => (ext::LRANGE, "lrange list first last", 3, 3),
        "lreverse" => (ext::LREVERSE, "lreverse list", 1, 1),
        "linsert" => (
            ext::LINSERT,
            "linsert list index ?element ...?",
            2,
            usize::MAX,
        ),
        "lreplace" => (
            ext::LREPLACE,
            "lreplace list first last ?element ...?",
            3,
            usize::MAX,
        ),
        "lsearch" => (
            ext::LSEARCH,
            "lsearch ?-option value ...? list pattern",
            2,
            usize::MAX,
        ),
        "lsort" => (ext::LSORT, "lsort ?-option value ...? list", 1, usize::MAX),
        "join" => (ext::JOIN, "join list ?joinString?", 1, 2),
        "split" => (ext::SPLIT, "split string ?splitChars?", 1, 2),
        "concat" => (ext::CONCAT, "concat ?arg ...?", 0, usize::MAX),
        "lrepeat" => (ext::LREPEAT, "lrepeat count ?value ...?", 1, usize::MAX),
        "lremove" => (ext::LREMOVE, "lremove list ?index ...?", 1, usize::MAX),
        // `lseq`'s own argument grammar decides how many arguments are too
        // many, and it reports that at run time as tclsh does, so the bounds
        // here are open rather than a second, earlier answer to the question.
        "lseq" => (ext::LSEQ, "lseq n ??op? n ??by? n??", 0, usize::MAX),
        "lappend" => return lappend(c, args),
        "lassign" => return lassign(c, args),
        "lset" => return lset(c, args),
        "lpop" => return lpop(c, args),
        "ledit" => return ledit(c, args),
        "lmap" => return lmap(c, args),
        other => return c.error(format!("invalid command name \"{other}\"")),
    };

    if args.len() < min || args.len() > max {
        return c.error(format!("wrong # args: should be \"{usage}\""));
    }
    let count = arg_count(c, args.len())?;
    for arg in args {
        c.word(arg)?;
    }
    c.emit(Op::Extended(id, count), 1 - args.len() as i32);
    Ok(())
}

/// `lappend varName ?value ...?`: read, extend, store, and yield the new value.
///
/// The op reaches the variable itself — the compiler pushes where it lives
/// rather than its value — so that [`lappend_at`] can append to the list's own
/// string instead of building a copy of it. A name the script also uses as an
/// array is lowered the read-extend-store way instead, through [`ext::LAPPEND`]:
/// its value is a `Value::Hash`, not a list, and the two paths must not disagree
/// about what that means.
fn lappend(c: &mut Compiler, args: &[Word]) -> Result<(), CompileError> {
    let Some((name, values)) = args.split_first() else {
        return c.error("wrong # args: should be \"lappend varName ?value ...?\"");
    };
    // An array element is the read-extend-store shape below, with the element
    // read and written in place of the variable: `lappend a(i) x` extends one
    // element, which tclsh takes and this compiler used to refuse.
    if let Target::Elem { name, index } = c.target_of(name)? {
        let count = arg_count(c, values.len() + 1)?;
        c.elem_get_tolerant(&name, &index)?;
        for value in values {
            c.word(value)?;
        }
        c.emit(Op::Extended(ext::LAPPEND, count), -(values.len() as i32));
        return c.elem_store(&name, &index);
    }
    let name = c.var_name_of(name)?;
    let count = arg_count(c, values.len() + 1)?;

    if c.is_array(&name) {
        c.emit_get_var(&name);
        for value in values {
            c.word(value)?;
        }
        c.emit(Op::Extended(ext::LAPPEND, count), -(values.len() as i32));
        c.emit(Op::Dup, 1);
        c.emit_set_var(&name);
        return Ok(());
    }

    let place = c.var_place(&name);
    let id = if place.in_frame() {
        ext::LAPPEND_SLOT
    } else {
        ext::LAPPEND_VAR
    };
    c.emit(Op::LoadInt(place.frame_operand()), 1);
    for value in values {
        c.word(value)?;
    }
    c.emit(Op::Extended(id, count), -(values.len() as i32));
    Ok(())
}

/// `lassign list ?varName ...?`: assign each element, yield the remainder.
///
/// The op does not write the variables — it splits the list and pushes the
/// remainder followed by one value per variable in reverse, and a `SetVar` per
/// variable pops them in order. That way the assignment goes through the
/// compiler's own variable path, so a frame slot and a global each behave as
/// they do everywhere else. An array element is refused, by the same
/// `var_name_of` that refuses one to `lappend` and `foreach`.
fn lassign(c: &mut Compiler, args: &[Word]) -> Result<(), CompileError> {
    let Some((list, vars)) = args.split_first() else {
        return c.error("wrong # args: should be \"lassign list ?varName ...?\"");
    };
    // Every name is resolved before anything is emitted, so a bad one is a
    // compile error rather than a half-run command.
    let targets: Vec<Target> = vars
        .iter()
        .map(|w| c.target_of(w))
        .collect::<Result<_, _>>()?;

    c.word(list)?;
    let count = arg_count(c, targets.len())?;
    // Consumes the list, leaves the remainder plus one value per variable.
    c.emit(Op::Extended(ext::LASSIGN, count), targets.len() as i32);
    for target in &targets {
        match target {
            Target::Scalar(name) => c.emit_set_var(name),
            Target::Elem { name, index } => {
                c.elem_store(name, index)?;
                c.emit(Op::Pop, -1);
            }
        }
    }
    Ok(())
}

/// The operands every variable-reaching list op starts with: the name, for the
/// unset-variable message; where the variable lives; and the array element the
/// op works on, empty when the variable is not one.
///
/// The element rides beside the place rather than inside it, because a [`Place`]
/// is a *variable*: `Place::Global(3)` names the whole array, and which element
/// of it is a second question. Keeping them apart is what lets `lset a(i) 0 v`
/// reuse the same read-rewrite-store handler `lset l 0 v` uses.
fn var_target(c: &mut Compiler, target: &Target) -> Result<usize, CompileError> {
    match target {
        Target::Scalar(name) => {
            c.push_str(name);
            let place = c.var_place(name);
            c.push_value(Value::Int(i64::from(place.in_frame())));
            c.emit(Op::LoadInt(place.frame_operand()), 1);
            c.push_str("");
            c.push_value(Value::Int(0));
        }
        Target::Elem { name, index } => {
            // The name in the diagnostic is the one the script wrote, elements
            // and all: tclsh reports `can't read "a(i)": no such variable`.
            let place = c.array_place_of(name);
            c.push_str(&Compiler::elem_report_name(name, index));
            c.push_value(Value::Int(i64::from(place.in_frame())));
            c.emit(Op::LoadInt(place.frame_operand()), 1);
            c.index_value(index)?;
            c.push_value(Value::Int(1));
        }
    }
    Ok(4)
}

/// `lset listVar ?index ...? value`.
fn lset(c: &mut Compiler, args: &[Word]) -> Result<(), CompileError> {
    const USAGE: &str = "wrong # args: should be \"lset listVar ?index? ?index ...? value\"";
    if args.len() < 2 {
        return c.error(USAGE);
    }
    let target = c.target_of(&args[0])?;
    let operands = var_target(c, &target)?;
    for arg in &args[1..] {
        c.word(arg)?;
    }
    let count = arg_count(c, operands + args.len() - 1)?;
    c.emit(
        Op::Extended(ext::LSET, count),
        1 - (operands + args.len() - 1) as i32,
    );
    Ok(())
}

/// `lpop listvar ?index ...?` — the usage string spells the variable lowercase,
/// unlike `lset`'s and `ledit`'s. tclsh's wording, kept as it is.
fn lpop(c: &mut Compiler, args: &[Word]) -> Result<(), CompileError> {
    if args.is_empty() {
        return c.error("wrong # args: should be \"lpop listvar ?index?\"");
    }
    let target = c.target_of(&args[0])?;
    let operands = var_target(c, &target)?;
    for arg in &args[1..] {
        c.word(arg)?;
    }
    let count = arg_count(c, operands + args.len() - 1)?;
    c.emit(
        Op::Extended(ext::LPOP, count),
        1 - (operands + args.len() - 1) as i32,
    );
    Ok(())
}

/// `ledit listVar first last ?element ...?`.
fn ledit(c: &mut Compiler, args: &[Word]) -> Result<(), CompileError> {
    if args.len() < 3 {
        return c.error("wrong # args: should be \"ledit listVar first last ?element ...?\"");
    }
    let target = c.target_of(&args[0])?;
    let operands = var_target(c, &target)?;
    for arg in &args[1..] {
        c.word(arg)?;
    }
    let count = arg_count(c, operands + args.len() - 1)?;
    c.emit(
        Op::Extended(ext::LEDIT, count),
        1 - (operands + args.len() - 1) as i32,
    );
    Ok(())
}

/// `lmap varList list ?varList list ...? command` — `foreach` that collects.
///
/// The same loop state and the same rotated shape, so it keeps whatever
/// `foreach` keeps; the accumulator is the state's fourth element, and the
/// collect sits at the end of the body. `continue` jumps to the step, which is
/// past the collect, which is why a skipped iteration contributes nothing —
/// tclsh's `lmap x {1 2 3} {if {$x==2} continue; set x}` is `1 3`, not
/// `1 {} 3`.
fn lmap(c: &mut Compiler, args: &[Word]) -> Result<(), CompileError> {
    const USAGE: &str = "wrong # args: should be \"lmap varList list ?varList list ...? command\"";
    let Some((body, pairs)) = args.split_last() else {
        return c.error(USAGE);
    };
    if pairs.is_empty() || pairs.len() % 2 != 0 {
        return c.error(USAGE);
    }

    let mut names = Vec::new();
    for pair in pairs.chunks(2) {
        let text = c.literal_of(&pair[0], "lmap variable list")?.to_string();
        let vars = list::split(&text).map_err(|msg| c.err(msg))?;
        if vars.is_empty() {
            return c.error("lmap varlist is empty");
        }
        let width = vars.len();
        names.extend(vars);
        c.push_value(Value::Int(width as i64));
        c.word(&pair[1])?;
    }
    let lists = u8::try_from(pairs.len() / 2)
        .map_err(|_| c.err("too many lists for \"lmap\"".to_string()))?;
    let width = u8::try_from(names.len())
        .map_err(|_| c.err("too many variables for \"lmap\"".to_string()))?;
    c.emit(Op::Extended(ext::LMAP_INIT, lists), 1 - pairs.len() as i32);

    let script = c.body_of(body)?;
    let taken: Vec<String> = names.iter().rev().cloned().collect();
    c.rotated_loop(
        |c| {
            c.emit(Op::Extended(ext::FOREACH_TAKE, width), i32::from(width));
            for name in &taken {
                c.store_named(name)?;
            }
            // The body's value, then straight into the accumulator.
            c.emit_body_value(&script)?;
            c.emit(Op::Extended(ext::LMAP_COLLECT, 0), -1);
            Ok(())
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
    c.emit(Op::Extended(ext::LMAP_RESULT, 0), 0);
    Ok(())
}

/// An extension op carries its operand count in one byte.
fn arg_count(c: &Compiler, len: usize) -> Result<u8, CompileError> {
    u8::try_from(len).map_err(|_| CompileError {
        msg: "too many arguments for a list command".to_string(),
        line: c.line,
    })
}

// ── running ──────────────────────────────────────────────────────────────

/// Execute one of this module's extension ops.
pub(crate) fn run(vm: &mut VM, id: u16, arg: u8) -> Result<(), String> {
    if (ext::FOREACH_INIT..=ext::FOREACH_ADVANCE).contains(&id) {
        return foreach_op(vm, id, arg);
    }
    if id == ext::LAPPEND_VAR || id == ext::LAPPEND_SLOT {
        return lappend_at(vm, id, arg);
    }
    if (ext::LMAP_INIT..=ext::LMAP_RESULT).contains(&id) {
        return lmap_op(vm, id, arg);
    }
    if id == ext::LASSIGN {
        return lassign_op(vm, arg);
    }
    if matches!(id, ext::LSET | ext::LPOP | ext::LEDIT) {
        return list_var_op(vm, id, arg);
    }
    let mut args: Vec<String> = (0..arg).map(|_| to_tcl_string(&vm.pop())).collect();
    args.reverse();
    let result = dispatch(id, &args)?;
    vm.push(Value::Str(Arc::new(result)));
    Ok(())
}

fn dispatch(id: u16, args: &[String]) -> Result<String, String> {
    match id {
        ext::LIST => Ok(list::join(args)),
        ext::LLENGTH => Ok(list::length(&args[0])?.to_string()),
        ext::LINDEX => lindex(&args[0], &args[1..]),
        ext::LAPPEND => lappend_value(&args[0], &args[1..]),
        ext::LRANGE => lrange(&args[0], &args[1], &args[2]),
        ext::LREVERSE => {
            let mut items = list::split(&args[0])?;
            items.reverse();
            Ok(list::join(&items))
        }
        ext::LINSERT => linsert(&args[0], &args[1], &args[2..]),
        ext::LREPLACE => lreplace(&args[0], &args[1], &args[2], &args[3..]),
        ext::LSEARCH => lsearch(args),
        ext::LSORT => lsort(args),
        ext::JOIN => {
            let sep = args.get(1).map(String::as_str).unwrap_or(" ");
            Ok(list::split(&args[0])?.join(sep))
        }
        // The default separators are only these four — not the wider set that
        // separates list elements.
        ext::SPLIT => Ok(split(&args[0], args.get(1).map_or(" \n\t\r", |s| s))),
        ext::CONCAT => Ok(concat(args)),
        ext::LREPEAT => lrepeat(&args[0], &args[1..]),
        ext::LREMOVE => lremove(&args[0], &args[1..]),
        ext::LSEQ => lseq(args),
        other => Err(format!("unknown list op {other}")),
    }
}

/// `lrepeat count ?value ...?`. A count of zero, and no values at all, are both
/// an empty list rather than an error; only a negative count is refused, and in
/// its own wording rather than the integer parser's.
fn lrepeat(count: &str, values: &[String]) -> Result<String, String> {
    let n = list::wide(count).map_err(|_| format!("expected integer but got \"{count}\""))?;
    if n < 0 {
        return Err(format!("bad count \"{count}\": must be integer >= 0"));
    }
    let mut out = Vec::with_capacity(values.len() * n.max(0) as usize);
    for _ in 0..n {
        out.extend(values.iter().cloned());
    }
    Ok(list::join(&out))
}

/// `lremove list ?index ...?`. An index outside the list is not an error — the
/// list comes back unchanged — and repeated or unordered indices remove each
/// element once.
fn lremove(value: &str, indices: &[String]) -> Result<String, String> {
    let items = list::split(value)?;
    let end = items.len() as i64 - 1;
    let mut drop = vec![false; items.len()];
    for text in indices {
        let at = list::index(text, end)?;
        if at >= 0 && at < items.len() as i64 {
            drop[at as usize] = true;
        }
    }
    let kept: Vec<String> = items
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !drop[*i])
        .map(|(_, v)| v)
        .collect();
    Ok(list::join(&kept))
}

/// `lseq n`, `lseq from to`, `lseq from to step`, and the keyword spellings
/// `from to n`, `from .. n`, `from to n by step`, `from count n`.
///
/// The rules are tclsh's and are not what the manual suggests: a step of zero
/// yields one element rather than looping forever, a step pointing away from
/// the end yields none, and with no step at all the direction is inferred, so
/// `lseq 5 1` counts down. Integers stay integers; one float operand makes the
/// whole sequence floats, which is why `lseq 0 1 0.25` starts at `0.0`.
fn lseq(args: &[String]) -> Result<String, String> {
    // The grammar is `from ?op? to ?by step?`, and which error an ill-formed
    // call gets depends on where the parse stops — all of it measured against
    // tclsh rather than read off the usage string.
    const USAGE: &str = "wrong # args: should be \"lseq n ??op? n ??by? n??\"";
    if args.is_empty() {
        return Err(USAGE.to_string());
    }
    let (from, to, step, by_count) = if args.len() == 1 {
        (None, &args[0], None, false)
    } else {
        // `lseq 1 zz 4 by 2`: a trailing `by step` fixes the shape, so the
        // slot before `to` is the operation slot even when what sits there is
        // not a keyword — tclsh then reports it as a number it could not read.
        let anchored = args.len() == 5 && args[3] == "by";
        let (op, to_at) = if is_lseq_op(&args[1]) {
            (Some(args[1].as_str()), 2)
        } else if anchored {
            return Err(format!("expected number but got \"{}\"", args[1]));
        } else {
            (None, 1)
        };
        let Some(to) = args.get(to_at) else {
            return Err(USAGE.to_string());
        };
        let rest = &args[to_at + 1..];
        let step = match rest {
            [] => None,
            // `lseq 1 2 3` is from/to/step; with an operation already given
            // there is no bare step slot left.
            [s] if op.is_none() => Some(s),
            [s] if s == "by" => return Err("missing \"by\" value.".to_string()),
            [_] => return Err(USAGE.to_string()),
            [by, s] if by == "by" => Some(s),
            // A keyword in the `by` slot is a shape error; anything else is
            // named as the operation it failed to be.
            [other, _] if is_lseq_op(other) => return Err(USAGE.to_string()),
            [other, _] => {
                return Err(format!(
                    "bad operation \"{other}\": must be .., to, count, or by"
                ))
            }
            _ => return Err(USAGE.to_string()),
        };
        (Some(&args[0]), to, step, op == Some("count"))
    };

    let number = |t: &str| list::parse_double(t).ok_or(format!("expected number but got \"{t}\""));
    let integral = |t: &str| list::parse_int(t).is_some();

    let (start, count_form) = match from {
        Some(a) => (number(a)?, by_count),
        None => (0.0, false),
    };
    let limit = number(to)?;
    let stride = match step {
        Some(s) => number(s)?,
        None => {
            if count_form || from.is_none() {
                1.0
            } else if limit < start {
                -1.0
            } else {
                1.0
            }
        }
    };

    // A float start or step makes every element a float — `lseq 0 1 0.25`
    // prints `0.0` for a start the script wrote as `0`. A float *count* does
    // not: `lseq 3.0` is `0 1 2` and `lseq 1 count 3.0` is `1 2 3`, because a
    // count is how many, not where. In a range form the end is a place on the
    // same number line, so it counts.
    let counting = count_form || from.is_none();
    let floating = from.is_some_and(|a| !integral(a))
        || step.is_some_and(|s| !integral(s))
        || (!counting && !integral(to));

    let mut out: Vec<String> = Vec::new();
    let mut push = |v: f64| {
        out.push(if floating {
            crate::runtime::format_double(v)
        } else {
            (v as i64).to_string()
        });
    };

    if count_form {
        // `lseq 5 count 3` is three elements from 5. A count of zero is empty.
        let n = limit as i64;
        for i in 0..n.max(0) {
            push(start + stride * i as f64);
        }
        return Ok(list::join(&out));
    }
    if from.is_none() {
        // `lseq 5` is 0..4, and a non-positive n is empty.
        let n = limit as i64;
        for i in 0..n.max(0) {
            push(i as f64);
        }
        return Ok(list::join(&out));
    }
    if stride == 0.0 {
        // Not an error and not a hang: tclsh answers with the start alone.
        push(start);
        return Ok(list::join(&out));
    }

    let mut at = start;
    // A guard rather than a `while` on the value alone, so a step that cannot
    // reach the end stops instead of running away on a rounding error.
    let span = (limit - start) / stride;
    if span < 0.0 {
        return Ok(String::new());
    }
    let iterations = span.floor() as i64;
    for _ in 0..=iterations {
        push(at);
        at += stride;
    }
    Ok(list::join(&out))
}

fn is_lseq_op(text: &str) -> bool {
    matches!(text, ".." | "to" | "count" | "by")
}

/// `lindex list ?index ...?`. With exactly one index argument the argument may
/// itself be a list of indices, so it is tried as a single index first and
/// re-parsed as a list only when that fails.
fn lindex(value: &str, indices: &[String]) -> Result<String, String> {
    if indices.len() == 1 && list::index(&indices[0], i64::MAX - 1).is_err() {
        let path = list::split(&indices[0]).unwrap_or_else(|_| vec![indices[0].clone()]);
        return lindex_flat(value, &path);
    }
    lindex_flat(value, indices)
}

fn lindex_flat(value: &str, indices: &[String]) -> Result<String, String> {
    let mut current = value.to_string();
    for (i, text) in indices.iter().enumerate() {
        let items = list::split(&current)?;
        let at = list::index(text, items.len() as i64 - 1)?;
        if at < 0 || at >= items.len() as i64 {
            // Out of range yields nothing, but the indices that follow still
            // have to be well formed.
            for rest in &indices[i + 1..] {
                list::index(rest, i64::MAX - 1)?;
            }
            return Ok(String::new());
        }
        current = items[at as usize].clone();
    }
    Ok(current)
}

/// The new value of the variable `lappend` was given. With no values to append
/// the variable's own string is returned untouched — only checked for being a
/// list — which is what keeps `lappend x` from rewriting `x`.
fn lappend_value(current: &str, values: &[String]) -> Result<String, String> {
    let mut items = list::split(current)?;
    if values.is_empty() {
        return Ok(current.to_string());
    }
    items.extend(values.iter().cloned());
    Ok(list::join(&items))
}

// ── lappend, in place ────────────────────────────────────────────────────

thread_local! {
    /// The list the last `lappend` produced, kept only so that the next one can
    /// recognise it.
    ///
    /// A string [`list::join`] built is canonical — single spaces between
    /// elements, each quoted exactly as that function quotes it — and appending
    /// to a canonical list is a space plus the new element's own quoting, with
    /// nothing already in it re-derived. Nothing in a string says it is
    /// canonical, so this remembers the value that was, and identity is the
    /// test: a pointer comparison rather than a scan of the whole list.
    ///
    /// Remembering it keeps its allocation alive, so its address cannot be
    /// reused by a different string while it is remembered — the comparison
    /// cannot mistake one list for another. [`forget`] lets go of it before the
    /// append, which is what leaves the string unshared and able to grow in
    /// place.
    static CANONICAL: RefCell<Option<Arc<String>>> = const { RefCell::new(None) };
}

/// `lappend` where the variable is the op's own operand: `[place, value …]`,
/// leaving the new value.
///
/// Reading the variable here rather than through `GetVar` is the whole point:
/// the value is *taken* out of its place, so the list's string is unshared and
/// the elements are appended to it. Read-extend-store cannot do that — the
/// variable still holds the string while the op runs, so every append would
/// copy the whole list, which is what made building one quadratic.
fn lappend_at(vm: &mut VM, id: u16, arg: u8) -> Result<(), String> {
    let mut values: Vec<String> = (1..arg).map(|_| to_tcl_string(&vm.pop())).collect();
    values.reverse();
    let place = place_of(vm, id == ext::LAPPEND_SLOT)?;

    let current = take_var(vm, place);
    let extended = extend(current, &values)?;
    if let Some(cell) = var_cell(vm, place) {
        *cell = Value::Str(Arc::clone(&extended));
    }
    remember(&extended);
    vm.push(Value::Str(extended));
    Ok(())
}

/// The variable's new value. A list this module built and has not lost sight of
/// is extended in place; anything else is re-derived through [`lappend_value`],
/// which is also what refuses a value that is not a well-formed list.
fn extend(current: Value, values: &[String]) -> Result<Arc<String>, String> {
    if let Value::Str(list) = current {
        if forget(&list) {
            return Ok(append_canonical(list, values));
        }
        return Ok(Arc::new(lappend_value(&list, values)?));
    }
    Ok(Arc::new(lappend_value(&to_tcl_string(&current), values)?))
}

fn append_canonical(mut list: Arc<String>, values: &[String]) -> Arc<String> {
    match Arc::get_mut(&mut list) {
        // Unshared: the elements go onto the string the variable held.
        Some(text) => {
            for value in values {
                push_element(text, value);
            }
            list
        }
        // Shared with a value the script kept, which must not change under it,
        // so the append lands on a copy.
        None => {
            let extra: usize = values.iter().map(|value| value.len() + 3).sum();
            let mut text = String::with_capacity(list.len() + extra);
            text.push_str(&list);
            for value in values {
                push_element(&mut text, value);
            }
            Arc::new(text)
        }
    }
}

/// Append one element to a canonical list. Only a list's first element quotes a
/// leading `#`, so the empty list is the case that differs.
fn push_element(out: &mut String, value: &str) {
    if out.is_empty() {
        out.push_str(&list::quote(value, true));
    } else {
        out.push(' ');
        out.push_str(&list::quote(value, false));
    }
}

fn remember(list: &Arc<String>) {
    CANONICAL.with(|canonical| *canonical.borrow_mut() = Some(Arc::clone(list)));
}

/// Whether this is the list the last `lappend` produced — and when it is, let go
/// of it, so the append that follows finds the string unshared.
fn forget(list: &Arc<String>) -> bool {
    CANONICAL.with(|canonical| {
        let mut remembered = canonical.borrow_mut();
        match &*remembered {
            Some(previous) if Arc::ptr_eq(previous, list) => {
                *remembered = None;
                true
            }
            _ => false,
        }
    })
}

fn lrange(value: &str, first: &str, last: &str) -> Result<String, String> {
    let items = list::split(value)?;
    let end = items.len() as i64 - 1;
    let first = list::index(first, end)?.max(0);
    let last = list::index(last, end)?.min(end);
    if first > last {
        return Ok(String::new());
    }
    Ok(list::join(&items[first as usize..=last as usize]))
}

fn linsert(value: &str, index: &str, elements: &[String]) -> Result<String, String> {
    let mut items = list::split(value)?;
    // `end` here means the position after the last element, so inserting there
    // appends.
    let at = list::index(index, items.len() as i64)?.clamp(0, items.len() as i64) as usize;
    items.splice(at..at, elements.iter().cloned());
    Ok(list::join(&items))
}

fn lreplace(value: &str, first: &str, last: &str, elements: &[String]) -> Result<String, String> {
    let mut items = list::split(value)?;
    let len = items.len() as i64;
    let first = list::index(first, len - 1)?.clamp(0, len);
    let last = list::index(last, len - 1)?.min(len - 1);
    let deleted = if first <= last {
        (last - first + 1) as usize
    } else {
        0
    };
    let at = first as usize;
    items.splice(at..at + deleted, elements.iter().cloned());
    Ok(list::join(&items))
}

/// `split string ?splitChars?`: every character of `chars` is a separator, and
/// an empty `chars` makes every character its own element.
fn split(value: &str, chars: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    if chars.is_empty() {
        let items: Vec<String> = value.chars().map(String::from).collect();
        return list::join(&items);
    }
    let items: Vec<String> = value
        .split(|c| chars.contains(c))
        .map(str::to_string)
        .collect();
    list::join(&items)
}

/// `concat`: join the arguments with single spaces after trimming white space
/// from each end, dropping any that trim away to nothing. Trimming stops short
/// of exposing a final backslash, which would escape the separator.
pub(crate) fn concat(args: &[String]) -> String {
    let space = |c: char| c.is_ascii() && list::is_space(c as u8);
    let mut out = String::new();
    let mut emitted = false;
    for arg in args {
        let start = arg.len() - arg.trim_start_matches(space).len();
        let mut end = arg.trim_end_matches(space).len();
        if end <= start {
            continue;
        }
        if end < arg.len() && arg[start..end].ends_with('\\') {
            end += 1;
        }
        if emitted {
            out.push(' ');
        }
        out.push_str(&arg[start..end]);
        emitted = true;
    }
    out
}

// ── lsearch ──────────────────────────────────────────────────────────────

const LSEARCH_OPTIONS: &[&str] = &[
    "-all",
    "-ascii",
    "-bisect",
    "-decreasing",
    "-dictionary",
    "-exact",
    "-glob",
    "-increasing",
    "-index",
    "-inline",
    "-integer",
    "-nocase",
    "-not",
    "-real",
    "-regexp",
    "-sorted",
    "-start",
    "-stride",
    "-subindices",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Exact,
    Glob,
    /// `-regexp`, matched by [`crate::regexp`] rather than by the glob matcher.
    Regexp,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DataType {
    Ascii,
    Integer,
    Real,
}

fn lsearch(args: &[String]) -> Result<String, String> {
    let mut mode = Mode::Glob;
    let mut data = DataType::Ascii;
    let mut all = false;
    let mut inline = false;
    let mut negated = false;
    let mut start_text: Option<&str> = None;
    // Recorded for the diagnostic below; nothing else reads it while `-sorted`
    // and `-bisect` are unimplemented, which is the whole point — the option is
    // accepted because it changes no answer without them.
    let mut increasing = true;
    let mut ordered: Option<&str> = None;
    let mut stride = 1usize;

    let mut i = 0;
    while i + 2 < args.len() {
        let name = LSEARCH_OPTIONS[option(LSEARCH_OPTIONS, &args[i])?];
        match name {
            "-all" => all = true,
            "-ascii" => data = DataType::Ascii,
            "-exact" => mode = Mode::Exact,
            "-glob" => mode = Mode::Glob,
            "-regexp" => mode = Mode::Regexp,
            "-inline" => inline = true,
            "-integer" => data = DataType::Integer,
            "-not" => negated = true,
            "-real" => data = DataType::Real,
            // The sort-order options describe the list `-sorted` and `-bisect`
            // binary-search through, and lsearch(n) gives them no other effect:
            // `lsearch -decreasing {a b c} b` is 1 in tclsh 9.0.4, exactly as
            // the same search without the option is, because the search is
            // linear either way. They are therefore accepted and recorded here,
            // and only the two options that would *use* the order are still
            // refused below.
            "-increasing" => increasing = true,
            "-decreasing" => increasing = false,
            "-start" => {
                if i + 2 > args.len() - 2 {
                    return Err("missing starting index".to_string());
                }
                i += 1;
                start_text = Some(&args[i]);
            }
            // The two options that would read the order. Refused after the loop,
            // not here, so the order they name is the one the whole command
            // settled on rather than whichever flag came first.
            "-sorted" | "-bisect" => ordered = Some(name),
            // `-stride N` makes the list groups of N and searches each group's
            // first element, answering where the *group* starts. The floor is 1
            // here and 2 in `lsort`, with a different wording each — measured:
            // `lsearch -stride 1 {a b} b` is 1, while `lsort -stride 1 {a b}`
            // is "stride length must be at least 2".
            "-stride" => {
                if i + 2 > args.len() - 2 {
                    return Err(
                        "\"-stride\" option must be followed by a stride length".to_string()
                    );
                }
                i += 1;
                let n = list::wide(&args[i])?;
                if n < 1 {
                    return Err("stride length must be at least 1".to_string());
                }
                stride = n as usize;
            }
            other => return Err(format!("lsearch {other} is not supported yet")),
        }
        i += 1;
    }
    if let Some(name) = ordered {
        let order = if increasing {
            "-increasing"
        } else {
            "-decreasing"
        };
        return Err(format!("lsearch {name} {order} is not supported yet"));
    }

    let items = list::split(&args[args.len() - 2])?;
    let pattern = &args[args.len() - 1];
    if stride > 1 && !items.len().is_multiple_of(stride) {
        return Err("list size must be a multiple of the stride length".to_string());
    }

    let mut start = 0usize;
    if let Some(text) = start_text {
        let at = list::index(text, items.len() as i64 - 1)?.max(0);
        if at >= items.len() as i64 {
            return Ok(if all || inline {
                String::new()
            } else {
                "-1".to_string()
            });
        }
        start = at as usize;
    }

    // The data-type options describe how to compare, so they only apply where a
    // comparison happens; the glob matcher works on strings whatever they hold.
    let target = match (mode, data) {
        (Mode::Exact, DataType::Integer) => Some(Compare::Integer(list::wide(pattern)?)),
        (Mode::Exact, DataType::Real) => Some(Compare::Real(list::double(pattern)?)),
        _ => None,
    };

    let mut hits: Vec<usize> = Vec::new();
    for (i, item) in items
        .iter()
        .enumerate()
        .skip(start)
        .filter(|(i, _)| i.is_multiple_of(stride))
    {
        let mut hit = match (&target, mode) {
            (Some(Compare::Integer(want)), _) => list::wide(item)? == *want,
            (Some(Compare::Real(want)), _) => list::double(item)? == *want,
            (None, Mode::Exact) => item == pattern,
            (None, Mode::Glob) => list::glob_match(pattern, item),
            // The regular-expression engine owns this one; a pattern it
            // refuses is `lsearch`'s error too. `-nocase` is not threaded
            // through because `lsearch` does not implement it — that option
            // still reports its own refusal above.
            (None, Mode::Regexp) => crate::regexp::matches_anywhere(pattern, item, false)?,
        };
        if negated {
            hit = !hit;
        }
        if hit {
            hits.push(i);
            if !all {
                break;
            }
        }
    }

    // With a stride, `-inline` answers the whole group rather than the element
    // that matched: `lsearch -stride 2 -inline {a 1 b 2} b` is `b 2`.
    let group_of = |i: usize| -> Vec<String> { items[i..i + stride].to_vec() };

    Ok(match (all, inline) {
        (true, true) => {
            let values: Vec<String> = hits.iter().flat_map(|&i| group_of(i)).collect();
            list::join(&values)
        }
        (true, false) => {
            let values: Vec<String> = hits.iter().map(|i| i.to_string()).collect();
            list::join(&values)
        }
        (false, true) => hits
            .first()
            .map_or(String::new(), |&i| list::join(&group_of(i))),
        (false, false) => hits.first().map_or(-1, |&i| i as i64).to_string(),
    })
}

enum Compare {
    Integer(i64),
    Real(f64),
}

// ── lsort ────────────────────────────────────────────────────────────────

const LSORT_OPTIONS: &[&str] = &[
    "-ascii",
    "-command",
    "-decreasing",
    "-dictionary",
    "-increasing",
    "-index",
    "-indices",
    "-integer",
    "-nocase",
    "-real",
    "-stride",
    "-unique",
];

/// What two elements are compared as.
enum Key {
    Text(String),
    Integer(i64),
    Real(f64),
}

/// One element of the merge sort's intrusive list, as in the reference
/// implementation: `next` indexes back into the same vector.
struct Element {
    key: Key,
    payload: usize,
    next: Option<usize>,
}

/// `lsort -stride N`: the list is groups of `N`, each group moves as a unit, and
/// the key is the group's first element. `-indices` answers the index of each
/// group's first element, which is what the reference interpreter answers.
fn lsort_stride(
    items: &[String],
    stride: usize,
    data: DataType,
    order: Order,
    indices: bool,
) -> Result<String, String> {
    let groups = items.len() / stride;
    let mut keyed: Vec<(Key, usize)> = Vec::with_capacity(groups);
    for g in 0..groups {
        let first = &items[g * stride];
        keyed.push((
            match data {
                DataType::Ascii => Key::Text(first.clone()),
                DataType::Integer => Key::Integer(list::wide(first)?),
                DataType::Real => Key::Real(list::double(first)?),
            },
            g,
        ));
    }
    // A stable sort keeps equal groups in the order they were written, which is
    // what the reference merge sort does with them.
    keyed.sort_by(|a, b| {
        let ord = compare_keys(&a.0, &b.0);
        if order.increasing {
            ord
        } else {
            ord.reverse()
        }
    });

    let mut out = Vec::with_capacity(items.len());
    for (i, (key, g)) in keyed.iter().enumerate() {
        // `-unique` keeps the *last* of a run of equal groups, not the first:
        // `lsort -stride 2 -unique {a 1 a 2 b 3}` is `a 2 b 3`. That is the
        // reference merge's doing — it takes the right operand when two compare
        // equal — and the same rule the element-wise sort in this file follows.
        if order.unique {
            if let Some((next, _)) = keyed.get(i + 1) {
                if compare_keys(key, next).is_eq() {
                    continue;
                }
            }
        }
        // `-indices` answers every index of the group, not the group's first:
        // a stride-2 sort of three groups answers six numbers.
        if indices {
            out.extend((g * stride..(g + 1) * stride).map(|k| k.to_string()));
        } else {
            out.extend_from_slice(&items[g * stride..(g + 1) * stride]);
        }
    }
    Ok(list::join(&out))
}

fn lsort(args: &[String]) -> Result<String, String> {
    let mut data = DataType::Ascii;
    let mut increasing = true;
    let mut unique = false;
    let mut indices = false;

    let mut stride = 1usize;

    let mut i = 0;
    while i + 1 < args.len() {
        let name = LSORT_OPTIONS[option(LSORT_OPTIONS, &args[i])?];
        match name {
            "-ascii" => data = DataType::Ascii,
            "-decreasing" => increasing = false,
            "-increasing" => increasing = true,
            "-indices" => indices = true,
            "-integer" => data = DataType::Integer,
            "-real" => data = DataType::Real,
            "-unique" => unique = true,
            // `-stride N` sorts groups of N as units, keyed on the group's first
            // element, and both refusals are the interpreter's own wording.
            "-stride" => {
                let Some(value) = args.get(i + 1) else {
                    return Err(
                        "\"-stride\" option must be followed by a stride length".to_string()
                    );
                };
                let n = list::wide(value)?;
                if n < 2 {
                    return Err("stride length must be at least 2".to_string());
                }
                stride = n as usize;
                i += 1;
            }
            other => return Err(format!("lsort {other} is not supported yet")),
        }
        i += 1;
    }

    let items = list::split(&args[args.len() - 1])?;
    if stride > 1 {
        if !items.len().is_multiple_of(stride) {
            return Err("list size must be a multiple of the stride length".to_string());
        }
        return lsort_stride(&items, stride, data, Order { increasing, unique }, indices);
    }
    let mut elements = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        elements.push(Element {
            key: match data {
                DataType::Ascii => Key::Text(item.clone()),
                DataType::Integer => Key::Integer(list::wide(item)?),
                DataType::Real => Key::Real(list::double(item)?),
            },
            payload: i,
            next: None,
        });
    }
    if elements.is_empty() {
        return Ok(String::new());
    }

    let order = Order { increasing, unique };
    // The reference sort builds sublists of length 2**j and merges each new
    // element into them; which of two equal elements `-unique` keeps falls out
    // of that shape, so the shape is reproduced rather than replaced with a
    // library sort.
    const RUNS: usize = 30;
    let mut sublists: [Option<usize>; RUNS] = [None; RUNS];
    for i in 0..elements.len() {
        let mut head = Some(i);
        let mut j = 0;
        while j < RUNS && sublists[j].is_some() {
            let left = sublists[j].take();
            head = merge(&mut elements, left, head, order);
            j += 1;
        }
        sublists[j.min(RUNS - 1)] = head;
    }
    let mut head = sublists[0];
    for &run in &sublists[1..] {
        head = merge(&mut elements, run, head, order);
    }

    let mut sorted = Vec::new();
    let mut cursor = head;
    while let Some(i) = cursor {
        sorted.push(if indices {
            elements[i].payload.to_string()
        } else {
            items[elements[i].payload].clone()
        });
        cursor = elements[i].next;
    }
    Ok(list::join(&sorted))
}

#[derive(Clone, Copy)]
struct Order {
    increasing: bool,
    unique: bool,
}

/// Two keys in the reference implementation's order, before `-decreasing` is
/// applied. Shared with the strided sort, which orders whole groups by their
/// first element rather than elements by themselves.
fn compare_keys(a: &Key, b: &Key) -> std::cmp::Ordering {
    match (a, b) {
        (Key::Text(x), Key::Text(y)) => x.cmp(y),
        (Key::Integer(x), Key::Integer(y)) => x.cmp(y),
        (Key::Real(x), Key::Real(y)) => {
            // The reference compares with `(a >= b) - (a <= b)`, which calls
            // any pair involving a NaN equal.
            match (x >= y, x <= y) {
                (true, false) => std::cmp::Ordering::Greater,
                (false, true) => std::cmp::Ordering::Less,
                _ => std::cmp::Ordering::Equal,
            }
        }
        _ => std::cmp::Ordering::Equal,
    }
}

fn compare(elements: &[Element], a: usize, b: usize, order: Order) -> std::cmp::Ordering {
    let ordering = match (&elements[a].key, &elements[b].key) {
        (Key::Text(x), Key::Text(y)) => x.cmp(y),
        (Key::Integer(x), Key::Integer(y)) => x.cmp(y),
        (Key::Real(x), Key::Real(y)) => {
            // The reference compares with `(a >= b) - (a <= b)`, which calls
            // any pair involving a NaN equal.
            match (x >= y, x <= y) {
                (true, false) => std::cmp::Ordering::Greater,
                (false, true) => std::cmp::Ordering::Less,
                _ => std::cmp::Ordering::Equal,
            }
        }
        _ => std::cmp::Ordering::Equal,
    };
    if order.increasing {
        ordering
    } else {
        ordering.reverse()
    }
}

/// Merge two sorted runs. With `-unique`, an element equal to one in the right
/// run is dropped from the left run — and since the left run always holds the
/// earlier elements, that is what makes the *later* of two duplicates survive.
fn merge(
    elements: &mut [Element],
    left: Option<usize>,
    right: Option<usize>,
    order: Order,
) -> Option<usize> {
    let (Some(first_left), Some(first_right)) = (left, right) else {
        return left.or(right);
    };
    let (mut left, mut right) = (left, right);

    let ordering = compare(elements, first_left, first_right, order);
    let head = if ordering.is_gt() || (ordering.is_eq() && order.unique) {
        if ordering.is_eq() {
            left = elements[first_left].next;
        }
        right = elements[first_right].next;
        first_right
    } else {
        left = elements[first_left].next;
        first_left
    };

    let mut tail = head;
    while let (Some(l), Some(r)) = (left, right) {
        let ordering = compare(elements, l, r, order);
        let take_right = if order.unique {
            ordering.is_ge()
        } else {
            ordering.is_gt()
        };
        if take_right {
            if order.unique && ordering.is_eq() {
                left = elements[l].next;
            }
            elements[tail].next = Some(r);
            tail = r;
            right = elements[r].next;
        } else {
            elements[tail].next = Some(l);
            tail = l;
            left = elements[l].next;
        }
    }
    elements[tail].next = left.or(right);
    Some(head)
}

// ── option words ─────────────────────────────────────────────────────────

/// `Tcl_GetIndexFromObj`: an exact match wins, otherwise a unique prefix does,
/// and anything else names the whole table in the error.
fn option(table: &[&str], word: &str) -> Result<usize, String> {
    if let Some(i) = table.iter().position(|&name| name == word) {
        return Ok(i);
    }
    let mut hits = table
        .iter()
        .enumerate()
        .filter(|(_, name)| !word.is_empty() && name.starts_with(word));
    match (hits.next(), hits.next()) {
        (Some((i, _)), None) => Ok(i),
        (Some(_), Some(_)) => Err(format!(
            "ambiguous option \"{word}\": must be {}",
            names(table)
        )),
        _ => Err(format!("bad option \"{word}\": must be {}", names(table))),
    }
}

fn names(table: &[&str]) -> String {
    match table {
        [] => String::new(),
        [only] => only.to_string(),
        [first @ .., last] => format!("{}, or {last}", first.join(", ")),
    }
}

// ── the commands that name a variable ────────────────────────────────────

/// `lassign`: split the list and leave the remainder under one value per
/// variable, in reverse, for the `SetVar`s the compiler emitted after this op.
fn lassign_op(vm: &mut VM, arg: u8) -> Result<(), String> {
    let items = list::split(&to_tcl_string(&vm.pop()))?;
    let wanted = arg as usize;
    let remainder = if items.len() > wanted {
        list::join(&items[wanted..])
    } else {
        String::new()
    };
    vm.push(Value::Str(Arc::new(remainder)));
    // Reverse order: the first variable's `SetVar` runs first and pops last.
    for i in (0..wanted).rev() {
        let value = items.get(i).cloned().unwrap_or_default();
        vm.push(Value::Str(Arc::new(value)));
    }
    Ok(())
}

/// `lset`, `lpop` and `ledit`: read the variable the op was handed, rewrite it,
/// store it back.
///
/// The operands under the arguments are the variable's name and where it lives,
/// the same shape `append` uses — the name only so that an unset variable can
/// be reported by name, which is what tclsh does and what a plain read of the
/// empty string would not.
fn list_var_op(vm: &mut VM, id: u16, arg: u8) -> Result<(), String> {
    let count = arg as usize - 4;
    let mut rest: Vec<String> = (0..count).map(|_| to_tcl_string(&vm.pop())).collect();
    rest.reverse();
    // The four the compiler pushed, innermost last: name, slot flag, place,
    // element index — the last of which is empty unless the variable is one.
    let is_elem = matches!(vm.pop(), Value::Int(1));
    let index = to_tcl_string(&vm.pop());
    let operand = vm.pop();
    let slot_form = matches!(vm.pop(), Value::Int(1));
    let place = place_at(&operand, slot_form)?;
    let name = to_tcl_string(&vm.pop());

    let current = if is_elem {
        take_element(vm, place, &index)
    } else {
        take_var(vm, place)
    };
    if current == Value::Undef {
        return Err(format!("can't read \"{name}\": no such variable"));
    }
    let text = to_tcl_string(&current);

    let (stored, yielded) = match id {
        ext::LSET => {
            let Some((value, indices)) = rest.split_last() else {
                return Err(
                    "wrong # args: should be \"lset listVar ?index? ?index ...? value\""
                        .to_string(),
                );
            };
            let new = lset_value(&text, indices, value)?;
            (new.clone(), new)
        }
        ext::LPOP => {
            let (new, popped) = lpop_value(&text, &rest)?;
            (new, popped)
        }
        _ => {
            let new = ledit_value(&text, &rest[0], &rest[1], &rest[2..])?;
            (new.clone(), new)
        }
    };

    let stored = Value::Str(Arc::new(stored));
    if is_elem {
        if let Some(map) = crate::assoc::elements_of(vm, place) {
            map.insert(index, stored);
        }
    } else if let Some(cell) = var_cell(vm, place) {
        *cell = stored;
    }
    vm.push(Value::Str(Arc::new(yielded)));
    Ok(())
}

/// Take one element out of the array at `place`, leaving it absent — the
/// element-flavoured [`take_var`], so that `lset a(i) …` rewrites the element's
/// own string rather than a copy of it.
fn take_element(vm: &mut VM, place: Place, index: &str) -> Value {
    match crate::assoc::elements_of(vm, place) {
        Some(map) => map.remove(index).unwrap_or(Value::Undef),
        None => Value::Undef,
    }
}

/// `lset`'s replacement, down an index path.
///
/// No index at all — and an empty index list — replaces the whole value, which
/// is why `lset l {} X` is `X` rather than a no-op. An index one past the end
/// appends; two past is `index "N" out of range`, so the growth is by exactly
/// one and nothing wider.
fn lset_value(value: &str, indices: &[String], replacement: &str) -> Result<String, String> {
    // One index argument may itself be a list of indices, as `lindex`'s is.
    let path: Vec<String> = match indices {
        [] => Vec::new(),
        [single] => {
            if list::index(single, i64::MAX - 1).is_err() {
                list::split(single)?
            } else {
                vec![single.clone()]
            }
        }
        many => many.to_vec(),
    };
    if path.is_empty() {
        return Ok(replacement.to_string());
    }
    lset_path(value, &path, replacement)
}

fn lset_path(value: &str, path: &[String], replacement: &str) -> Result<String, String> {
    let Some((first, rest)) = path.split_first() else {
        return Ok(replacement.to_string());
    };
    let mut items = list::split(value)?;
    let end = items.len() as i64 - 1;
    let at = list::index(first, end)?;
    if at < 0 || at > items.len() as i64 {
        return Err(format!("index \"{first}\" out of range"));
    }
    if at == items.len() as i64 {
        // Growing is by one element only, and only at the end.
        if !rest.is_empty() {
            return Err(format!("index \"{first}\" out of range"));
        }
        items.push(replacement.to_string());
        return Ok(list::join(&items));
    }
    let at = at as usize;
    items[at] = if rest.is_empty() {
        replacement.to_string()
    } else {
        lset_path(&items[at], rest, replacement)?
    };
    Ok(list::join(&items))
}

/// `lpop`: the element at the index path, and the list without it.
fn lpop_value(value: &str, indices: &[String]) -> Result<(String, String), String> {
    let path: Vec<String> = if indices.is_empty() {
        vec!["end".to_string()]
    } else {
        indices.to_vec()
    };
    let popped = lindex_flat(value, &path)?;
    let items = list::split(value)?;
    let end = items.len() as i64 - 1;
    // Only the outermost index decides what is removed when the path is deep;
    // a deeper path rewrites that element instead.
    let at = list::index(&path[0], end)?;
    if at < 0 || at >= items.len() as i64 {
        return Err(format!("index \"{}\" out of range", path[0]));
    }
    let at = at as usize;
    let mut items = items;
    if path.len() == 1 {
        items.remove(at);
    } else {
        let inner = lremove_at(&items[at], &path[1..])?;
        items[at] = inner;
    }
    Ok((list::join(&items), popped))
}

/// Remove the element an index path names, for `lpop`'s deep form.
fn lremove_at(value: &str, path: &[String]) -> Result<String, String> {
    let mut items = list::split(value)?;
    let end = items.len() as i64 - 1;
    let at = list::index(&path[0], end)?;
    if at < 0 || at >= items.len() as i64 {
        return Err(format!("index \"{}\" out of range", path[0]));
    }
    let at = at as usize;
    if path.len() == 1 {
        items.remove(at);
    } else {
        items[at] = lremove_at(&items[at], &path[1..])?;
    }
    Ok(list::join(&items))
}

/// `ledit listVar first last ?element ...?` — `lreplace` that writes back.
/// Both ends clamp rather than refusing, so `ledit l 9 9 Z` appends and a
/// reversed range inserts.
fn ledit_value(
    value: &str,
    first: &str,
    last: &str,
    elements: &[String],
) -> Result<String, String> {
    lreplace(value, first, last, elements)
}

// ── foreach ──────────────────────────────────────────────────────────────

/// `foreach`'s loop state, carried on the stack between iterations: the current
/// iteration, the total, and every variable's value for every iteration laid
/// out one iteration after another.
fn foreach_op(vm: &mut VM, id: u16, arg: u8) -> Result<(), String> {
    match id {
        ext::FOREACH_INIT => {
            // Each list arrives as its variable count followed by its text.
            let mut pairs: Vec<(usize, String)> = (0..arg)
                .map(|_| {
                    let text = to_tcl_string(&vm.pop());
                    let vars = to_tcl_string(&vm.pop()).parse::<usize>().unwrap_or(0);
                    (vars, text)
                })
                .collect();
            pairs.reverse();

            let mut lists = Vec::with_capacity(pairs.len());
            let mut iterations = 0usize;
            for (vars, text) in &pairs {
                let items = list::split(text)?;
                iterations = iterations.max(items.len().div_ceil(*vars));
                lists.push(items);
            }

            let mut flat = Vec::new();
            for iteration in 0..iterations {
                for (list_index, (vars, _)) in pairs.iter().enumerate() {
                    for slot in 0..*vars {
                        let at = iteration * vars + slot;
                        let value = lists[list_index].get(at).cloned().unwrap_or_default();
                        flat.push(Value::Str(Arc::new(value)));
                    }
                }
            }
            vm.push(Value::Array(vec![
                Value::Int(0),
                Value::Int(iterations as i64),
                Value::Array(flat),
            ]));
            Ok(())
        }
        // These two read the state where it sits. Popping it would mean
        // duplicating it first, and the state holds every value of every
        // iteration, so a copy per iteration would make the loop quadratic.
        ext::FOREACH_MORE => {
            let (at, total, _) = borrow_state(vm.peek())?;
            vm.push(Value::Bool(at < total));
            Ok(())
        }
        ext::FOREACH_TAKE => {
            let width = arg as usize;
            let (at, _, values) = borrow_state(vm.peek())?;
            let row: Vec<Value> = values[at as usize * width..][..width].to_vec();
            for value in row {
                vm.push(value);
            }
            Ok(())
        }
        // Advancing takes the state apart and puts it back, which moves the
        // values rather than copying them. The iteration counter is the only
        // part it touches, so an `lmap` accumulator on the end rides along
        // untouched rather than needing a step of its own.
        _ => {
            let Value::Array(mut parts) = vm.pop() else {
                return Err(CORRUPT.to_string());
            };
            let Some(Value::Int(at)) = parts.first_mut() else {
                return Err(CORRUPT.to_string());
            };
            *at += 1;
            vm.push(Value::Array(parts));
            Ok(())
        }
    }
}

/// `lmap`'s three steps. The state is `foreach`'s with a fourth element, the
/// accumulator, so `MORE`, `TAKE` and `ADVANCE` are shared — each of those
/// either reads the first three or moves the whole array.
fn lmap_op(vm: &mut VM, id: u16, arg: u8) -> Result<(), String> {
    match id {
        ext::LMAP_INIT => {
            foreach_op(vm, ext::FOREACH_INIT, arg)?;
            let Value::Array(mut parts) = vm.pop() else {
                return Err(CORRUPT.to_string());
            };
            parts.push(Value::Array(Vec::new()));
            vm.push(Value::Array(parts));
            Ok(())
        }
        ext::LMAP_COLLECT => {
            let value = to_tcl_string(&vm.pop());
            let Value::Array(mut parts) = vm.pop() else {
                return Err(CORRUPT.to_string());
            };
            let Some(Value::Array(acc)) = parts.last_mut() else {
                return Err(CORRUPT.to_string());
            };
            acc.push(Value::Str(Arc::new(value)));
            vm.push(Value::Array(parts));
            Ok(())
        }
        _ => {
            let Value::Array(parts) = vm.pop() else {
                return Err(CORRUPT.to_string());
            };
            let Some(Value::Array(acc)) = parts.into_iter().next_back() else {
                return Err(CORRUPT.to_string());
            };
            let items: Vec<String> = acc.iter().map(to_tcl_string).collect();
            vm.push(Value::Str(Arc::new(list::join(&items))));
            Ok(())
        }
    }
}

const CORRUPT: &str = "corrupt foreach state";

/// The three parts every loop state starts with. `lmap`'s carries a fourth —
/// the accumulator — which the trailing `..` lets through, so `MORE` and `TAKE`
/// serve both loops.
fn borrow_state(value: &Value) -> Result<(i64, i64, &[Value]), String> {
    match value {
        Value::Array(parts) => match parts.as_slice() {
            [Value::Int(at), Value::Int(total), Value::Array(values), ..] => {
                Ok((*at, *total, values))
            }
            _ => Err(CORRUPT.to_string()),
        },
        _ => Err(CORRUPT.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::COMMANDS;

    /// [`COMMANDS`] is a second spelling of the match in [`super::compile`], and
    /// the REPL completes from it. Running each listed name must therefore
    /// reach a real command: a name the match does not know answers
    /// `invalid command name`, and nothing else here does. Argument counts are
    /// not the subject — a bare name may well be the wrong number of arguments.
    ///
    /// Asked of a *run* rather than of a compile, because an unknown name is
    /// now what it is to the reference interpreter: an error raised when the
    /// command is reached, not a refusal to read the script (see
    /// `Compiler::defer`). Compiling alone would answer `Ok` for every string
    /// and prove nothing.
    #[test]
    fn every_listed_command_runs_as_a_command() {
        for name in COMMANDS {
            let err = crate::Interp::capturing()
                .eval(name)
                .err()
                .map(|e| e.msg)
                .unwrap_or_default();
            assert!(
                !err.contains("invalid command name"),
                "{name} is listed but the compiler does not know it: {err}"
            );
        }
    }

    /// The other half: a name that is not a command is still refused, so the
    /// test above is not passing because nothing is refused.
    #[test]
    fn an_unlisted_name_is_refused_when_it_runs() {
        let err = crate::Interp::capturing()
            .eval("lnotacommand")
            .err()
            .map(|e| e.msg)
            .unwrap_or_default();
        assert!(err.contains("invalid command name"), "got {err:?}");
    }

    /// And the refusal waits for control to arrive, which is the whole point of
    /// deferring it: tclsh runs this script to completion and so does this one.
    #[test]
    fn an_unlisted_name_in_a_branch_never_taken_is_not_an_error() {
        let outcome = crate::Interp::capturing()
            .eval("if {0} {lnotacommand}\nset x done")
            .expect("a branch never taken cannot fail");
        assert_eq!(outcome, "done");
    }
}
