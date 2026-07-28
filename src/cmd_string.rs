//! Tcl's string handling: the `string` ensemble, `append`, and `format`.
//!
//! Each command lowers to one frontend extension op. The compiler resolves
//! everything that is fixed at compile time — the ensemble subcommand, its
//! options, the `string is` class — and pushes only the values that have to be
//! computed, so the runtime side never re-parses an argument list. Optional
//! arguments are not padded with sentinels; the operand count carried in the
//! op's payload byte tells the handler which variant it is, which keeps a real
//! argument that happens to look like a sentinel from being mistaken for one.
//!
//! Tcl counts and indexes by code point, so the runtime works over `Vec<char>`
//! rather than bytes. Behavior is ported from tclsh 9.0.4 — `Tcl_StringCaseMatch`
//! and `TclFindElement` in `generic/tclUtil.c`, `StringMapCmd` and friends in
//! `generic/tclCmdMZ.c`, `Tcl_UtfToTitle` in `generic/tclUtf.c` — and every
//! subcommand is pinned against that interpreter by the differential suite.
//!
//! Two areas are refused rather than approximated. Tcl's Unicode character
//! classes come from its own general-category tables, which Rust's standard
//! library does not expose and which are a different Unicode revision besides;
//! the classes that need them accept ASCII and report an error otherwise. Case
//! conversion is exact: Rust's simple case mappings agree with Tcl's over every
//! code point up to U+2FFFF once Tcl's three quirks are reproduced — the
//! in-place length guard, the Greek ypogegrammeni capitals, and Georgian
//! Mkhedruli having no titlecase.

use std::sync::Arc;

use fusevm::{Op, Value, VM};

use crate::compiler::{CompileError, Compiler};
use crate::parser::Word;
use crate::runtime::to_tcl_string;

/// Extension opcode ids owned by this module. The base is declared with every
/// other module's in [`crate::compiler::ext`], which is where the frontend's
/// one id space is laid out; [`crate::runtime`] dispatches by range from the
/// highest base down.
pub mod ext {
    pub use crate::compiler::ext::STRING_BASE as BASE;
    pub const CAT: u16 = BASE;
    pub const COMPARE: u16 = BASE + 1;
    pub const EQUAL: u16 = BASE + 2;
    pub const FIRST: u16 = BASE + 3;
    pub const INDEX: u16 = BASE + 4;
    pub const INSERT: u16 = BASE + 5;
    pub const IS: u16 = BASE + 6;
    pub const LAST: u16 = BASE + 7;
    pub const LENGTH: u16 = BASE + 8;
    pub const MAP: u16 = BASE + 9;
    pub const MATCH: u16 = BASE + 10;
    pub const RANGE: u16 = BASE + 11;
    pub const REPEAT: u16 = BASE + 12;
    pub const REPLACE: u16 = BASE + 13;
    pub const REVERSE: u16 = BASE + 14;
    pub const TOLOWER: u16 = BASE + 15;
    pub const TOTITLE: u16 = BASE + 16;
    pub const TOUPPER: u16 = BASE + 17;
    pub const TRIM: u16 = BASE + 18;
    pub const TRIMLEFT: u16 = BASE + 19;
    pub const TRIMRIGHT: u16 = BASE + 20;
    pub const APPEND: u16 = BASE + 21;
    pub const FORMAT: u16 = BASE + 22;
}

/// Every subcommand the ensemble knows, in the order the interpreter lists them
/// when it rejects one. `wordend` and `wordstart` are listed because their
/// presence decides whether an abbreviation is ambiguous, but they are refused.
pub(crate) const SUBCOMMANDS: &[&str] = &[
    "cat",
    "compare",
    "equal",
    "first",
    "index",
    "insert",
    "is",
    "last",
    "length",
    "map",
    "match",
    "range",
    "repeat",
    "replace",
    "reverse",
    "tolower",
    "totitle",
    "toupper",
    "trim",
    "trimleft",
    "trimright",
    "wordend",
    "wordstart",
];

/// The `string is` classes, in the interpreter's listing order.
const CLASSES: &[&str] = &[
    "alnum",
    "alpha",
    "ascii",
    "control",
    "boolean",
    "dict",
    "digit",
    "double",
    "entier",
    "false",
    "graph",
    "integer",
    "list",
    "lower",
    "print",
    "punct",
    "space",
    "true",
    "upper",
    "wideinteger",
    "wordchar",
    "xdigit",
];

/// Resolve a name against a table the way `Tcl_GetIndexFromObj` does: an exact
/// match wins, otherwise a prefix that fits exactly one entry.
fn resolve<'t>(name: &str, table: &[&'t str]) -> Option<&'t str> {
    if let Some(exact) = table.iter().find(|c| **c == name) {
        return Some(exact);
    }
    let mut hit = None;
    for candidate in table {
        if candidate.starts_with(name) {
            if hit.is_some() {
                return None;
            }
            hit = Some(*candidate);
        }
    }
    hit
}

/// The interpreter's rendering of a table in an error message.
fn listing(table: &[&str]) -> String {
    let mut out = String::new();
    for (i, name) in table.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        if i + 1 == table.len() {
            out.push_str("or ");
        }
        out.push_str(name);
    }
    out
}

// ── compiling ────────────────────────────────────────────────────────────

impl Compiler {
    /// The one entry point the command dispatcher needs.
    pub(crate) fn cmd_string_family(
        &mut self,
        name: &str,
        args: &[Word],
    ) -> Result<(), CompileError> {
        match name {
            "string" => self.cmd_string(args),
            "append" => self.cmd_append(args),
            _ => self.cmd_format(args),
        }
    }

    /// Emit the extension op for a command whose operands are already pushed.
    fn string_op(&mut self, id: u16, argc: usize) -> Result<(), CompileError> {
        let Ok(argc8) = u8::try_from(argc) else {
            return self.error("too many arguments for one command");
        };
        self.emit(Op::Extended(id, argc8), 1 - argc as i32);
        Ok(())
    }

    fn push_flag(&mut self, on: bool) {
        self.push_value(Value::Int(on as i64));
    }

    /// Push every word in order and emit the op.
    fn words_op(&mut self, id: u16, words: &[Word]) -> Result<(), CompileError> {
        for w in words {
            self.word(w)?;
        }
        self.string_op(id, words.len())
    }

