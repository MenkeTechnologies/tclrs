//! `tcl::prefix`: `match`, `all` and `longest` over a table of strings.
//!
//! `Tcl_PrefixMatchObjCmd`, `PrefixAllObjCmd` and `PrefixLongestObjCmd`
//! (`generic/tclIndexObj.c`). An entry is matched the way
//! `Tcl_GetIndexFromObj` matches one — exactly, or by a unique abbreviation,
//! with the empty string never standing for a single entry — which is what the
//! messages `bad option "x": must be a, b, or c` and `ambiguous option "a":
//! must be ab or ac` come from. The pure ops need nothing of the interpreter;
//! only a `match -error` failure raises through [`crate::runtime`]'s
//! `return`-option machinery, so the three run from the interpreter's closure.

use std::sync::Arc;

use fusevm::{Op, Value, VM};

use crate::compiler::{CompileError, Compiler};
use crate::list;
use crate::parser::Word;
use crate::runtime::{to_tcl_string, TclError};

/// Extension opcode ids owned by this module.
pub mod ext {
    pub use crate::compiler::ext::PREFIX_BASE as BASE;
    /// `[arg …]` → the table entry `string` selects.
    pub const MATCH: u16 = BASE;
    /// `[table, string]` → every entry that begins with `string`.
    pub const ALL: u16 = BASE + 1;
    /// `[table, string]` → the longest prefix every such entry shares.
    pub const LONGEST: u16 = BASE + 2;
}

/// The command names this module claims.
pub const COMMANDS: &[&str] = &["tcl::prefix"];

/// The subcommands, in the order the interpreter lists them.
pub const SUBCOMMANDS: &[&str] = &["all", "longest", "match"];

/// The options `match` takes.
const OPTIONS: &[&str] = &["-error", "-exact", "-message"];

/// Whether an id belongs to this module's block.
pub(crate) fn is_op(id: u16) -> bool {
    (ext::BASE..crate::compiler::ext::PREFIX_END).contains(&id)
}

// ── compiling ────────────────────────────────────────────────────────────

pub(crate) fn compile(c: &mut Compiler, args: &[Word]) -> Result<(), CompileError> {
    let Some((first, rest)) = args.split_first() else {
        return c.error("wrong # args: should be \"tcl::prefix subcommand ?arg ...?\"");
    };
    let given = c.literal_of(first, "subcommand")?.to_string();
    let sub = match crate::assoc::resolve(&given, SUBCOMMANDS) {
        Ok(sub) => sub,
        Err(message) => return c.error(message),
    };
    let id = match sub {
        "all" => ext::ALL,
        "longest" => ext::LONGEST,
        _ => ext::MATCH,
    };
    let argc = u8::try_from(rest.len()).map_err(|_| c.err("too many arguments for one command"))?;
    for arg in rest {
        c.word(arg)?;
    }
    c.emit(Op::Extended(id, argc), 1 - rest.len() as i32);
    Ok(())
}

// ── running ──────────────────────────────────────────────────────────────

pub(crate) fn run(vm: &mut VM, id: u16, argc: u8) -> Result<(), TclError> {
    let mut words: Vec<String> = (0..argc).map(|_| to_tcl_string(&vm.pop())).collect();
    words.reverse();
    let result = match id {
        ext::MATCH => prefix_match(&words)?,
        ext::ALL => prefix_all(&words)?,
        _ => prefix_longest(&words)?,
    };
    vm.push(Value::Str(Arc::new(result)));
    Ok(())
}

fn wrong_args(usage: &str) -> TclError {
    TclError::plain(format!("wrong # args: should be \"tcl::prefix {usage}\""))
}

