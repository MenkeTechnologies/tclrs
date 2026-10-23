//! `tcl::mathop` — the operators of `expr` as commands (TIP #174, `mathop(n)`).
//!
//! `::tcl::mathop::+ 1 2 3` is 6, `::tcl::mathop::< 1 2 3` is 1, and
//! `[::tcl::mathop::* {*}$list]` is the product of a list. tclsh registers the
//! twenty-seven of them from the `mathOpCmds` table in `generic/tclBasic.c`,
//! each with one of four argument shapes (`generic/tclCompExpr.c`):
//!
//! * `TclSingleOpCmd` — `~ ! << >> % != ne in ni` take exactly one or exactly
//!   two arguments, and refuse any other count with the operand names the table
//!   gives (`wrong # args: should be "<< integer shift"`).
//! * `TclVariadicOpCmd` — `+ * & | ^ **` take any number. None is the
//!   operator's identity (0, 1, -1, 0, 0, 1); one is the identity combined with
//!   the argument, which is what makes `+ 0x10` answer 16 and `** 007` keep its
//!   spelling; more are folded from the left, except `**`, which groups from the
//!   right: `** 2 3 2` is 2 to the 9th.
//! * `TclNoIdentOpCmd` — `- /` are the same fold without the empty case:
//!   `- 5` is -5, `/ 2` is 0.5 (one over a double), and no argument at all is
//!   `wrong # args`.
//! * `TclSortingOpCmd` — `< <= > >= == eq lt le gt ge` AND the operator over
//!   every adjacent pair, so `< 1 2 3` asks whether the arguments are sorted.
//!   Fewer than two is 1. The AND short-circuits as `expr`'s does, so an
//!   operand past the pair that decided the answer is never looked at:
//!   `< 2 1 x` is 0, not a refusal of `x`.
//!
//! tclsh evaluates each command by building the expression tree the arguments
//! spell and running it through the `expr` engine, with the arguments as the
//! leaves. The arithmetic here is the same engine's: every pairwise step is one
//! of [`crate::runtime`]'s `expr` operations — the numeric hook for `+ - *` and
//! the comparisons, the extension ops for the rest — so an operand is refused in
//! `expr`'s words (`cannot use non-numeric string "abc" as left operand of
//! "+"`), an integer promotes past `i64` rather than overflowing, and a double
//! anywhere makes the answer a double, exactly as inside `expr {…}`.
//!
//! One op. The command's words are pushed in order, then their count, then
//! [`ext::APPLY`] with the operator's index in [`TABLE`] as its inline operand.
//! The count travels as a value rather than in the byte operand so that
//! `+ {*}[lseq 1 1000]` — a thousand words, well past what a byte holds — is
//! one command, as it is in tclsh. The words are all evaluated before any
//! arithmetic runs, which is Tcl's rule for every command: `+ abc [puts hi]`
//! prints `hi` and then refuses `abc`.
//!
//! The names are accepted with and without the leading `::` — `tcl::mathop::+`
//! and `::tcl::mathop::+` are the same command in tclsh — and
//! `namespace import ::tcl::mathop::*` brings the bare operators in
//! (`crate::cmd_namespace`), since the namespace exports every name it holds
//! (`Tcl_Export(interp, nsPtr, "*", 1)`).

use fusevm::{Op, Value, VM};

use crate::compiler::{CompileError, Compiler};
use crate::parser::Word;
use crate::runtime::{mathop_binary, mathop_unary, tcl_bool};

/// Extension opcode ids owned by this module.
pub mod ext {
    pub use crate::compiler::ext::MATHOP_BASE as BASE;
    /// `[value …, count]` → the operator `arg` indexes in the table, applied to
    /// the `count` values.
    pub const APPLY: u16 = BASE;
}

/// The namespace every operator lives in, without the leading `::`.
const PREFIX: &str = "tcl::mathop::";