    fn cmd_string(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let Some(first) = args.first() else {
            return self.error("wrong # args: should be \"string subcommand ?arg ...?\"");
        };
        let given = self.literal_of(first, "subcommand")?.to_string();
        let Some(sub) = resolve(&given, SUBCOMMANDS) else {
            return self.error(format!(
                "unknown or ambiguous subcommand \"{given}\": must be {}",
                listing(SUBCOMMANDS)
            ));
        };
        let rest = &args[1..];

        match sub {
            "cat" => self.words_op(ext::CAT, rest),
            "compare" | "equal" => self.cmd_string_compare(sub, rest),
            "first" | "last" => {
                let id = if sub == "first" {
                    ext::FIRST
                } else {
                    ext::LAST
                };
                let what = if sub == "first" {
                    "needleString haystackString ?startIndex?"
                } else {
                    "needleString haystackString ?lastIndex?"
                };
                self.fixed(id, rest, 2, 3, sub, what)
            }
            "index" => self.fixed(ext::INDEX, rest, 2, 2, sub, "string charIndex"),
            "insert" => self.fixed(ext::INSERT, rest, 3, 3, sub, "string index insertString"),
            "is" => self.cmd_string_is(rest),
            "length" => self.fixed(ext::LENGTH, rest, 1, 1, sub, "string"),
            "reverse" => self.fixed(ext::REVERSE, rest, 1, 1, sub, "string"),
            "map" | "match" => self.cmd_string_nocase(sub, rest),
            "range" => self.fixed(ext::RANGE, rest, 3, 3, sub, "string first last"),
            "repeat" => self.fixed(ext::REPEAT, rest, 2, 2, sub, "string count"),
            "replace" => self.fixed(ext::REPLACE, rest, 3, 4, sub, "string first last ?string?"),
            "tolower" | "totitle" | "toupper" => {
                let id = match sub {
                    "tolower" => ext::TOLOWER,
                    "totitle" => ext::TOTITLE,
                    _ => ext::TOUPPER,
                };
                self.fixed(id, rest, 1, 3, sub, "string ?first? ?last?")
            }
            "trim" | "trimleft" | "trimright" => {
                let id = match sub {
                    "trim" => ext::TRIM,
                    "trimleft" => ext::TRIMLEFT,
                    _ => ext::TRIMRIGHT,
                };
                self.fixed(id, rest, 1, 2, sub, "string ?chars?")
            }
            other => self.error(format!("\"string {other}\" is not supported yet")),
        }
    }

    /// A subcommand whose arguments are all values, with an arity range.
    fn fixed(
        &mut self,
        id: u16,
        args: &[Word],
        min: usize,
        max: usize,
        sub: &str,
        usage: &str,
    ) -> Result<(), CompileError> {
        if args.len() < min || args.len() > max {
            return self.error(format!("wrong # args: should be \"string {sub} {usage}\""));
        }
        self.words_op(id, args)
    }

    /// `string compare` and `string equal`, whose options sit before the last
    /// two arguments. The `-nocase` flag is resolved here; `-length` is a value
    /// and is pushed, so the operand count distinguishes the two forms.
    fn cmd_string_compare(&mut self, sub: &str, args: &[Word]) -> Result<(), CompileError> {
        let usage = format!(
            "wrong # args: should be \"string {sub} ?-nocase? ?-length int? string1 string2\""
        );
        if args.len() < 2 || args.len() > 5 {
            return self.error(usage);
        }
        let (opts, operands) = args.split_at(args.len() - 2);
        let mut nocase = false;
        let mut length: Option<&Word> = None;
        let mut i = 0;
        while i < opts.len() {
            let opt = self.literal_of(&opts[i], "option")?;
            if opt.len() > 1 && "-nocase".starts_with(opt) {
                nocase = true;
                i += 1;
            } else if opt.len() > 1 && "-length".starts_with(opt) {
                if i + 1 >= opts.len() {
                    return self.error(usage);
                }
                length = Some(&opts[i + 1]);
                i += 2;
            } else {
                return self.error(format!("bad option \"{opt}\": must be -nocase or -length"));
            }
        }

        let id = if sub == "compare" {
            ext::COMPARE
        } else {
            ext::EQUAL
        };
        self.push_flag(nocase);
        let mut argc = 3;
        if let Some(w) = length {
            self.word(w)?;
            argc = 4;
        }
        self.word(&operands[0])?;
        self.word(&operands[1])?;
        self.string_op(id, argc)
    }

    /// `string map` and `string match`, which take only `-nocase`.
    fn cmd_string_nocase(&mut self, sub: &str, args: &[Word]) -> Result<(), CompileError> {
        let what = if sub == "map" {
            "charMap string"
        } else {
            "pattern string"
        };
        if args.len() < 2 || args.len() > 3 {
            return self.error(format!(
                "wrong # args: should be \"string {sub} ?-nocase? {what}\""
            ));
        }
        let mut nocase = false;
        if args.len() == 3 {
            let opt = self.literal_of(&args[0], "option")?;
            if opt.len() > 1 && "-nocase".starts_with(opt) {
                nocase = true;
            } else {
                return self.error(format!("bad option \"{opt}\": must be -nocase"));
            }
        }
        let id = if sub == "map" { ext::MAP } else { ext::MATCH };
        self.push_flag(nocase);
        self.word(&args[args.len() - 2])?;
        self.word(&args[args.len() - 1])?;
        self.string_op(id, 3)
    }

    /// `string is class ?-strict? str`. The class and the flag are compile-time
    /// constants; `-failindex` needs to write a variable from inside the op and
    /// is refused instead of being half-implemented.
    fn cmd_string_is(&mut self, args: &[Word]) -> Result<(), CompileError> {
        const USAGE: &str =
            "wrong # args: should be \"string is class ?-strict? ?-failindex var? str\"";
        if args.len() < 2 || args.len() > 5 {
            return self.error(USAGE);
        }
        let given = self.literal_of(&args[0], "class")?.to_string();
        let Some(class) = resolve(&given, CLASSES) else {
            let ambiguous = CLASSES.iter().filter(|c| c.starts_with(&given)).count() > 1;
            let what = if ambiguous { "ambiguous" } else { "bad" };
            return self.error(format!(
                "{what} class \"{given}\": must be {}",
                listing(CLASSES)
            ));
        };

        let mut strict = false;
        for w in &args[1..args.len() - 1] {
            let opt = self.literal_of(w, "option")?;
            if opt.len() > 1 && "-strict".starts_with(opt) {
                strict = true;
            } else if opt.len() > 1 && "-failindex".starts_with(opt) {
                return self.error("the -failindex option of \"string is\" is not supported yet");
            } else {
                return self.error(format!(
                    "bad option \"{opt}\": must be -strict or -failindex"
                ));
            }
        }
        if matches!(class, "dict" | "graph" | "print" | "punct") {
            return self.error(format!(
                "the \"{class}\" character class needs Unicode category tables, which are not built yet"
            ));
        }

        self.push_str(class);
        self.push_flag(strict);
        self.word(&args[args.len() - 1])?;
        self.string_op(ext::IS, 3)
    }

    /// `append varName ?value ...?` — read, concatenate, store, and yield the
    /// new value. The name travels as an operand so the op can report reading
    /// an unset variable the way the interpreter does.
    fn cmd_append(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let Some(target) = args.first() else {
            return self.error("wrong # args: should be \"append varName ?value ...?\"");
        };
        let name = self.var_name_of(target)?;

        self.push_str(&name);
        self.scalar_get(&name);
        for w in &args[1..] {
            self.word(w)?;
        }
        self.string_op(ext::APPEND, args.len() + 1)?;
        self.emit(Op::Dup, 1);
        self.emit_set_var(&name);
        Ok(())
    }

