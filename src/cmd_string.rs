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

use num_bigint::{BigInt, BigUint, Sign};
use num_traits::Zero;

use crate::compiler::{CompileError, Compiler};
use crate::parser::Word;
use crate::runtime::{place_at, take_var, tcl_str, to_tcl_string, var_cell};

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
    /// `[name, place, value …]` → the extended string, stored in the variable
    /// the op reaches itself: `APPEND_VAR` at a name index in the VM's global
    /// table, `APPEND_SLOT` at a frame slot. Reaching the variable here rather
    /// than through `GetVar` / `SetVar` is what lets the values be appended to
    /// the string the variable already holds instead of a copy of it. The name
    /// travels along only so an unset variable can be reported by name.
    /// [`APPEND`] is still emitted for a name the script also uses as an array.
    pub const APPEND_VAR: u16 = BASE + 23;
    pub const APPEND_SLOT: u16 = BASE + 24;

    /// `[string, charIndex]` → the index just past the end of the word holding
    /// that character, and the index of that word's first character.
    ///
    /// Deliberately at `BASE + 32` rather than `+ 25`: ids in the string range
    /// are handed out in blocks so that two people adding subcommands at the
    /// same time cannot pick the same number. `runtime::extension` dispatches
    /// this range by `id >= STRING_BASE`, and an explicit arm above that test
    /// would shadow it silently, so a new id also has to be absent from there.
    pub const WORDEND: u16 = BASE + 32;
    pub const WORDSTART: u16 = BASE + 33;

    /// `[subject, pattern, flags]` → `1` or `0`: how a `switch` clause matches.
    /// `flags` carries `-glob` in bit 0 and `-nocase` in bit 1, and rides as an
    /// operand rather than in the op's inline byte because that is how every
    /// other flag in this module travels (see `push_flag`).
    ///
    /// It lives in the string range rather than beside the frontend's own
    /// `MATCH` because the folding it needs is this module's — the same
    /// matcher and comparator `string match -nocase` and `string equal -nocase`
    /// run, so the two cannot drift into two rules for one question.
    pub const SWITCH_MATCH: u16 = BASE + 34;

    /// `[class, strict, name, place, string]` → 1 or 0, and on 0 the index of
    /// the first character that failed, written into the variable `place`
    /// names. `string is` only touches that variable when the answer is 0 —
    /// measured — so the write cannot be lifted out of the op.
    pub const IS_FAILINDEX: u16 = BASE + 35;
    pub const IS_FAILINDEX_SLOT: u16 = BASE + 36;
}

/// Every subcommand the ensemble knows, in the order the interpreter lists them
/// when it rejects one. All 23 are implemented — `wordend` and `wordstart`
/// were the last two to land, and they refuse only the characters outside ASCII
/// whose word classes need Unicode tables at the reference interpreter's
/// revision.
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