/// `Tcl_GetIndexFromObj` over a table of strings: the index the key selects, or
/// the error it raises, with the `TCL LOOKUP INDEX` code.
fn lookup(key: &str, table: &[String], what: &str, exact: bool) -> Result<usize, TclError> {
    if let Some(at) = table.iter().position(|entry| entry == key) {
        return Ok(at);
    }
    // The empty string abbreviates every entry and so selects none of them
    // alone: with several it is ambiguous, with one it is simply not found.
    let abbreviates: Vec<usize> = (0..table.len())
        .filter(|&i| !exact && table[i].starts_with(key))
        .collect();
    if let [only] = abbreviates.as_slice() {
        if !key.is_empty() {
            return Ok(*only);
        }
    }
    let how = if abbreviates.len() > 1 {
        "ambiguous"
    } else {
        "bad"
    };
    let choices = match table {
        [] => "no valid options".to_string(),
        [one] => format!("must be {one}"),
        [a, b] => format!("must be {a} or {b}"),
        [init @ .., last] => format!("must be {}, or {last}", init.join(", ")),
    };
    Err(TclError {
        errorcode: Some(list::join(&["TCL", "LOOKUP", "INDEX", what, key])),
        ..TclError::plain(format!("{how} {what} \"{key}\": {choices}"))
    })
}

fn prefix_match(args: &[String]) -> Result<String, TclError> {
    let Some(last_option) = args.len().checked_sub(2) else {
        return Err(wrong_args("match ?options? table string"));
    };
    let table_and_string = (&args[last_option], &args[last_option + 1]);
    let option_names: Vec<String> = OPTIONS.iter().map(|o| o.to_string()).collect();
    let (mut exact, mut message, mut error) = (false, "option".to_string(), None);
    let mut i = 0;
    while i < last_option {
        let at = lookup(&args[i], &option_names, "option", false)?;
        match OPTIONS[at] {
            "-exact" => exact = true,
            flag => {
                if i + 1 >= last_option {
                    return Err(TclError {
                        errorcode: Some("TCL OPERATION NOARG".to_string()),
                        ..TclError::plain(format!("missing value for {flag}"))
                    });
                }
                i += 1;
                if flag == "-message" {
                    message = args[i].clone();
                } else {
                    let valid = list::split(&args[i]).is_ok_and(|w| w.len() % 2 == 0);
                    if !valid {
                        return Err(TclError {
                            errorcode: Some("TCL VALUE DICTIONARY".to_string()),
                            ..TclError::plain("error options must have an even number of elements")
                        });
                    }
                    error = Some(args[i].clone());
                }
            }
        }
        i += 1;
    }
    let table = list::split(table_and_string.0).map_err(TclError::plain)?;
    match lookup(table_and_string.1, &table, &message, exact) {
        Ok(at) => Ok(table[at].clone()),
        Err(failure) => match error {
            // An empty option dictionary asks for the failure to be swallowed.
            Some(options) if list::split(&options).is_ok_and(|w| w.is_empty()) => Ok(String::new()),
            // Anything else is `return -options $options -code error`.
            Some(options) => Err(
                match crate::runtime::merge_return_options(
                    &format!("{options} -code error"),
                    failure.msg,
                ) {
                    Ok(raised) | Err(raised) => raised,
                },
            ),
            None => Err(failure),
        },
    }
}

/// The entries of `table` that begin with `string`, in table order.
fn candidates(args: &[String], usage: &str) -> Result<Vec<String>, TclError> {
    let [table, string] = args else {
        return Err(wrong_args(usage));
    };
    Ok(list::split(table)
        .map_err(TclError::plain)?
        .into_iter()
        .filter(|entry| entry.starts_with(string.as_str()))
        .collect())
}

fn prefix_all(args: &[String]) -> Result<String, TclError> {
    Ok(list::join(&candidates(args, "all table string")?))
}

fn prefix_longest(args: &[String]) -> Result<String, TclError> {
    let found = candidates(args, "longest table string")?;
    let Some((first, rest)) = found.split_first() else {
        return Ok(String::new());
    };
    let mut keep = first.len();
    for other in rest {
        let shared = first
            .char_indices()
            .zip(other.chars())
            .take_while(|((_, a), b)| a == b)
            .last()
            .map_or(0, |((at, a), _)| at + a.len_utf8());
        keep = keep.min(shared);
    }
    Ok(first[..keep].to_string())
}