    fn cmd_format(&mut self, args: &[Word]) -> Result<(), CompileError> {
        if args.is_empty() {
            return self.error("wrong # args: should be \"format formatString ?arg ...?\"");
        }
        self.words_op(ext::FORMAT, args)
    }
}

// ── running ──────────────────────────────────────────────────────────────

/// Run one of this module's ops: pop `argc` operands, push one result.
pub(crate) fn extension(vm: &mut VM, id: u16, argc: u8) -> Result<(), String> {
    let mut operands = Vec::with_capacity(argc as usize);
    for _ in 0..argc {
        operands.push(vm.pop());
    }
    operands.reverse();
    let result = dispatch(id, &operands)?;
    vm.push(Value::Str(Arc::new(result)));
    Ok(())
}

fn dispatch(id: u16, operands: &[Value]) -> Result<String, String> {
    // `append` is the one op that needs an operand's identity rather than its
    // string form: an unset variable reads back as `Undef`, and appending to
    // one is only an error when there is nothing to append.
    if id == ext::APPEND {
        let name = to_tcl_string(&operands[0]);
        if operands.len() == 2 && operands[1] == Value::Undef {
            return Err(format!("can't read \"{name}\": no such variable"));
        }
        let mut out = to_tcl_string(&operands[1]);
        for v in &operands[2..] {
            out.push_str(&to_tcl_string(v));
        }
        return Ok(out);
    }

    let a: Vec<String> = operands.iter().map(to_tcl_string).collect();
    match id {
        ext::CAT => Ok(a.concat()),
        ext::COMPARE | ext::EQUAL => {
            let nocase = truth(&a[0]);
            let (req, s1, s2) = if a.len() == 4 {
                (Some(want_int(&a[1])?), &a[2], &a[3])
            } else {
                (None, &a[1], &a[2])
            };
            let ordering = compare(&chars(s1), &chars(s2), nocase, req);
            Ok(if id == ext::COMPARE {
                ordering.to_string()
            } else {
                ((ordering == 0) as i32).to_string()
            })
        }
        ext::FIRST => {
            let hay = chars(&a[1]);
            let start = match a.get(2) {
                Some(spec) => index_of(spec, hay.len() as i64 - 1)?,
                None => 0,
            };
            Ok(find_first(&chars(&a[0]), &hay, start).to_string())
        }
        ext::LAST => {
            let hay = chars(&a[1]);
            let last = match a.get(2) {
                Some(spec) => index_of(spec, hay.len() as i64 - 1)?,
                None => i64::MAX,
            };
            Ok(find_last(&chars(&a[0]), &hay, last).to_string())
        }
        ext::INDEX => {
            let s = chars(&a[0]);
            let i = index_of(&a[1], s.len() as i64 - 1)?;
            Ok(match usize::try_from(i).ok().and_then(|i| s.get(i)) {
                Some(c) => c.to_string(),
                None => String::new(),
            })
        }
        ext::INSERT => {
            let s = chars(&a[0]);
            let mut at = index_of(&a[1], s.len() as i64)?;
            at = at.clamp(0, s.len() as i64);
            let at = at as usize;
            let mut out: String = s[..at].iter().collect();
            out.push_str(&a[2]);
            out.extend(&s[at..]);
            Ok(out)
        }
        ext::IS => Ok((is_class(&a[0], truth(&a[1]), &a[2])? as i32).to_string()),
        ext::LENGTH => Ok(a[0].chars().count().to_string()),
        ext::MAP => map(truth(&a[0]), &a[1], &a[2]),
        ext::MATCH => Ok((matches(&chars(&a[1]), &chars(&a[2]), truth(&a[0])) as i32).to_string()),
        ext::RANGE => {
            let s = chars(&a[0]);
            let end = s.len() as i64 - 1;
            let first = index_of(&a[1], end)?.max(0);
            let last = index_of(&a[2], end)?.min(end);
            Ok(if first > last {
                String::new()
            } else {
                s[first as usize..=last as usize].iter().collect()
            })
        }
        ext::REPEAT => {
            let count = want_int(&a[1])?;
            if count <= 0 || a[0].is_empty() {
                return Ok(String::new());
            }
            // The interpreter tries the allocation and dies with the process
            // when it cannot have it; refusing is the more useful answer.
            let bytes = (count as i128) * (a[0].len() as i128);
            if bytes > i32::MAX as i128 {
                return Err("string repeat: result would exceed 2 GiB".to_string());
            }
            Ok(a[0].repeat(count as usize))
        }
        ext::REPLACE => {
            let s = chars(&a[0]);
            let end = s.len() as i64 - 1;
            let first = index_of(&a[1], end)?;
            let last = index_of(&a[2], end)?;
            if first > last || first > end || last < 0 {
                return Ok(a[0].clone());
            }
            let first = first.max(0) as usize;
            let last = last.min(end) as usize;
            let mut out: String = s[..first].iter().collect();
            if let Some(new) = a.get(3) {
                out.push_str(new);
            }
            out.extend(&s[last + 1..]);
            Ok(out)
        }
        ext::REVERSE => Ok(chars(&a[0]).iter().rev().collect()),
        ext::TOLOWER | ext::TOTITLE | ext::TOUPPER => convert_case(id, &a),
        ext::TRIM | ext::TRIMLEFT | ext::TRIMRIGHT => {
            let s = chars(&a[0]);
            let set = a.get(1).map(|c| chars(c));
            let keep = |c: &char| match &set {
                Some(set) => !set.contains(c),
                None => !(*c == '\0' || is_space(*c)),
            };
            let start = if id == ext::TRIMRIGHT {
                0
            } else {
                s.iter().position(keep).unwrap_or(s.len())
            };
            let stop = if id == ext::TRIMLEFT {
                s.len()
            } else {
                s.iter().rposition(keep).map_or(start, |i| i + 1).max(start)
            };
            Ok(s[start..stop].iter().collect())
        }
        ext::FORMAT => format_string(&a[0], &a[1..]),
        other => Err(format!("unknown extension op {other}")),
    }
}

fn chars(s: &str) -> Vec<char> {
    s.chars().collect()
}

/// A compiler-pushed flag.
fn truth(s: &str) -> bool {
    s == "1"
}

// ── indices ──────────────────────────────────────────────────────────────