/// The `string is` classes, in the interpreter's listing order. Public for the
/// same reason as [`crate::expr::LEVELS`]: the reference page lists them, and
/// the list it prints has to be the one `string is` resolves against.
pub const CLASSES: &[&str] = &[
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
pub(crate) fn resolve<'t>(name: &str, table: &[&'t str]) -> Option<&'t str> {
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
pub(crate) fn listing(table: &[&str]) -> String {
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
            "wordend" => self.fixed(ext::WORDEND, rest, 2, 2, sub, "string charIndex"),
            "wordstart" => self.fixed(ext::WORDSTART, rest, 2, 2, sub, "string charIndex"),
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
        let mut failindex: Option<String> = None;
        let mut rest = &args[1..args.len() - 1];
        while let Some((w, tail)) = rest.split_first() {
            let opt = self.literal_of(w, "option")?.to_string();
            if opt.len() > 1 && "-strict".starts_with(opt.as_str()) {
                strict = true;
                rest = tail;
            } else if opt.len() > 1 && "-failindex".starts_with(opt.as_str()) {
                let Some((name, after)) = tail.split_first() else {
                    return self.error(USAGE);
                };
                failindex = Some(self.var_name_of(name)?);
                rest = after;
            } else {
                return self.error(format!(
                    "bad option \"{opt}\": must be -strict or -failindex"
                ));
            }
        }
        // `graph`, `print` and `punct` were refused here on the grounds that they
        // needed category tables this crate did not carry. It carries them now
        // (`is_graph` and friends), and the earlier note had the shape of the
        // classes wrong besides: `punct` is the seven punctuation categories and
        // the symbols belong to `graph`, not the other way round.

        let Some(name) = failindex else {
            self.push_str(class);
            self.push_flag(strict);
            self.word(&args[args.len() - 1])?;
            return self.string_op(ext::IS, 3);
        };

        // The variable is written by the op, so it travels as a place the way
        // `append`'s does; a name the script also uses as an array keeps the
        // guarded read rather than being reached past.
        self.push_str(class);
        self.push_flag(strict);
        self.push_str(&name);
        let place = self.var_place(&name);
        self.emit(Op::LoadInt(place.frame_operand()), 1);
        let id = if place.in_frame() {
            ext::IS_FAILINDEX_SLOT
        } else {
            ext::IS_FAILINDEX
        };
        self.word(&args[args.len() - 1])?;
        self.string_op(id, 5)
    }

    /// `append varName ?value ...?` — append to the variable's own string and
    /// yield the new value.
    ///
    /// The values are on the stack before the op runs, and the variable is read
    /// by the op itself, which is the order `Tcl_AppendObjCmd` reads them in:
    /// `append s [set s x]` appends to what the argument left behind. Reaching
    /// the variable there is also what makes the append in place — see
    /// [`append_at`]. A name the script also uses as an array keeps the
    /// read-concatenate-store lowering, whose operand is a guarded read that
    /// refuses an array rather than stringifying one.
    fn cmd_append(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let Some(target) = args.first() else {
            return self.error("wrong # args: should be \"append varName ?value ...?\"");
        };
        // `append $n x`: the variable's name is a value, so it is the same
        // read-concatenate-store shape with the computed-name ops standing in
        // for the read and the store. The read answers the empty string for a
        // variable that does not exist, which is how `append` creates one.
        //
        // `ext::APPEND` is handed the *name* twice over — once as the first
        // operand it reports a bad value under, once under the value it
        // concatenates onto — so the name word is compiled once and duplicated;
        // see [`Compiler::dyn_read_modify`].
        if crate::assoc::target_of(target).is_none() {
            self.dyn_read_modify(target, crate::compiler::Absent::Empty)?;
            // `[name, value]` → `[name, name, value]`: the op wants the name
            // under the value as its diagnostic operand, and `dyn_write_back`
            // wants the one at the bottom.
            self.emit(Op::Dup2, 2);
            self.emit(Op::Rot, 0);
            self.emit(Op::Pop, -1);
            for w in &args[1..] {
                self.word(w)?;
            }
            self.string_op(ext::APPEND, args.len() + 1)?;
            self.dyn_write_back();
            return Ok(());
        }
        // An array element appends in place too: the element is read, the values
        // are concatenated onto it, and it is stored back. `append b(j) hi there`
        // is `hithere`, which tclsh answers and this compiler used to refuse.
        if let crate::assoc::Target::Elem { name, index } = self.target_of(target)? {
            self.push_str(&name);
            self.elem_get_tolerant(&name, &index)?;
            for w in &args[1..] {
                self.word(w)?;
            }
            self.string_op(ext::APPEND, args.len() + 1)?;
            return self.elem_store(&name, &index);
        }
        let name = self.var_name_of(target)?;

        if self.is_array(&name) {
            self.push_str(&name);
            self.scalar_get(&name);
            for w in &args[1..] {
                self.word(w)?;
            }
            self.string_op(ext::APPEND, args.len() + 1)?;
            self.emit(Op::Dup, 1);
            self.emit_set_var(&name);
            return Ok(());
        }

        let id = self.append_target(&name);
        for w in &args[1..] {
            self.word(w)?;
        }
        self.string_op(id, args.len() + 1)
    }

    /// Push what an in-place append addresses — the variable's name, then where
    /// it lives — and answer with the op that closes it. The caller pushes the
    /// values and emits `id` with `2 + values` operands.
    pub(crate) fn append_target(&mut self, name: &str) -> u16 {
        self.push_str(name);
        let place = self.var_place(name);
        self.emit(Op::LoadInt(place.frame_operand()), 1);
        // The `_SLOT` id says "the operand is a frame place"; which of the two
        // frame places it is rides in the operand's sign, so a linked name
        // appends in place exactly as a local does. See [`Place::frame_operand`].
        if place.in_frame() {
            ext::APPEND_SLOT
        } else {
            ext::APPEND_VAR
        }
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
    if id == ext::APPEND_VAR || id == ext::APPEND_SLOT {
        return append_at(vm, id, argc);
    }
    if id == ext::IS_FAILINDEX || id == ext::IS_FAILINDEX_SLOT {
        return is_failindex(vm, id);
    }
    // `scan` lives in this block because it is `format` read backwards, and it
    // reaches its variables itself for the same reason `-failindex` does.
    if id == crate::cmd_scan::ext::SCAN {
        return crate::cmd_scan::extension(vm);
    }
    let mut operands = Vec::with_capacity(argc as usize);
    for _ in 0..argc {
        operands.push(vm.pop());
    }
    operands.reverse();
    let result = dispatch(id, &operands)?;
    vm.push(Value::Str(Arc::new(result)));
    Ok(())
}

/// `append`, and every `set x "$x…"` that is one, with the variable as the op's
/// own operand: `[name, place, value …]`, leaving the new value.
///
/// The value is taken out of the variable rather than read from it, so the
/// string it holds is unshared and the values are appended to it. Growing a
/// string that way is amortized linear in the bytes appended; the
/// read-concatenate-store shape it replaces copied the whole accumulated string
/// on every iteration, which made a build loop quadratic. A string another
/// value is holding is copied instead — it must not change under whoever holds
/// it — and so is one the variable does not hold as a string at all.
fn append_at(vm: &mut VM, id: u16, argc: u8) -> Result<(), String> {
    // The operands are read where they sit rather than popped one at a time:
    // the values are already in order there, and a value that is a string is
    // appended straight out of the one it holds, with nothing copied on the way.
    let count = argc as usize - 2;
    let base = vm.stack.len() - count;
    let place = place_at(&vm.stack[base - 1], id == ext::APPEND_SLOT)?;

    let current = take_var(vm, place);
    // Reading an unset variable is an error only when there is nothing to
    // append — `append x a` creates `x`, `append x` cannot.
    if count == 0 && current == Value::Undef {
        let name = to_tcl_string(&vm.stack[base - 2]);
        vm.stack.truncate(base - 2);
        return Err(format!("can't read \"{name}\": no such variable"));
    }

    let extended = append_onto(current, &vm.stack[base..]);
    vm.stack.truncate(base - 2);
    if let Some(cell) = var_cell(vm, place) {
        *cell = Value::Str(Arc::clone(&extended));
    }
    vm.push(Value::Str(extended));
    Ok(())
}

