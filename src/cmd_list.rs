//! The list commands.
//!
//! Each one lowers to a single frontend extension op: the compiler pushes the
//! command's arguments and emits `Extended(id, argc)`, and [`run`] pops that
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

use std::sync::Arc;

use fusevm::{Op, Value, VM};

use crate::compiler::{ext, CompileError, Compiler};
use crate::list;
use crate::parser::Word;
use crate::runtime::to_tcl_string;

// ── compiling ────────────────────────────────────────────────────────────

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
        "lappend" => return lappend(c, args),
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
fn lappend(c: &mut Compiler, args: &[Word]) -> Result<(), CompileError> {
    let Some((name, values)) = args.split_first() else {
        return c.error("wrong # args: should be \"lappend varName ?value ...?\"");
    };
    let name = c.var_name_of(name)?;
    let count = arg_count(c, values.len() + 1)?;
    let idx = c.b.add_name(&name);
    c.emit(Op::GetVar(idx), 1);
    for value in values {
        c.word(value)?;
    }
    c.emit(Op::Extended(ext::LAPPEND, count), -(values.len() as i32));
    c.emit(Op::Dup, 1);
    c.emit(Op::SetVar(idx), -1);
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
        other => Err(format!("unknown list op {other}")),
    }
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
fn concat(args: &[String]) -> String {
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

    let mut i = 0;
    while i + 2 < args.len() {
        let name = LSEARCH_OPTIONS[option(LSEARCH_OPTIONS, &args[i])?];
        match name {
            "-all" => all = true,
            "-ascii" => data = DataType::Ascii,
            "-exact" => mode = Mode::Exact,
            "-glob" => mode = Mode::Glob,
            "-inline" => inline = true,
            "-integer" => data = DataType::Integer,
            "-not" => negated = true,
            "-real" => data = DataType::Real,
            "-start" => {
                if i + 2 > args.len() - 2 {
                    return Err("missing starting index".to_string());
                }
                i += 1;
                start_text = Some(&args[i]);
            }
            other => return Err(format!("lsearch {other} is not supported yet")),
        }
        i += 1;
    }

    let items = list::split(&args[args.len() - 2])?;
    let pattern = &args[args.len() - 1];

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
    for (i, item) in items.iter().enumerate().skip(start) {
        let mut hit = match (&target, mode) {
            (Some(Compare::Integer(want)), _) => list::wide(item)? == *want,
            (Some(Compare::Real(want)), _) => list::double(item)? == *want,
            (None, Mode::Exact) => item == pattern,
            (None, Mode::Glob) => list::glob_match(pattern, item),
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

    Ok(match (all, inline) {
        (true, true) => {
            let values: Vec<&String> = hits.iter().map(|&i| &items[i]).collect();
            list::join(&values)
        }
        (true, false) => {
            let values: Vec<String> = hits.iter().map(|i| i.to_string()).collect();
            list::join(&values)
        }
        (false, true) => hits.first().map_or(String::new(), |&i| items[i].clone()),
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

fn lsort(args: &[String]) -> Result<String, String> {
    let mut data = DataType::Ascii;
    let mut increasing = true;
    let mut unique = false;
    let mut indices = false;

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
            other => return Err(format!("lsort {other} is not supported yet")),
        }
        i += 1;
    }

    let items = list::split(&args[args.len() - 1])?;
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
        // values rather than copying them.
        _ => {
            let Value::Array(parts) = vm.pop() else {
                return Err(CORRUPT.to_string());
            };
            let Ok([Value::Int(at), total, values]) = <[Value; 3]>::try_from(parts) else {
                return Err(CORRUPT.to_string());
            };
            vm.push(Value::Array(vec![Value::Int(at + 1), total, values]));
            Ok(())
        }
    }
}

const CORRUPT: &str = "corrupt foreach state";

fn borrow_state(value: &Value) -> Result<(i64, i64, &[Value]), String> {
    match value {
        Value::Array(parts) => match parts.as_slice() {
            [Value::Int(at), Value::Int(total), Value::Array(values)] => Ok((*at, *total, values)),
            _ => Err(CORRUPT.to_string()),
        },
        _ => Err(CORRUPT.to_string()),
    }
}