/// A Tcl index: `integer?[+-]integer?` or `end?[+-]integer?`, where `end`
/// stands for `end_value`. Ported from `GetEndOffsetFromObj` in
/// `generic/tclUtil.c`; every index that works out negative collapses to -1,
/// which is what `Tcl_GetIntForIndex` hands its callers.
fn index_of(spec: &str, end_value: i64) -> Result<i64, String> {
    let bad = || {
        Err(format!(
            "bad index \"{spec}\": must be integer?[+-]integer? or end?[+-]integer?"
        ))
    };

    if let Some(v) = parse_int(spec.trim_matches(is_ascii_space)) {
        return Ok(if v < 0 { -1 } else { v });
    }

    if let Some(rest) = spec.strip_prefix("end") {
        if rest.is_empty() {
            return Ok(end_value);
        }
        // Split on the byte, not with `split_at(1)`: the character after `end`
        // may be multi-byte — `string index abc endé` — and slicing into one
        // aborts the process where tclsh reports `bad index`.
        let op = rest.as_bytes()[0];
        if (op != b'-' && op != b'+') || rest[1..].starts_with(is_ascii_space) {
            return bad();
        }
        let digits = &rest[1..];
        let Some(mut offset) = parse_int(digits.trim_end_matches(is_ascii_space)) else {
            return bad();
        };
        if op == b'-' {
            offset = offset.saturating_neg();
        }
        // The interpreter distinguishes "end+1" from "end+n" so that commands
        // like lset can tell an append from an out-of-range write; every caller
        // here only needs a number past the end.
        return Ok(match offset {
            1 => end_value.saturating_add(1),
            n if n > 1 => i64::MAX - 1,
            n => end_value.saturating_add(n),
        });
    }

    // `M+N`: no whitespace may touch the operator, but the whole may be padded.
    let body = spec.trim_start_matches(is_ascii_space);
    let split = body
        .char_indices()
        .skip(1)
        .find(|(_, c)| *c == '+' || *c == '-')
        .map(|(i, _)| i);
    let Some(at) = split else { return bad() };
    let (left, rest) = body.split_at(at);
    let (op, right) = rest.split_at(1);
    if right.starts_with(is_ascii_space) {
        return bad();
    }
    let (Some(m), Some(n)) = (
        parse_int(left),
        parse_int(right.trim_end_matches(is_ascii_space)),
    ) else {
        return bad();
    };
    let sum = if op == "-" {
        m.saturating_sub(n)
    } else {
        m.saturating_add(n)
    };
    Ok(if sum < 0 { -1 } else { sum })
}

fn is_ascii_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r')
}

/// Tcl's integer syntax: an optional sign, an optional radix prefix, and digits
/// that may be separated by underscores. Values that do not fit a `i64`
/// saturate, which is all any index needs.
fn parse_int(text: &str) -> Option<i64> {
    let lit = scan_int(text)?;
    let mut value: i64 = 0;
    for d in lit.digits.chars().filter_map(|c| c.to_digit(lit.radix)) {
        value = value
            .saturating_mul(lit.radix as i64)
            .saturating_add(d as i64);
    }
    Some(if lit.negative { -value } else { value })
}

struct IntLit {
    negative: bool,
    radix: u32,
    digits: String,
}

/// Scan a complete Tcl integer literal, or `None` when the text is not one.
fn scan_int(text: &str) -> Option<IntLit> {
    let (negative, body) = match text.strip_prefix(['-', '+']) {
        Some(rest) => (text.starts_with('-'), rest),
        None => (false, text),
    };
    let (radix, digits) = match body.get(..2) {
        Some("0x") | Some("0X") => (16, &body[2..]),
        Some("0o") | Some("0O") => (8, &body[2..]),
        Some("0b") | Some("0B") => (2, &body[2..]),
        Some("0d") | Some("0D") => (10, &body[2..]),
        _ => (10, body),
    };
    if !valid_digits(digits, radix) {
        return None;
    }
    Some(IntLit {
        negative,
        radix,
        digits: digits.to_string(),
    })
}

/// Digits in `radix`, with underscore runs allowed only between digits.
fn valid_digits(text: &str, radix: u32) -> bool {
    if text.is_empty() {
        return false;
    }
    let mut previous_digit = false;
    let mut pending_underscore = false;
    for c in text.chars() {
        if c == '_' {
            if !previous_digit {
                return false;
            }
            pending_underscore = true;
        } else if c.is_digit(radix) {
            previous_digit = true;
            pending_underscore = false;
        } else {
            return false;
        }
    }
    !pending_underscore
}

/// An operand a command needs as an integer.
fn want_int(text: &str) -> Result<i64, String> {
    parse_int(text.trim_matches(is_ascii_space))
        .ok_or_else(|| format!("expected integer but got \"{text}\""))
}

// ── comparison and search ────────────────────────────────────────────────

/// `TclStringCmp`: compare by code point, optionally case-folded, optionally
/// only over the first `req` characters.
fn compare(a: &[char], b: &[char], nocase: bool, req: Option<i64>) -> i32 {
    if req == Some(0) {
        return 0;
    }
    let mut len = a.len().min(b.len());
    let req = req.filter(|r| *r > 0).map(|r| r as usize);
    if let Some(r) = req {
        len = len.min(r);
    }
    for i in 0..len {
        let (x, y) = if nocase {
            (lower(a[i]), lower(b[i]))
        } else {
            (a[i], b[i])
        };
        if x != y {
            return if x < y { -1 } else { 1 };
        }
    }
    match req {
        Some(r) if r <= len => 0,
        _ => (a.len() as i64 - b.len() as i64).signum() as i32,
    }
}

fn find_first(needle: &[char], hay: &[char], start: i64) -> i64 {
    if needle.is_empty() || needle.len() > hay.len() {
        return -1;
    }
    let from = start.max(0) as usize;
    for i in from..=hay.len().saturating_sub(needle.len()) {
        if hay[i..].starts_with(needle) {
            return i as i64;
        }
    }
    -1
}

fn find_last(needle: &[char], hay: &[char], last: i64) -> i64 {
    if needle.is_empty() || needle.len() > hay.len() || last < 0 {
        return -1;
    }
    let highest = hay.len() - needle.len();
    let limit = if last < highest as i64 {
        last as usize
    } else {
        highest
    };
    for i in (0..=limit).rev() {
        if hay[i..].starts_with(needle) {
            return i as i64;
        }
    }
    -1
}

// ── glob matching ────────────────────────────────────────────────────────