fn append_onto(current: Value, values: &[Value]) -> Arc<String> {
    let extra: usize = values.iter().map(|value| tcl_str(value).len()).sum();
    if let Value::Str(mut held) = current {
        match Arc::get_mut(&mut held) {
            // Unshared: the values go onto the string the variable held.
            Some(text) => {
                text.reserve(extra);
                extend(text, values);
                return held;
            }
            // Shared with a value the script kept, which must not change under
            // it, so the append lands on a copy.
            None => {
                let mut text = String::with_capacity(held.len() + extra);
                text.push_str(&held);
                extend(&mut text, values);
                return Arc::new(text);
            }
        }
    }
    // Not a string yet — a number, or an unset variable being created.
    let mut text = to_tcl_string(&current);
    text.reserve(extra);
    extend(&mut text, values);
    Arc::new(text)
}

fn extend(text: &mut String, values: &[Value]) {
    for value in values {
        text.push_str(&tcl_str(value));
    }
}

/// `string is class ?-strict? -failindex var string`.
///
/// The stack is `[class, strict, name, place, string]`. Answering 1 leaves the
/// variable exactly as it was — `set v PRE; string is alpha -failindex v abc`
/// leaves `PRE` — so the write happens only on the failing branch.
fn is_failindex(vm: &mut VM, id: u16) -> Result<(), String> {
    let text = to_tcl_string(&vm.pop());
    let place = place_at(&vm.pop(), id == ext::IS_FAILINDEX_SLOT)?;
    let _name = vm.pop();
    let strict = truth(&to_tcl_string(&vm.pop()));
    let class = to_tcl_string(&vm.pop());

    let ok = is_class(&class, strict, &text)?;
    if !ok {
        let at = fail_index(&class, strict, &text);
        if let Some(cell) = var_cell(vm, place) {
            *cell = Value::Int(at as i64);
        }
    }
    vm.push(Value::Str(Arc::new((ok as i32).to_string())));
    Ok(())
}