/// How an operator takes its arguments: the four `objProc`s of `mathOpCmds`.
#[derive(Debug, Clone, Copy)]
enum Shape {
    /// Exactly this many arguments, named this way in the `wrong # args`
    /// message.
    Single(usize, &'static str),
    /// Any number of arguments, folded from this identity.
    Variadic(i64),
    /// At least one argument, folded from the left; `-` and `/`.
    NoIdent,
    /// The AND of the operator over every adjacent pair.
    Sorting,
}

/// The operators, in the order `mathOpCmds` registers them.
const TABLE: &[(&str, Shape)] = &[
    ("~", Shape::Single(1, "integer")),
    ("!", Shape::Single(1, "boolean")),
    ("+", Shape::Variadic(0)),
    ("*", Shape::Variadic(1)),
    ("&", Shape::Variadic(-1)),
    ("|", Shape::Variadic(0)),
    ("^", Shape::Variadic(0)),
    ("**", Shape::Variadic(1)),
    ("<<", Shape::Single(2, "integer shift")),
    (">>", Shape::Single(2, "integer shift")),
    ("%", Shape::Single(2, "integer integer")),
    ("!=", Shape::Single(2, "value value")),
    ("ne", Shape::Single(2, "value value")),
    ("in", Shape::Single(2, "value list")),
    ("ni", Shape::Single(2, "value list")),
    ("-", Shape::NoIdent),
    ("/", Shape::NoIdent),
    ("<", Shape::Sorting),
    ("<=", Shape::Sorting),
    (">", Shape::Sorting),
    (">=", Shape::Sorting),
    ("==", Shape::Sorting),
    ("eq", Shape::Sorting),
    ("lt", Shape::Sorting),
    ("le", Shape::Sorting),
    ("gt", Shape::Sorting),
    ("ge", Shape::Sorting),
];

/// The command names this module claims, as `crate::names` lists every builtin:
/// without the leading `::`. [`operator`] accepts either spelling, and
/// `table_and_commands_agree` holds this list to [`TABLE`].
pub const COMMANDS: &[&str] = &[
    "tcl::mathop::~",
    "tcl::mathop::!",
    "tcl::mathop::+",
    "tcl::mathop::*",
    "tcl::mathop::&",
    "tcl::mathop::|",
    "tcl::mathop::^",
    "tcl::mathop::**",
    "tcl::mathop::<<",
    "tcl::mathop::>>",
    "tcl::mathop::%",
    "tcl::mathop::!=",
    "tcl::mathop::ne",
    "tcl::mathop::in",
    "tcl::mathop::ni",
    "tcl::mathop::-",
    "tcl::mathop::/",
    "tcl::mathop::<",
    "tcl::mathop::<=",
    "tcl::mathop::>",
    "tcl::mathop::>=",
    "tcl::mathop::==",
    "tcl::mathop::eq",
    "tcl::mathop::lt",
    "tcl::mathop::le",
    "tcl::mathop::gt",
    "tcl::mathop::ge",
];

/// Whether an id belongs to this module's block.
pub(crate) fn is_op(id: u16) -> bool {
    (ext::BASE..crate::compiler::ext::MATHOP_END).contains(&id)
}

/// The index in [`TABLE`] of the operator a command name spells, with or
/// without the leading `::`. `None` for any other name.
pub fn operator(name: &str) -> Option<usize> {
    let bare = name.strip_prefix("::").unwrap_or(name).strip_prefix(PREFIX)?;
    TABLE.iter().position(|(op, _)| *op == bare)
}

// ── compiling ────────────────────────────────────────────────────────────

/// Lower a call of the operator `key` names, written as `written` — the two
/// differ for an imported name, where the script wrote `+` and the import
/// resolved it to `tcl::mathop::+`. `Tcl_WrongNumArgs` names the command as the
/// script wrote it, so the usage message carries `written`.
pub(crate) fn compile(
    c: &mut Compiler,
    written: &str,
    key: &str,
    args: &[Word],
) -> Result<(), CompileError> {
    let index = operator(key).expect("the dispatch arm asked first");
    if let Some(msg) = arity_error(index, written, args.len()) {
        return c.error(msg);
    }
    for arg in args {
        c.word(arg)?;
    }
    c.emit(Op::LoadInt(args.len() as i64), 1);
    let index = u8::try_from(index).expect("the table is shorter than a byte");
    c.emit(Op::Extended(ext::APPLY, index), -(args.len() as i32));
    Ok(())
}

/// `Tcl_WrongNumArgs` for `count` arguments to the operator at `index`, naming
/// the command as the script wrote it — or `None` when the count is one the
/// operator takes.
fn arity_error(index: usize, written: &str, count: usize) -> Option<String> {
    let expected = match TABLE[index].1 {
        Shape::Single(wanted, expected) if count != wanted => expected,
        Shape::NoIdent if count == 0 => "value ?value ...?",
        _ => return None,
    };
    Some(format!("wrong # args: should be \"{written} {expected}\""))
}

// ── running ──────────────────────────────────────────────────────────────

/// [`ext::APPLY`]: pop the count and the values, push the operator's answer.
pub(crate) fn run(vm: &mut VM, index: u8) -> Result<(), String> {
    let Value::Int(count) = vm.pop() else {
        unreachable!("the compiler pushes the operand count as an integer");
    };
    let mut values: Vec<Value> = (0..count).map(|_| vm.pop()).collect();
    values.reverse();
    let result = apply(vm, usize::from(index), values)?;
    vm.push(result);
    Ok(())
}

/// The operator `key` names, called with already-evaluated arguments under the
/// name `written` — the path a computed command name takes once
/// `namespace import` has made a bare operator a command
/// (`crate::procs`), where no compile-time check has run.
pub(crate) fn call(vm: &mut VM, written: &str, key: &str, args: &[Value]) -> Result<Value, String> {
    let index = operator(key).ok_or_else(|| format!("invalid command name \"{written}\""))?;
    if let Some(msg) = arity_error(index, written, args.len()) {
        return Err(msg);
    }
    apply(vm, index, args.to_vec())
}

/// The operator at `index`, applied to its argument values by its shape.
fn apply(vm: &mut VM, index: usize, mut values: Vec<Value>) -> Result<Value, String> {
    let (op, shape) = TABLE[index];
    match shape {
        Shape::Single(1, _) => mathop_unary(vm, op, values.remove(0)),
        Shape::Single(..) => {
            let b = values.pop().expect("two operands");
            let a = values.pop().expect("two operands");
            mathop_binary(vm, op, a, b)
        }
        Shape::Variadic(identity) => fold(vm, op, identity, values),
        Shape::NoIdent => fold(vm, op, 0, values),
        Shape::Sorting => sorted(vm, op, values),
    }
}

/// `TclVariadicOpCmd`: the identity alone for no operand, the identity combined
/// with a lone operand, and otherwise the fold — from the right for `**`, from
/// the left for everything else.
fn fold(vm: &mut VM, op: &str, identity: i64, mut values: Vec<Value>) -> Result<Value, String> {
    match values.len() {
        0 => Ok(Value::Int(identity)),
        1 => {
            let only = values.remove(0);
            match op {
                // The identity sits on the exponent side: `x ** 1`, which keeps
                // an integer operand's spelling exactly as `expr` does.
                "**" => mathop_binary(vm, op, only, Value::Int(1)),
                // `TclVariadicOpCmd` seeds `/` with the double 1.0, so `/ 2` is
                // 0.5 rather than 0.
                "/" => mathop_binary(vm, op, Value::Float(1.0), only),
                _ => mathop_binary(vm, op, Value::Int(identity), only),
            }
        }
        _ if op == "**" => {
            let mut acc = values.pop().expect("at least two operands");
            while let Some(base) = values.pop() {
                acc = mathop_binary(vm, op, base, acc)?;
            }
            Ok(acc)
        }
        _ => {
            let mut rest = values.into_iter();
            let mut acc = rest.next().expect("at least two operands");
            for value in rest {
                acc = mathop_binary(vm, op, acc, value)?;
            }
            Ok(acc)
        }
    }
}

/// `TclSortingOpCmd`: 1 when every adjacent pair satisfies the operator, 0 at
/// the first pair that does not — and nothing after that pair is read.
fn sorted(vm: &mut VM, op: &str, values: Vec<Value>) -> Result<Value, String> {
    for pair in values.windows(2) {
        let hit = mathop_binary(vm, op, pair[0].clone(), pair[1].clone())?;
        if !tcl_bool(&hit)? {
            return Ok(Value::Int(0));
        }
    }
    Ok(Value::Int(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`COMMANDS`] is read by `crate::names` as a static list; it has to be the
    /// table under the namespace prefix, in the table's order.
    #[test]
    fn table_and_commands_agree() {
        let from_table: Vec<String> = TABLE.iter().map(|(op, _)| format!("{PREFIX}{op}")).collect();
        assert_eq!(from_table, COMMANDS);
        assert!(COMMANDS.len() <= usize::from(u8::MAX));
    }

    #[test]
    fn both_spellings_name_the_same_operator() {
        assert_eq!(operator("tcl::mathop::+"), operator("::tcl::mathop::+"));
        assert_eq!(operator("::tcl::mathop::**"), Some(7));
        assert_eq!(operator("tcl::mathop::"), None);
        assert_eq!(operator("tcl::mathop::plus"), None);
        assert_eq!(operator("+"), None);
        assert_eq!(operator("::::tcl::mathop::+"), None);
    }
}