/// `Tcl_StringCaseMatch`, ported. `[` opens a set that ends at the first `]`;
/// `^` has no meaning inside one, an unterminated set matches only when both
/// pattern and string run out together, and a reversed range still matches.
fn matches(pattern: &[char], text: &[char], nocase: bool) -> bool {
    let (mut p, mut s) = (0usize, 0usize);
    loop {
        if p >= pattern.len() {
            return s >= text.len();
        }
        if s >= text.len() && pattern[p] != '*' {
            return false;
        }

        if pattern[p] == '*' {
            while p < pattern.len() && pattern[p] == '*' {
                p += 1;
            }
            if p >= pattern.len() {
                return true;
            }
            let head = fold(pattern[p], nocase);
            loop {
                if !matches!(pattern[p], '[' | '?' | '\\') {
                    while s < text.len() && fold(text[s], nocase) != head {
                        s += 1;
                    }
                }
                if matches(&pattern[p..], &text[s..], nocase) {
                    return true;
                }
                if s >= text.len() {
                    return false;
                }
                s += 1;
            }
        }

        if pattern[p] == '?' {
            p += 1;
            s += 1;
            continue;
        }

        if pattern[p] == '[' {
            p += 1;
            let ch = fold(text[s], nocase);
            s += 1;
            loop {
                if p >= pattern.len() || pattern[p] == ']' {
                    return false;
                }
                let start = fold(pattern[p], nocase);
                p += 1;
                if pattern.get(p) == Some(&'-') {
                    p += 1;
                    let Some(&raw) = pattern.get(p) else {
                        return false;
                    };
                    let stop = fold(raw, nocase);
                    p += 1;
                    if (start <= ch && ch <= stop) || (stop <= ch && ch <= start) {
                        break;
                    }
                } else if start == ch {
                    break;
                }
            }
            while pattern.get(p) != Some(&']') {
                if p >= pattern.len() {
                    // The set never closed; a match still stands if both ran out.
                    return s >= text.len();
                }
                p += 1;
            }
            p += 1;
            continue;
        }

        if pattern[p] == '\\' {
            p += 1;
            if p >= pattern.len() {
                return false;
            }
        }

        if fold(text[s], nocase) != fold(pattern[p], nocase) {
            return false;
        }
        s += 1;
        p += 1;
    }
}

fn fold(c: char, nocase: bool) -> char {
    if nocase {
        lower(c)
    } else {
        c
    }
}

// ── string map ───────────────────────────────────────────────────────────

/// `StringMapCmd`: one left-to-right pass, keys tried in list order, the first
/// that matches wins and the scan resumes after it.
fn map(nocase: bool, mapping: &str, text: &str) -> Result<String, String> {
    let pairs = split_list(mapping)?;
    if pairs.is_empty() {
        return Ok(text.to_string());
    }
    if pairs.len() % 2 != 0 {
        return Err("char map list unbalanced".to_string());
    }
    let keys: Vec<Vec<char>> = pairs.iter().step_by(2).map(|k| chars(k)).collect();
    let s = chars(text);
    let mut out = String::new();
    let mut i = 0;
    'outer: while i < s.len() {
        for (k, key) in keys.iter().enumerate() {
            if key.is_empty() || key.len() > s.len() - i {
                continue;
            }
            let hit = key
                .iter()
                .zip(&s[i..])
                .all(|(a, b)| fold(*a, nocase) == fold(*b, nocase));
            if hit {
                out.push_str(&pairs[2 * k + 1]);
                i += key.len();
                continue 'outer;
            }
        }
        out.push(s[i]);
        i += 1;
    }
    Ok(out)
}

/// `TclFindElement`, ported: split a Tcl list into its elements.
///
/// Byte offsets are safe here because every character the parse reacts to is
/// ASCII, and a UTF-8 continuation byte can never be one of them.
fn split_list(text: &str) -> Result<Vec<String>, String> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    loop {
        while i < b.len() && is_list_space(b[i]) {
            i += 1;
        }
        if i >= b.len() {
            return Ok(out);
        }
        let (mut braces, mut quoted) = (0usize, false);
        match b[i] {
            b'{' => {
                braces = 1;
                i += 1;
            }
            b'"' => {
                quoted = true;
                i += 1;
            }
            _ => {}
        }
        let start = i;
        let mut literal = true;
        let stop;
        loop {
            if i >= b.len() {
                if braces != 0 {
                    return Err("unmatched open brace in list".to_string());
                }
                if quoted {
                    return Err("unmatched open quote in list".to_string());
                }
                stop = i;
                break;
            }
            match b[i] {
                b'{' if braces != 0 => {
                    braces += 1;
                    i += 1;
                }
                b'}' if braces > 1 => {
                    braces -= 1;
                    i += 1;
                }
                b'}' if braces == 1 => {
                    stop = i;
                    i += 1;
                    if i < b.len() && !is_list_space(b[i]) {
                        return Err("list element in braces followed by junk".to_string());
                    }
                    break;
                }
                b'\\' => {
                    if braces == 0 {
                        // The element's value differs from the text scanned, so
                        // the caller has to collapse it.
                        literal = false;
                    }
                    i = crate::parser::backslash_at(text, i).1;
                }
                b'"' if quoted => {
                    stop = i;
                    i += 1;
                    if i < b.len() && !is_list_space(b[i]) {
                        return Err("list element in quotes followed by junk".to_string());
                    }
                    break;
                }
                c if is_list_space(c) && braces == 0 && !quoted => {
                    stop = i;
                    break;
                }
                _ => i += 1,
            }
        }
        out.push(if literal {
            text[start..stop].to_string()
        } else {
            collapse(&text[start..stop])
        });
    }
}

/// `TclCopyAndCollapse`: resolve the backslash sequences of a list element,
/// which the splitter reports verbatim.
fn collapse(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::new();
    let mut i = 0usize;
    while i < b.len() {
        if b[i] == b'\\' {
            let (value, next) = crate::parser::backslash_at(text, i);
            out.push_str(&value);
            i = next;
        } else {
            let start = i;
            i += 1;
            while i < b.len() && b[i] & 0xC0 == 0x80 {
                i += 1;
            }
            out.push_str(&text[start..i]);
        }
    }
    out
}

fn is_list_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

// ── case conversion ──────────────────────────────────────────────────────

/// Tcl's simple uppercase. Rust's `to_uppercase` is the full mapping, which
/// expands some characters into several; Tcl uses the one-to-one mapping from
/// `UnicodeData`, and where none exists the character is unchanged. The Greek
/// ypogegrammeni letters are the set where the two disagree — their full
/// mapping is two characters but their simple mapping is the capital eight code
/// points along.
fn upper(c: char) -> char {
    let v = c as u32;
    let simple = match v {
        0x1F80..=0x1F87 | 0x1F90..=0x1F97 | 0x1FA0..=0x1FA7 => char::from_u32(v + 8).unwrap_or(c),
        0x1FB3 => '\u{1FBC}',
        0x1FC3 => '\u{1FCC}',
        0x1FF3 => '\u{1FFC}',
        _ => single(c.to_uppercase(), c),
    };
    guard(c, simple)
}

/// Tcl's simple lowercase. U+0130 is the only character whose full lowercase
/// runs to two code points, and its simple mapping is a plain `i`.
fn lower(c: char) -> char {
    let simple = if c == '\u{130}' {
        'i'
    } else {
        single(c.to_lowercase(), c)
    };
    guard(c, simple)
}

/// Tcl's titlecase. Georgian Mkhedruli has no titlecase in Tcl's tables even
/// though it has an uppercase, so it is left alone.
fn title(c: char) -> char {
    if matches!(c as u32, 0x10D0..=0x10FA | 0x10FD..=0x10FF) {
        return c;
    }
    let simple = match c {
        '\u{1C4}' | '\u{1C5}' | '\u{1C6}' => '\u{1C5}',
        '\u{1C7}' | '\u{1C8}' | '\u{1C9}' => '\u{1C8}',
        '\u{1CA}' | '\u{1CB}' | '\u{1CC}' => '\u{1CB}',
        '\u{1F1}' | '\u{1F2}' | '\u{1F3}' => '\u{1F2}',
        _ => return upper(c),
    };
    guard(c, simple)
}