/// Where a string stopped belonging to its class: the length, in characters, of
/// the longest prefix that still does.
///
/// One rule covers every class, which is what the reference interpreter's
/// answers say it is rather than what its source suggests: `string is integer
/// -failindex v "  12x"` is 4 because `"  12"` is still an integer, `string is
/// double -failindex v 1.2e+` is 3 where the same string as an *integer* is 1,
/// and `string is list -failindex v "{a} {b"` is 4 — the offset of the element
/// that would not parse. A character class falls out of the same rule as the
/// index of its first offending character.
fn fail_index(class: &str, strict: bool, text: &str) -> usize {
    let chars: Vec<char> = text.chars().collect();
    for n in (0..=chars.len()).rev() {
        let prefix: String = chars[..n].iter().collect();
        if is_class(class, strict, &prefix).unwrap_or(false) {
            return n;
        }
    }
    0
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
        ext::SWITCH_MATCH => {
            let flags = want_int(&a[2])?;
            let (glob, nocase) = (flags & 1 == 1, flags & 2 == 2);
            // `switch -regexp`, whose matcher is the regular-expression engine
            // rather than this module's glob one. Bit 2, set by `control.rs`.
            let hit = if flags & 4 == 4 {
                crate::regexp::matches_anywhere(&a[1], &a[0], nocase)?
            } else if glob {
                matches(&chars(&a[1]), &chars(&a[0]), nocase)
            } else if nocase {
                compare(&chars(&a[0]), &chars(&a[1]), true, None) == 0
            } else {
                a[0] == a[1]
            };
            Ok((hit as i32).to_string())
        }
        ext::WORDEND | ext::WORDSTART => {
            let text = chars(&a[0]);
            let at = word_index(&a[1], text.len())?;
            Ok(if id == ext::WORDEND {
                word_end(&text, at)?.to_string()
            } else {
                word_start(&text, at)?.to_string()
            })
        }
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
            // Where the kept tail starts. Computed signed and clamped, not cast
            // first: an empty subject puts `end` at -1, and `string replace {}
            // -5 3` reaches here with `last` at 3, so `last.min(end)` is -1 and
            // casting that to `usize` made `last + 1` overflow and abort the
            // process. tclsh answers `{}` for that, and `X` for `string replace
            // {} -5 3 X`, which is what falls out of a tail that starts at 0.
            let tail = (last.min(end) + 1).max(0) as usize;
            let mut out: String = s[..first].iter().collect();
            if let Some(new) = a.get(3) {
                out.push_str(new);
            }
            out.extend(&s[tail..]);
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

/// The same syntax [`parse_int`] reads, at the precision Tcl 9's integers
/// actually have. `format` needs it: a conversion truncates modulo its width,
/// and a saturated `i64` has already lost the bits the truncation would keep.
pub(crate) fn parse_big(text: &str) -> Option<BigInt> {
    let lit = scan_int(text)?;
    let digits: String = lit.digits.chars().filter(|c| *c != '_').collect();
    let value = BigUint::parse_bytes(digits.as_bytes(), lit.radix)?;
    Some(BigInt::from_biguint(
        if lit.negative && !value.is_zero() {
            Sign::Minus
        } else {
            Sign::Plus
        },
        value,
    ))
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
    parse_int(text.trim_matches(is_ascii_space)).ok_or_else(|| {
        format!(
            "expected integer but got {}",
            crate::runtime::named(text, 50)
        )
    })
}

/// The character `%c` produces for `n`, or the refusal tclsh gives instead.
///
/// `%c` hands its argument to a C `int`, and `Tcl_GetIntFromObj` accepts a
/// 32-bit word written either way — so the window is `i32::MIN ..= u32::MAX`
/// rather than one type's range. Measured against tclsh 9.0.4: `-2147483648`
/// and `4294967295` are accepted, `-2147483649` and `4294967296` each give
/// `integer value too large to represent`. Truncating instead is what the fuzzer
/// found (seed 70123): `format {%+ 5.2c} 4611686018427387903` printed a
/// character where tclsh fails the command.
///
/// Inside the window the low 32 bits are the code point, and one that is not a
/// character becomes U+FFFD — which is what tclsh writes too, verified byte for
/// byte for `-1`, `-2147483648`, `2147483648`, `4294967295` and `1114112`.
fn code_point(n: i64) -> Result<String, String> {
    if n < i32::MIN as i64 || n > u32::MAX as i64 {
        return Err("integer value too large to represent".to_string());
    }
    Ok(char::from_u32(n as u32)
        .unwrap_or(char::REPLACEMENT_CHARACTER)
        .to_string())
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

// ── words ────────────────────────────────────────────────────────────────

/// `string wordend` / `string wordstart`'s character index, clamped the way the
/// interpreter clamps it: an index before the string is the first character and
/// one at or past its end is the last, so neither subcommand can be asked about
/// a character that is not there. Measured against tclsh 9.0.4, which answers
/// `wordend "hello world" -1` with 5 and `wordend "hello world" 11` with 11.
fn word_index(spec: &str, len: usize) -> Result<usize, String> {
    let last = len as i64 - 1;
    Ok(index_of(spec, last)?.clamp(0, last.max(0)) as usize)
}

/// A word character: `string(n)` defines a word as a run of Unicode letters,
/// decimal digits and connector punctuation, and any other single character is
/// a word of its own.
///
/// Answered for ASCII and refused beyond it, which is this module's standing
/// rule for anything resting on Unicode general categories: Rust's tables are a
/// different Unicode revision than Tcl's, so answering from them would be a
/// guess wearing a number. `string is wordchar` draws the line in the same
/// place, and the two agree by construction.
fn is_word_char(c: char) -> Result<bool, String> {
    if (c as u32) >= 0x80 {
        return Err(
            "string wordend/wordstart: characters beyond ASCII need Unicode category tables, \
             which are not built yet"
                .to_string(),
        );
    }
    Ok(c.is_ascii_alphanumeric() || c == '_')
}

/// The index just past the word holding `at`. A non-word character is its own
/// word, so the answer is `at + 1` there.
fn word_end(text: &[char], at: usize) -> Result<usize, String> {
    if text.is_empty() {
        return Ok(0);
    }
    if !is_word_char(text[at])? {
        return Ok(at + 1);
    }
    let mut end = at;
    while end < text.len() && is_word_char(text[end])? {
        end += 1;
    }
    Ok(end)
}

/// The index of the first character of the word holding `at`.
fn word_start(text: &[char], at: usize) -> Result<usize, String> {
    if text.is_empty() {
        return Ok(0);
    }
    if !is_word_char(text[at])? {
        return Ok(at);
    }
    let mut start = at;
    while start > 0 && is_word_char(text[start - 1])? {
        start -= 1;
    }
    Ok(start)
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
    if class == "dict" {
        // Structural, not a character class: a dict is a list of an even number
        // of elements. `string is dict {a 1 b}` is 0 and `string is dict {}` is
        // 1, both measured. Strictness is ignored for the same reason as `list`.
        return Ok(match split_list(text) {
            Ok(elements) => elements.len().is_multiple_of(2),
            Err(_) => false,
        });
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
        if beyond_our_tables(c) {
            return Err(format!(
                "string is {class}: U+{:04X} is categorised by tclsh 9.0.4 and not by \
                 Unicode 16.0, which is the table this build carries",
                c as u32
            ));
        }
        Ok(match class {
            "alnum" => is_alpha(c) || is_digit(c),
            "alpha" => is_alpha(c),
            "control" => matches!(category(c), G::Control | G::Format),
            "digit" => is_digit(c),
            "graph" => is_graph(c),
            "lower" => category(c) == G::LowercaseLetter,
            "print" => is_graph(c) || is_unicode_space(c),
            "punct" => is_punct(c),
            "upper" => category(c) == G::UppercaseLetter,
            "wordchar" => is_wordchar(c),
            _ => return Err(format!("unknown character class \"{class}\"")),
        })
    };

    // Left to right, and a decision reached before an unreadable character is
    // still a decision: `string is alpha 1<newer>` is 0 in tclsh because the `1`
    // settles it, so it must not become a refusal here either.
    for c in text.chars() {
        if !ok(c)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Tcl's character classes are unions of Unicode **general categories** — the
/// `ALPHA_BITS` / `PUNCT_BITS` / `GRAPH_BITS` masks in `tclUtf.c` — and not the
/// derived properties Rust's std exposes. `char::is_alphabetic` is the
/// Alphabetic property, which folds in `Nl` and `Other_Alphabetic`, so it
/// answers differently from `string is alpha`; every function below is the
/// category union, checked against tclsh over the whole code point space.
use unicode_general_category::GeneralCategory as G;

fn category(c: char) -> G {
    unicode_general_category::get_general_category(c)
}

fn is_alpha(c: char) -> bool {
    matches!(
        category(c),
        G::UppercaseLetter
            | G::LowercaseLetter
            | G::TitlecaseLetter
            | G::ModifierLetter
            | G::OtherLetter
    )
}

fn is_digit(c: char) -> bool {
    category(c) == G::DecimalNumber
}

/// The seven punctuation categories — and *not* the symbol ones. The symbols
/// belong to `graph`, which is why `string is punct €` is 0 and
/// `string is graph €` is 1.
fn is_punct(c: char) -> bool {
    matches!(
        category(c),
        G::ConnectorPunctuation
            | G::DashPunctuation
            | G::OpenPunctuation
            | G::ClosePunctuation
            | G::InitialPunctuation
            | G::FinalPunctuation
            | G::OtherPunctuation
    )
}

fn is_wordchar(c: char) -> bool {
    is_alpha(c) || is_digit(c) || category(c) == G::ConnectorPunctuation
}

fn is_graph(c: char) -> bool {
    is_wordchar(c)
        || is_punct(c)
        || matches!(
            category(c),
            G::NonspacingMark
                | G::EnclosingMark
                | G::SpacingMark
                | G::LetterNumber
                | G::OtherNumber
                | G::MathSymbol
                | G::CurrencySymbol
                | G::ModifierSymbol
                | G::OtherSymbol
        )
}

/// The three separator categories. `print` is `graph` plus these — and not plus
/// [`is_space`], which also takes the ASCII control whitespace: `string is print
/// \t` is 0 in tclsh while `string is space \t` is 1.
fn is_unicode_space(c: char) -> bool {
    matches!(
        category(c),
        G::SpaceSeparator | G::LineSeparator | G::ParagraphSeparator
    )
}

/// Code points tclsh 9.0.4 categorises and Unicode 16.0 does not.
///
/// The reference interpreter's tables are ahead of this build's. Sweeping every
/// code point through both engines puts the difference at 4804 of them: 4803
/// that tclsh assigns a category and Unicode 16.0 calls unassigned — verified
/// against Python's `unicodedata` at 16.0.0 as a third opinion — and U+0295,
/// which Unicode 16.0 calls `Ll` while tclsh answers 0 for `string is lower`,
/// so its table must call it `Lo`.
///
/// Everywhere else the two agree exactly, class for class, across the whole
/// space. Rather than answer these from a table that does not know them, the
/// class asks and is refused: a wrong answer for a character a script did use is
/// worse than a refusal naming it. Regenerate this list when the crate's
/// Unicode version catches up with the reference interpreter's, at which point
/// it should be empty.
/// The ranges [`beyond_our_tables`] tests, sorted so it can bisect them.
/// Generated by sweeping `string is` over every code point in both engines;
/// 48 ranges, 4804 code points.
const BEYOND_UNICODE_16: [(u32, u32); 48] = [
    (0x295, 0x295),
    (0x88f, 0x88f),
    (0xc5c, 0xc5c),
    (0xcdc, 0xcdc),
    (0x1acf, 0x1add),
    (0x1ae0, 0x1aeb),
    (0x20c1, 0x20c1),
    (0x2b96, 0x2b96),
    (0xa7ce, 0xa7cf),
    (0xa7d2, 0xa7d2),
    (0xa7d4, 0xa7d4),
    (0xa7f1, 0xa7f1),
    (0xfbc3, 0xfbd2),
    (0xfd90, 0xfd91),
    (0xfdc8, 0xfdce),
    (0x10940, 0x10959),
    (0x10ec5, 0x10ec7),
    (0x10ed0, 0x10ed8),
    (0x10efa, 0x10efb),
    (0x11b60, 0x11b67),
    (0x11db0, 0x11ddb),
    (0x11de0, 0x11de9),
    (0x16ea0, 0x16eb8),
    (0x16ebb, 0x16ed3),
    (0x16ff2, 0x16ff6),
    (0x187f8, 0x187ff),
    (0x18d09, 0x18d1e),
    (0x18d80, 0x18df2),
    (0x1ccfa, 0x1ccfc),
    (0x1ceba, 0x1ced0),
    (0x1cee0, 0x1cef0),
    (0x1e6c0, 0x1e6de),
    (0x1e6e0, 0x1e6f5),
    (0x1e6fe, 0x1e6ff),
    (0x1f6d8, 0x1f6d8),
    (0x1f777, 0x1f77a),
    (0x1f8d0, 0x1f8d8),
    (0x1fa54, 0x1fa57),
    (0x1fa8a, 0x1fa8a),
    (0x1fa8e, 0x1fa8e),
    (0x1fac8, 0x1fac8),
    (0x1facd, 0x1facd),
    (0x1faea, 0x1faea),
    (0x1faef, 0x1faef),
    (0x1fbfa, 0x1fbfa),
    (0x2b73a, 0x2b73f),
    (0x2cea2, 0x2cead),
    (0x323b0, 0x33479),
];

/// How many code points `beyond_our_tables` answers for, summed from the
/// ranges rather than written down beside them.
///
/// Public because the reference page states the figure, and a page that states
/// it from a literal would keep printing 4804 after the table changed.
pub fn beyond_our_tables_count() -> usize {
    BEYOND_UNICODE_16
        .iter()
        .map(|&(lo, hi)| (hi - lo + 1) as usize)
        .sum()
}

fn beyond_our_tables(c: char) -> bool {
    const RANGES: &[(u32, u32)] = &BEYOND_UNICODE_16;
    let cp = c as u32;
    RANGES
        .binary_search_by(|&(lo, hi)| {
            if cp < lo {
                std::cmp::Ordering::Greater
            } else if cp > hi {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
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
                // A spelling too long for an `i64` saturates rather than
                // reading as zero: it is a precision larger than any result,
                // so it belongs on the far side of the size check below, not
                // on the "no precision at all" side. tclsh reports
                // `max size for a Tcl value exceeded` for
                // `format %.99999999999999999999d 1`, which is what saturating
                // produces here.
                //
                // `%.f` — a point with no digits after it — is a precision of
                // zero, so an *empty* run is still zero.
                let text = f[i..stop].iter().collect::<String>();
                i = stop;
                if text.is_empty() {
                    0
                } else {
                    text.parse().unwrap_or(i64::MAX)
                }
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
            'c' => Signed::plain(code_point(want_int(value)?)?),
            'd' | 'i' | 'u' | 'o' | 'x' | 'X' | 'b' => {
                integer(conv, flags, precision, size, value)?
            }
            // `%p` is hexadecimal over the whole word, always prefixed: tclsh
            // prints `0xffffffffffffffff` for -1 where `%#x` prints
            // `0xffffffff`, and `0x0` for zero where `%#x` prints `0`. Those
            // two are the whole difference, so it takes the same path with the
            // width fixed and the prefix made unconditional.
            'p' => integer(
                conv,
                Flags {
                    hash: true,
                    ..flags
                },
                precision,
                Width::Bits64,
                value,
            )?,
            'e' | 'E' | 'f' | 'g' | 'G' => floating(conv, flags, precision, value)?,
            // The one conversion Tcl does not perform: it builds the C
            // conversion and hands the double to the platform library
            // (`generic/tclStringObj.c:2480-2547`), so what `%a` prints is the
            // C library's answer and not Tcl's — and the C libraries this crate
            // is built against do not agree. See BUGS.md; the wording names the
            // library rather than promising a port.
            'a' | 'A' => {
                return Err(format!(
                    "the \"%{conv}\" conversion is not supported: tclsh hands it to the platform \
                     C library, whose answer differs between the libraries this frontend is \
                     built against"
                ))
            }
            other => return Err(format!("bad field specifier \"{other}\"")),
        };
        push_padded(&mut out, converted, flags, width)?;
    }
    Ok(out)
}

/// The largest string `format` will build.
///
/// A field width and a precision both come straight from the script, and both
/// scale the result: `format %9223372036854775807d 1` asked for a string of
/// 9 exabytes, and the allocator's refusal is an abort — the process dies with
/// nothing for the script to catch. tclsh 9.0.4 reports
/// [`TOO_BIG`] for the same input, so refusing before the allocation is asked
/// for both matches it and keeps the failure catchable.
///
/// The limit is 2 GiB rather than tclsh's own: tclsh 9.0's `Tcl_Size` is 64-bit
/// and `format %4294967296d 1` really does build a 4 GiB string there, which is
/// a memory bomb rather than a useful answer. `string repeat` already refuses
/// above 2 GiB (see [`ext::REPEAT`]'s arm), and the two now agree.
const MAX_VALUE_BYTES: usize = i32::MAX as usize;

/// What tclsh 9.0.4 reports when a `format` result cannot be had. Measured:
/// `format %9223372036854775807d 1` and `format %.9223372036854775807f 1e-5`
/// both print it.
const TOO_BIG: &str = "max size for a Tcl value exceeded";

/// The largest precision Rust's formatter accepts: it holds one in a `u16`, and
/// anything above is `Formatting argument out of range` — a panic, not an error.
///
/// `format`'s precision is whatever the script wrote, and tclsh prints every
/// digit asked for (`string length [format %.65536f 1.0]` is 65538 there), so
/// the digits past this are produced by [`extend_exact`] instead.
const RUST_MAX_PRECISION: usize = u16::MAX as usize;

/// Append the fraction digits Rust's formatter would not produce.
///
/// A double's exact decimal expansion is finite — at most 1_074 fraction digits,
/// for the smallest subnormal — so a precision above the expansion asks only for
/// zeroes, and every digit past [`RUST_MAX_PRECISION`] is one of them. Appending
/// them is therefore exact, not an approximation.
fn extend_exact(digits: &mut String, precision: usize) -> Result<(), String> {
    if precision <= RUST_MAX_PRECISION {
        return Ok(());
    }
    let extra = precision - RUST_MAX_PRECISION;
    if digits.len().saturating_add(extra) > MAX_VALUE_BYTES {
        return Err(TOO_BIG.to_string());
    }
    digits.extend(std::iter::repeat_n('0', extra));
    Ok(())
}

/// What a left-justified field (`-`) does with the `0` flag, which Tcl answers
/// three different ways depending on the conversion. Each is tclsh 9.0.4's
/// answer for `%-08…` of 42, and none of them is C's — C99 says `-` always
/// overrides `0`:
///
/// | | tclsh | this |
/// | --- | --- | --- |
/// | `%-08d` | `00000042` | [`Justify::ZeroesLeft`] |
/// | `%-08.2f` | `42.00␠␠␠` | [`Justify::SpacesRight`] |
/// | `%-08s` | `42000000` | [`Justify::FillRight`] |
/// | `%-08c` | `*0000000` | [`Justify::FillRight`] |
#[derive(Clone, Copy, PartialEq)]
enum Justify {
    /// The `0` wins and the zeroes stay on the left, so `-` changes nothing.
    /// The integer conversions.
    ZeroesLeft,
    /// The `0` is dropped and the field is padded on the right with spaces.
    /// The floating conversions.
    SpacesRight,
    /// The `0` is kept as the fill but the fill moves to the right. `%s`, `%c`.
    FillRight,
}

/// A converted number split so that padding can go between its sign and its
/// digits.
struct Signed {
    prefix: String,
    digits: String,
    /// C ignores the `0` flag when an integer conversion states a precision,
    /// and for a value that prints as `inf`.
    zero_pad: bool,
    /// What `-` does to this conversion's padding. See [`Justify`].
    justify: Justify,
}

impl Signed {
    /// A string or a character: no sign to pad inside, and a left-justified
    /// field keeps the `0` as its fill and moves it to the right.
    fn plain(digits: String) -> Signed {
        Signed {
            prefix: String::new(),
            digits,
            zero_pad: true,
            justify: Justify::FillRight,
        }
    }
}

fn push_padded(out: &mut String, value: Signed, flags: Flags, width: i64) -> Result<(), String> {
    let len = value.prefix.chars().count() + value.digits.chars().count();
    let fill = (width as usize).saturating_sub(len);
    // The width is the script's, so the padding is too. Refuse before asking the
    // allocator for something it will die on — see [`MAX_VALUE_BYTES`]. The
    // running total is checked, not just this field, so a format string of many
    // wide specifiers cannot walk past the limit one field at a time.
    if out
        .len()
        .saturating_add(fill)
        .saturating_add(value.digits.len())
        > MAX_VALUE_BYTES
    {
        return Err(TOO_BIG.to_string());
    }
    if fill == 0 {
        out.push_str(&value.prefix);
        out.push_str(&value.digits);
        return Ok(());
    }
    let zero_fill = flags.zero && value.zero_pad;
    // Where the fill goes, and what it is. Without `-` every conversion agrees:
    // the fill is zeroes if the `0` flag asked for them, and it goes on the
    // left. With `-` the three families stop agreeing, and stop agreeing with C
    // — see [`Justify`].
    let (pad_right, fill_char) = match (flags.minus, zero_fill) {
        (false, zero) => (false, if zero { '0' } else { ' ' }),
        // Left-justified without the `0` flag: spaces on the right, as in C.
        (true, false) => (true, ' '),
        (true, true) => match value.justify {
            Justify::ZeroesLeft => (false, '0'),
            Justify::SpacesRight => (true, ' '),
            Justify::FillRight => (true, '0'),
        },
    };
    if pad_right {
        out.push_str(&value.prefix);
        out.push_str(&value.digits);
        out.extend(std::iter::repeat_n(fill_char, fill));
    } else if fill_char == '0' {
        // The zeroes go between the sign and the digits, never before the sign.
        out.push_str(&value.prefix);
        out.extend(std::iter::repeat_n('0', fill));
        out.push_str(&value.digits);
    } else {
        out.extend(std::iter::repeat_n(' ', fill));
        out.push_str(&value.prefix);
        out.push_str(&value.digits);
    }
    Ok(())
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
///
/// The truncation is *modular*, not saturating, and the value it truncates is
/// the script's own — which in Tcl 9 is arbitrary precision. That is why the
/// arithmetic here runs on a `BigInt` rather than on an `i64`: reading the
/// argument into an `i64` first would clamp `2**64` to `i64::MAX` and print
/// `-1` where `Tcl_AppendFormatToObj` prints `0`, because it reduces the whole
/// bignum modulo the conversion's width (`generic/tclStringObj.c`, the
/// `TCL_NUMBER_BIG` arm of the integer conversions). tclsh 9.0.4:
///
/// ```text
/// format %d  18446744073709551616  ->  0
/// format %d  18446744073709551617  ->  1
/// format %ld 18446744073709551615  ->  -1
/// format %lx 18446744073709551615  ->  ffffffffffffffff
/// ```
fn integer(
    conv: char,
    flags: Flags,
    precision: Option<i64>,
    size: Width,
    value: &str,
) -> Result<Signed, String> {
    let n = parse_big(value.trim_matches(is_ascii_space)).ok_or_else(|| {
        format!(
            "expected integer but got {}",
            crate::runtime::named(value, 50)
        )
    })?;
    let signed_conv = matches!(conv, 'd' | 'i');
    let radix = match conv {
        'o' => 8,
        'x' | 'X' | 'p' => 16,
        'b' => 2,
        _ => 10,
    };

    let (negative, magnitude) = if size == Width::Untruncated {
        // Nothing was truncated, so there is no width whose bit pattern could
        // stand in for a negative value: tclsh refuses only then, and prints
        // the magnitude for every value that is not negative
        // (`format %llu 340282366920938463463374607431768211457` is that
        // number back, `format %llu -1` is this refusal).
        if conv == 'u' && n.sign() == Sign::Minus {
            return Err("unsigned bignum format is invalid".to_string());
        }
        // Sign and magnitude, printed apart: `format %llx -1` is `-1` in tclsh
        // and not `ffff...`, because nothing was truncated to a width whose
        // top bit could carry the sign.
        (n.sign() == Sign::Minus, n.magnitude().clone())
    } else {
        let bits = match size {
            Width::Bits16 => 16u32,
            Width::Bits32 => 32,
            _ => 64,
        };
        // The two's-complement bit pattern: the value reduced into
        // `0 ..= 2**bits - 1`. num-bigint's bitwise operators act on the
        // two's-complement form with infinite sign extension, so masking off
        // the low `bits` is that reduction and it works for a negative value
        // without a separate branch.
        let modulus = BigUint::from(1u8) << bits;
        let mask = BigInt::from(modulus.clone() - BigUint::from(1u8));
        let pattern = (&n & &mask).magnitude().clone();
        if signed_conv && pattern >= (BigUint::from(1u8) << (bits - 1)) {
            (true, modulus - pattern)
        } else {
            (false, pattern)
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
            // An integer conversion pads on the *left*, so the precision scales
            // the result here the way it does for a double — and the number is
            // the script's. `format %.9223372036854775807d 1` asked for 9
            // exabytes of leading zeroes; see [`MAX_VALUE_BYTES`].
            if p > MAX_VALUE_BYTES {
                return Err(TOO_BIG.to_string());
            }
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
    // `%p` prefixes a zero too; every other conversion follows C and does not.
    if flags.hash && (!magnitude.is_zero() || conv == 'p') {
        prefix.push_str(match conv {
            'o' => "0o",
            'x' | 'p' => "0x",
            'X' => "0x",
            'b' => "0b",
            'd' | 'i' => "0d",
            // `%#u` prints no prefix, but `%#llu` prints `0d`: the untruncated
            // conversions share one arm in `Tcl_AppendFormatToObj`, so an
            // unsigned bignum is decorated as a decimal is. tclsh 9.0.4:
            // `format %#u 1` is `1` and `format %#llu 1` is `0d1`.
            'u' if size == Width::Untruncated => "0d",
            _ => "",
        });
    }
    Ok(Signed {
        prefix,
        digits,
        zero_pad: precision.is_none(),
        justify: Justify::ZeroesLeft,
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
    let x = parse_double(value).ok_or_else(|| {
        format!(
            "expected floating-point number but got {}",
            crate::runtime::named(value, 50)
        )
    })?;
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
            justify: Justify::SpacesRight,
        });
    }

    let magnitude = x.abs();
    let precision = precision.unwrap_or(6).max(0) as usize;
    // Checked once, up front, rather than only where the digits are built: `%g`
    // strips trailing zeroes, so its digits can be produced at a clamped
    // precision and come out identical — which would let a precision far past
    // any possible result quietly succeed. tclsh refuses on the precision
    // asked for, whatever the conversion does with it.
    if precision > MAX_VALUE_BYTES {
        return Err(TOO_BIG.to_string());
    }
    let digits = match conv.to_ascii_lowercase() {
        'f' => {
            let mut s = fixed(magnitude, precision)?;
            if precision == 0 && flags.hash {
                s.push('.');
            }
            s
        }
        'e' => exponential(magnitude, precision, flags.hash, upper_case)?,
        _ => {
            let significant = precision.max(1);
            let exponent = decimal_exponent(magnitude, significant - 1);
            // Compared as `i64`: `significant` is the script's precision, and
            // an `i32` cast of one past 2^31 wraps to a negative and sends the
            // conversion down the wrong branch.
            if exponent < -4 || exponent as i64 >= significant as i64 {
                if flags.hash {
                    exponential(magnitude, significant - 1, true, upper_case)?
                } else {
                    // The trailing zeroes are about to be stripped, so the
                    // digits past the double's exact expansion need never be
                    // produced: clamping to what Rust's formatter takes gives
                    // the same answer for a fraction of the work.
                    let s = exponential(
                        magnitude,
                        (significant - 1).min(RUST_MAX_PRECISION),
                        false,
                        upper_case,
                    )?;
                    let (mantissa, tail) = s.split_at(s.find(['e', 'E']).unwrap_or(s.len()));
                    format!("{}{tail}", strip_zeroes(mantissa))
                }
            } else {
                let places = (significant as i64 - 1 - exponent as i64).max(0) as usize;
                if flags.hash {
                    let s = fixed(magnitude, places)?;
                    if places == 0 {
                        format!("{s}.")
                    } else {
                        s
                    }
                } else {
                    // Stripped as above, so the clamp is again exact.
                    let s = fixed(magnitude, places.min(RUST_MAX_PRECISION))?;
                    strip_zeroes(&s).to_string()
                }
            }
        }
    };
    Ok(Signed {
        prefix,
        digits,
        zero_pad: true,
        justify: Justify::SpacesRight,
    })
}

/// `magnitude` with `precision` fraction digits, for a precision Rust's
/// formatter will not take. See [`RUST_MAX_PRECISION`].
fn fixed(magnitude: f64, precision: usize) -> Result<String, String> {
    let mut s = format!("{magnitude:.p$}", p = precision.min(RUST_MAX_PRECISION));
    extend_exact(&mut s, precision)?;
    Ok(s)
}

/// `x.yyye±zz`, with the two-digit exponent C guarantees.
fn exponential(
    magnitude: f64,
    precision: usize,
    hash: bool,
    upper_case: bool,
) -> Result<String, String> {
    let raw = format!("{magnitude:.p$e}", p = precision.min(RUST_MAX_PRECISION));
    let (mantissa, exponent) = raw.split_once('e').expect("exponential form");
    let exponent: i32 = exponent.parse().expect("exponent digits");
    let mut mantissa = mantissa.to_string();
    // The mantissa carries the fraction digits, so it is what grows past
    // Rust's ceiling.
    extend_exact(&mut mantissa, precision)?;
    if precision == 0 && hash {
        mantissa.push('.');
    }
    Ok(format!(
        "{mantissa}{}{}{:02}",
        if upper_case { 'E' } else { 'e' },
        if exponent < 0 { '-' } else { '+' },
        exponent.abs()
    ))
}

/// The exponent `%e` would print after rounding to `precision` fraction digits,
/// which is what decides whether `%g` uses fixed or exponential form.
///
/// The precision is clamped rather than extended: rounding a double past its
/// exact decimal expansion cannot move the decimal point, so every precision
/// above [`RUST_MAX_PRECISION`] gives the exponent that one gives.
fn decimal_exponent(magnitude: f64, precision: usize) -> i32 {
    let raw = format!("{magnitude:.p$e}", p = precision.min(RUST_MAX_PRECISION));
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
pub(crate) fn parse_double(text: &str) -> Option<f64> {
    let body = text.trim_matches(is_ascii_space);
    if let Some(lit) = scan_int(body) {
        // Rust's own parser, because it is correctly rounded and a digit-by-
        // digit `value * radix + d` accumulation in `f64` is not: nineteen
        // decimal digits accumulate enough error to move the result several
        // thousand ULP, and `format %f 9223372036854775807` printed
        // `9223372036854777856.000000` where tclsh 9.0.4 prints
        // `9223372036854775808.000000`. A radix other than ten goes through a
        // bignum only to be re-spelled in decimal, so the same correctly
        // rounded parser answers for `0x`, `0o` and `0b` too. An overflow is
        // an infinity here as it is in tclsh, not a refusal.
        let digits: String = lit.digits.chars().filter(|c| *c != '_').collect();
        let decimal = if lit.radix == 10 {
            digits
        } else {
            BigUint::parse_bytes(digits.as_bytes(), lit.radix)?.to_string()
        };
        let value: f64 = decimal.parse().unwrap_or(f64::INFINITY);
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