fn single(mut mapping: impl Iterator<Item = char>, fallback: char) -> char {
    let first = mapping.next().unwrap_or(fallback);
    if mapping.next().is_some() {
        fallback
    } else {
        first
    }
}

/// Tcl converts case in place and keeps the original character whenever the
/// converted one would need more bytes, so `ɐ` (two bytes) never becomes `Ɐ`
/// (three).
fn guard(original: char, mapped: char) -> char {
    if mapped.len_utf8() > original.len_utf8() {
        original
    } else {
        mapped
    }
}

/// `string tolower`, `totitle` and `toupper`, with their optional index range.
fn convert_case(id: u16, a: &[String]) -> Result<String, String> {
    let s = chars(&a[0]);
    let end = s.len() as i64 - 1;
    let (first, last) = if a.len() == 1 {
        (0, end)
    } else {
        let first = index_of(&a[1], end)?.max(0);
        let last = match a.get(2) {
            Some(spec) => index_of(spec, end)?,
            None => first,
        };
        // The interpreter clamps the end of the range before deciding whether
        // the range is empty, so `string tolower ABCDEF 10` is a no-op.
        (first, last.min(end))
    };
    if last < first {
        return Ok(a[0].clone());
    }

    let (first, last) = (first as usize, last as usize);
    let mut out: String = s[..first].iter().collect();
    for (offset, &c) in s[first..=last].iter().enumerate() {
        out.push(match id {
            ext::TOUPPER => upper(c),
            ext::TOLOWER => lower(c),
            _ if offset == 0 => title(c),
            // The tail of a titlecased range is lowercased, except for Georgian
            // Asomtavruli, which `Tcl_UtfToTitle` skips.
            _ if matches!(c as u32, 0x1C90..=0x1CBF) => c,
            _ => lower(c),
        });
    }
    out.extend(&s[last + 1..]);
    Ok(out)
}

// ── character classes ────────────────────────────────────────────────────

/// Tcl's whitespace: the C0 set below U+0080, then the Unicode separators plus
/// four characters Tcl names explicitly. Verified equal to the interpreter over
/// every code point up to U+2FFFF.
fn is_space(c: char) -> bool {
    if (c as u32) < 0x80 {
        matches!(c, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r')
    } else {
        c.is_whitespace() || matches!(c, '\u{180E}' | '\u{200B}' | '\u{2060}' | '\u{FEFF}')
    }
}

/// `string is`. The classes that rest on Unicode general categories accept
/// ASCII, where the two implementations were verified to agree exactly, and
/// report an error on anything else rather than answering from Rust's tables,
/// which are a different Unicode revision than Tcl's.
fn is_class(class: &str, strict: bool, text: &str) -> Result<bool, String> {
    if class == "list" {
        // Strictness is ignored here: an empty string is a well-formed list.
        return Ok(split_list(text).is_ok());
    }
    if text.is_empty() {
        return Ok(!strict);
    }

    match class {
        "boolean" | "true" | "false" => {
            let Some(value) = parse_bool(text) else {
                return Ok(false);
            };
            return Ok(match class {
                "true" => value,
                "false" => !value,
                _ => true,
            });
        }
        "integer" | "entier" => return Ok(scan_int(text.trim_matches(is_ascii_space)).is_some()),
        "wideinteger" => {
            let body = text.trim_matches(is_ascii_space);
            return Ok(scan_int(body).is_some() && fits_wide(body));
        }
        "double" => return Ok(is_double(text.trim_matches(is_ascii_space))),
        _ => {}
    }

    let ok = |c: char| -> Result<bool, String> {
        if class == "ascii" {
            return Ok((c as u32) < 0x80);
        }
        if class == "xdigit" {
            return Ok(c.is_ascii_hexdigit());
        }
        if class == "space" {
            return Ok(is_space(c));
        }
        if (c as u32) >= 0x80 {
            return Err(format!(
                "string is {class}: characters beyond ASCII need Unicode category tables, which are not built yet"
            ));
        }
        Ok(match class {
            "alnum" => c.is_ascii_alphanumeric(),
            "alpha" => c.is_ascii_alphabetic(),
            "control" => c.is_ascii_control(),
            "digit" => c.is_ascii_digit(),
            "lower" => c.is_ascii_lowercase(),
            "upper" => c.is_ascii_uppercase(),
            "wordchar" => c.is_ascii_alphanumeric() || c == '_',
            _ => return Err(format!("unknown character class \"{class}\"")),
        })
    };

    for c in text.chars() {
        if !ok(c)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// `Tcl_GetBoolean`: literally `0` or `1`, or a unique case-insensitive prefix
/// of one of the six words.
fn parse_bool(text: &str) -> Option<bool> {
    if text == "0" {
        return Some(false);
    }
    if text == "1" {
        return Some(true);
    }
    let lowered = text.to_ascii_lowercase();
    const WORDS: [(&str, bool); 6] = [
        ("true", true),
        ("false", false),
        ("yes", true),
        ("no", false),
        ("on", true),
        ("off", false),
    ];
    let mut hit = None;
    for (word, value) in WORDS {
        if word.starts_with(&lowered) {
            if hit.is_some_and(|v| v != value) {
                return None;
            }
            hit = Some(value);
        }
    }
    hit
}

/// Whether an integer literal fits the wide range, which is what separates
/// `wideinteger` from `integer`.
fn fits_wide(text: &str) -> bool {
    let Some(lit) = scan_int(text) else {
        return false;
    };
    let mut value: i128 = 0;
    for d in lit.digits.chars().filter_map(|c| c.to_digit(lit.radix)) {
        value = value * lit.radix as i128 + d as i128;
        if value > u64::MAX as i128 {
            return false;
        }
    }
    if lit.negative {
        -value >= i64::MIN as i128
    } else {
        value <= i64::MAX as i128
    }
}

/// Tcl's double syntax: an integer literal, a decimal number, or one of the
/// non-finite spellings.
fn is_double(text: &str) -> bool {
    if scan_int(text).is_some() {
        return true;
    }
    let body = text.strip_prefix(['-', '+']).unwrap_or(text);
    let folded = body.to_ascii_lowercase();
    if matches!(folded.as_str(), "inf" | "infinity" | "nan") {
        return true;
    }
    let (mantissa, exponent) = match folded.split_once('e') {
        Some((m, e)) => (m, Some(e)),
        None => (folded.as_str(), None),
    };
    let (whole, fraction) = match mantissa.split_once('.') {
        Some((w, f)) => (w, f),
        None => (mantissa, ""),
    };
    let digits = |part: &str| part.is_empty() || valid_digits(part, 10);
    if whole.is_empty() && fraction.is_empty() {
        return false;
    }
    if !digits(whole) || !digits(fraction) {
        return false;
    }
    match exponent {
        None => true,
        Some(e) => {
            let e = e.strip_prefix(['-', '+']).unwrap_or(e);
            valid_digits(e, 10)
        }
    }
}

// ── format ───────────────────────────────────────────────────────────────

#[derive(Default, Clone, Copy)]
struct Flags {
    minus: bool,
    plus: bool,
    space: bool,
    zero: bool,
    hash: bool,
}

/// How wide the integer is before it is converted. Tcl truncates to 32 bits
/// unless the specifier says otherwise, so `format %d 4294967296` is 0.
#[derive(Clone, Copy, PartialEq)]
enum Width {
    Bits16,
    Bits32,
    Bits64,
    Untruncated,
}

/// `format`, following `Tcl_AppendFormatToObj`.
fn format_string(fmt: &str, args: &[String]) -> Result<String, String> {
    let f = chars(fmt);
    let mut out = String::new();
    let mut i = 0;
    let mut next_arg = 0usize;
    let mut positional: Option<bool> = None;

    let take = |index: usize| -> Result<&String, String> {
        args.get(index)
            .ok_or_else(|| "not enough arguments for all format specifiers".to_string())
    };

    while i < f.len() {
        if f[i] != '%' {
            out.push(f[i]);
            i += 1;
            continue;
        }
        i += 1;
        if f.get(i) == Some(&'%') {
            out.push('%');
            i += 1;
            continue;
        }

        // XPG3 position, which must be used by every specifier or by none.
        let mut argument = next_arg;
        let mut is_positional = false;
        let digits = digit_run(&f, i);
        if digits > i && f.get(digits) == Some(&'$') {
            let n: usize = f[i..digits].iter().collect::<String>().parse().unwrap_or(0);
            if n == 0 {
                return Err("\"%n$\" argument index out of range".to_string());
            }
            if n > args.len() {
                return Err("\"%n$\" argument index out of range".to_string());
            }
            argument = n - 1;
            is_positional = true;
            i = digits + 1;
        }
        match positional {
            Some(previous) if previous != is_positional => {
                return Err("cannot mix \"%\" and \"%n$\" conversion specifiers".to_string())
            }
            _ => positional = Some(is_positional),
        }

        let mut flags = Flags::default();
        loop {
            match f.get(i) {
                Some('-') => flags.minus = true,
                Some('+') => flags.plus = true,
                Some(' ') => flags.space = true,
                Some('0') => flags.zero = true,
                Some('#') => flags.hash = true,
                _ => break,
            }
            i += 1;
        }

        let mut width: i64 = 0;
        if f.get(i) == Some(&'*') {
            width = want_int(take(argument)?)?;
            argument += 1;
            i += 1;
        } else {
            let stop = digit_run(&f, i);
            if stop > i {
                width = f[i..stop]
                    .iter()
                    .collect::<String>()
                    .parse()
                    .map_err(|_| "integer value too large to represent".to_string())?;
                i = stop;
            }
        }
        if width < 0 {
            flags.minus = true;
            width = -width;
        }

        let mut precision: Option<i64> = None;
        if f.get(i) == Some(&'.') {
            i += 1;
            let value = if f.get(i) == Some(&'*') {
                argument += 1;
                i += 1;
                want_int(take(argument - 1)?)?
            } else {
                let stop = digit_run(&f, i);
                let v = f[i..stop].iter().collect::<String>().parse().unwrap_or(0);
                i = stop;
                v
            };
            precision = Some(value.max(0));
        }

        let mut size = Width::Bits32;
        if f.get(i) == Some(&'l') && f.get(i + 1) == Some(&'l') {
            size = Width::Untruncated;
            i += 2;
        } else {
            match f.get(i) {
                Some('h') => {
                    size = Width::Bits16;
                    i += 1;
                }
                // `z` and `t` mean the platform's pointer width, which is the
                // wide range on every target this crate builds for.
                Some('l') | Some('j') | Some('q') | Some('z') | Some('t') => {
                    size = Width::Bits64;
                    i += 1;
                }
                Some('L') => {
                    size = Width::Untruncated;
                    i += 1;
                }
                _ => {}
            }
        }

        let Some(&conv) = f.get(i) else {
            return Err(if args.len() <= argument {
                "not enough arguments for all format specifiers".to_string()
            } else {
                "format string ended in middle of field specifier".to_string()
            });
        };
        i += 1;

        let value = take(argument)?;
        next_arg = argument + 1;
        let converted = match conv {
            's' => Signed::plain(match precision {
                Some(p) => value.chars().take(p as usize).collect(),
                None => value.clone(),
            }),
            'c' => Signed::plain(
                char::from_u32(want_int(value)? as u32)
                    .unwrap_or(char::REPLACEMENT_CHARACTER)
                    .to_string(),
            ),
            'd' | 'i' | 'u' | 'o' | 'x' | 'X' | 'b' => {
                integer(conv, flags, precision, size, value)?
            }
            'e' | 'E' | 'f' | 'g' | 'G' => floating(conv, flags, precision, value)?,
            'a' | 'A' | 'p' => {
                return Err(format!("the \"%{conv}\" conversion is not supported yet"))
            }
            other => return Err(format!("bad field specifier \"{other}\"")),
        };
        push_padded(&mut out, converted, flags, width);
    }
    Ok(out)
}

/// A converted number split so that padding can go between its sign and its
/// digits.
struct Signed {
    prefix: String,
    digits: String,
    /// C ignores the `0` flag when an integer conversion states a precision,
    /// and for a value that prints as `inf`.
    zero_pad: bool,
}

impl Signed {
    fn plain(digits: String) -> Signed {
        Signed {
            prefix: String::new(),
            digits,
            zero_pad: true,
        }
    }
}

fn push_padded(out: &mut String, value: Signed, flags: Flags, width: i64) {
    let len = value.prefix.chars().count() + value.digits.chars().count();
    let fill = (width as usize).saturating_sub(len);
    if fill == 0 {
        out.push_str(&value.prefix);
        out.push_str(&value.digits);
        return;
    }
    if flags.zero && value.zero_pad {
        // Tcl pads with zeroes even when the field is left-justified.
        out.push_str(&value.prefix);
        out.extend(std::iter::repeat_n('0', fill));
        out.push_str(&value.digits);
    } else if flags.minus {
        out.push_str(&value.prefix);
        out.push_str(&value.digits);
        out.extend(std::iter::repeat_n(' ', fill));
    } else {
        out.extend(std::iter::repeat_n(' ', fill));
        out.push_str(&value.prefix);
        out.push_str(&value.digits);
    }
}

fn digit_run(f: &[char], from: usize) -> usize {
    let mut i = from;
    while i < f.len() && f[i].is_ascii_digit() {
        i += 1;
    }
    i
}

/// An integer conversion. Without a size modifier Tcl truncates to 32 bits;
/// `ll` skips truncation altogether and prints a sign with the magnitude, which
/// is how a bignum would come out.
fn integer(
    conv: char,
    flags: Flags,
    precision: Option<i64>,
    size: Width,
    value: &str,
) -> Result<Signed, String> {
    let n = parse_int(value.trim_matches(is_ascii_space))
        .ok_or_else(|| format!("expected integer but got \"{value}\""))?;
    let signed_conv = matches!(conv, 'd' | 'i');
    let radix = match conv {
        'o' => 8,
        'x' | 'X' => 16,
        'b' => 2,
        _ => 10,
    };

    let (negative, magnitude) = if size == Width::Untruncated {
        if conv == 'u' {
            return Err("unsigned bignum format is invalid".to_string());
        }
        (n < 0, n.unsigned_abs())
    } else {
        let truncated = match size {
            Width::Bits16 => n as i16 as i64,
            Width::Bits32 => n as i32 as i64,
            _ => n,
        };
        if signed_conv {
            (truncated < 0, truncated.unsigned_abs())
        } else {
            let unsigned = match size {
                Width::Bits16 => truncated as u16 as u64,
                Width::Bits32 => truncated as u32 as u64,
                _ => truncated as u64,
            };
            (false, unsigned)
        }
    };

    let mut digits = match radix {
        8 => format!("{magnitude:o}"),
        16 if conv == 'X' => format!("{magnitude:X}"),
        16 => format!("{magnitude:x}"),
        2 => format!("{magnitude:b}"),
        _ => magnitude.to_string(),
    };
    if let Some(p) = precision {
        // Unlike C, Tcl still prints one digit when the precision is zero.
        let p = (p as usize).max(1);
        if digits.len() < p {
            digits.insert_str(0, &"0".repeat(p - digits.len()));
        }
    }

    let mut prefix = String::new();
    if negative {
        prefix.push('-');
    } else if signed_conv || size == Width::Untruncated {
        if flags.plus {
            prefix.push('+');
        } else if flags.space {
            prefix.push(' ');
        }
    }
    if flags.hash && magnitude != 0 {
        prefix.push_str(match conv {
            'o' => "0o",
            'x' => "0x",
            'X' => "0x",
            'b' => "0b",
            'd' | 'i' => "0d",
            _ => "",
        });
    }
    Ok(Signed {
        prefix,
        digits,
        zero_pad: precision.is_none(),
    })
}

/// A floating-point conversion. Rust's own formatting supplies the digits —
/// it rounds exactly the way C's does — and this reshapes them into C's layout.
fn floating(
    conv: char,
    flags: Flags,
    precision: Option<i64>,
    value: &str,
) -> Result<Signed, String> {
    let x = parse_double(value)
        .ok_or_else(|| format!("expected floating-point number but got \"{value}\""))?;
    if x.is_nan() {
        return Err("floating point value is Not a Number".to_string());
    }
    let upper_case = conv.is_ascii_uppercase();
    let mut prefix = String::new();
    if x.is_sign_negative() {
        prefix.push('-');
    } else if flags.plus {
        prefix.push('+');
    } else if flags.space {
        prefix.push(' ');
    }
    if x.is_infinite() {
        return Ok(Signed {
            prefix,
            digits: if upper_case { "INF" } else { "inf" }.to_string(),
            zero_pad: false,
        });
    }

    let magnitude = x.abs();
    let precision = precision.unwrap_or(6).max(0) as usize;
    let digits = match conv.to_ascii_lowercase() {
        'f' => {
            let mut s = format!("{magnitude:.precision$}");
            if precision == 0 && flags.hash {
                s.push('.');
            }
            s
        }
        'e' => exponential(magnitude, precision, flags.hash, upper_case),
        _ => {
            let significant = precision.max(1);
            let exponent = decimal_exponent(magnitude, significant - 1);
            if exponent < -4 || exponent >= significant as i32 {
                let s = exponential(magnitude, significant - 1, flags.hash, upper_case);
                if flags.hash {
                    s
                } else {
                    let (mantissa, tail) = s.split_at(s.find(['e', 'E']).unwrap_or(s.len()));
                    format!("{}{tail}", strip_zeroes(mantissa))
                }
            } else {
                let places = (significant as i32 - 1 - exponent).max(0) as usize;
                let s = format!("{magnitude:.places$}");
                if flags.hash {
                    if places == 0 {
                        format!("{s}.")
                    } else {
                        s
                    }
                } else {
                    strip_zeroes(&s).to_string()
                }
            }
        }
    };
    Ok(Signed {
        prefix,
        digits,
        zero_pad: true,
    })
}

/// `x.yyye±zz`, with the two-digit exponent C guarantees.
fn exponential(magnitude: f64, precision: usize, hash: bool, upper_case: bool) -> String {
    let raw = format!("{magnitude:.precision$e}");
    let (mantissa, exponent) = raw.split_once('e').expect("exponential form");
    let exponent: i32 = exponent.parse().expect("exponent digits");
    let mut mantissa = mantissa.to_string();
    if precision == 0 && hash {
        mantissa.push('.');
    }
    format!(
        "{mantissa}{}{}{:02}",
        if upper_case { 'E' } else { 'e' },
        if exponent < 0 { '-' } else { '+' },
        exponent.abs()
    )
}

/// The exponent `%e` would print after rounding to `precision` fraction digits,
/// which is what decides whether `%g` uses fixed or exponential form.
fn decimal_exponent(magnitude: f64, precision: usize) -> i32 {
    let raw = format!("{magnitude:.precision$e}");
    raw.split_once('e')
        .and_then(|(_, e)| e.parse().ok())
        .unwrap_or(0)
}

/// Drop the trailing zeroes of a fraction, and the point if nothing is left.
fn strip_zeroes(s: &str) -> &str {
    if !s.contains('.') {
        return s;
    }
    s.trim_end_matches('0').trim_end_matches('.')
}

/// Tcl's double parser, which also takes the integer spellings.
fn parse_double(text: &str) -> Option<f64> {
    let body = text.trim_matches(is_ascii_space);
    if let Some(lit) = scan_int(body) {
        let mut value = 0f64;
        for d in lit.digits.chars().filter_map(|c| c.to_digit(lit.radix)) {
            value = value * lit.radix as f64 + d as f64;
        }
        // An integer spelling is converted as an integer, and an integer has no
        // negative zero — so `format %.2f -0` prints `0.00` where the *double*
        // `-0.0` prints `-0.00`. Measured against tclsh 9.0.4, which agrees for
        // `-00`, `-0x0`, `-0_0` and ` -0 ` and keeps the sign of every nonzero
        // magnitude, including one too large for an `i64`.
        return Some(if lit.negative && value != 0.0 {
            -value
        } else {
            value
        });
    }
    if !is_double(body) {
        return None;
    }
    let cleaned: String = body.chars().filter(|c| *c != '_').collect();
    cleaned.parse().ok()
}
