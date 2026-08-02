//! Associative data: array variables, the `array` command, the `dict` command,
//! and the Tcl list syntax both of them are stated in.
//!
//! The two structures are not the same kind of thing, and the difference drives
//! the whole module. An **array** is a property of a *variable*: `a(i)` names an
//! element, `$a` on its own is an error, and nothing in Tcl ever holds an array
//! as a value. A **dict** is a *value* — a list of alternating keys and values —
//! so `dict get` takes a string, `dict keys` returns a string, and a dict can be
//! passed around, nested, and printed like any other value.
//!
//! An array therefore lives wherever its *variable* lives — a `Value::Hash` in
//! fusevm's global table for a script's own variable, and in the call frame's
//! slot for a procedure's local — reached only through the ops below. Every one
//! of them takes the variable's [`Place`] rather than a name index, which is
//! what lets one op serve both; dicts never touch a variable except when `dict
//! set` writes one back.
//!
//! ## Why the VM's hash ops are not used for element access
//!
//! fusevm's `HashGet`/`HashSet`/`HashExists`/`HashDelete` are total: a missing
//! key reads as `Value::Undef`, and a store into a global that is not a
//! `Value::Hash` is silently dropped (`vm.rs`, `Op::HashSet`). Tcl distinguishes
//! three cases that all collapse to `Undef` there — no such variable, variable
//! isn't array, no such element in array — and each is a different error. The
//! extension handler reaches the variable itself instead, which also avoids the
//! whole-map clone that `GetVar` would perform on every element access. The
//! representation is still fusevm's `Value::Hash`, so the native ops remain
//! applicable to the same data. `ArrayLen`/`ArrayGet` *are* used, for `dict
//! for`'s cursor over a `Value::Array` of pairs, where the VM's semantics are
//! exactly Tcl's.
//!
//! Behavior is matched against tclsh 9.0.4. The list quoting and splitting
//! routines are ports of `TclScanElement`/`TclConvertElement`/`FindElement` in
//! its `generic/tclUtil.c`, and the glob matcher is a port of
//! `Tcl_StringCaseMatch` from the same file; the string forms they produce are
//! observable, so they are ported rather than approximated.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use fusevm::{Op, Value, VM};

use crate::compiler::{ext, CompileError, Compiler, Place};
use crate::parser::{Part, Word};
use crate::runtime::{tcl_int, to_tcl_string};

// ─── Tcl list syntax ──────────────────────────────────────────────────────

/// The characters Tcl treats as white space when parsing and generating lists
/// (`TclIsSpaceProcM`).
fn is_space(b: u8) -> bool {
    b == b' ' || (0x09..=0x0d).contains(&b)
}

/// Split a list into its elements. `kind` is `list` or `dict` and appears
/// verbatim in the error messages, as it does in the reference implementation's
/// shared `FindElement`.
pub(crate) fn split(src: &str, kind: &str) -> Result<Vec<String>, String> {
    let s = src.as_bytes();
    let mut out = Vec::new();
    let mut p = 0usize;

    loop {
        while p < s.len() && is_space(s[p]) {
            p += 1;
        }
        if p >= s.len() {
            return Ok(out);
        }

        let mut braces = 0usize;
        let mut quoted = false;
        match s[p] {
            b'{' => {
                braces = 1;
                p += 1;
            }
            b'"' => {
                quoted = true;
                p += 1;
            }
            _ => {}
        }
        let start = p;
        // A brace-quoted element is its own text; anything else may carry
        // backslash sequences that have to be resolved before use.
        let mut literal = true;

        let size = loop {
            if p >= s.len() {
                if braces != 0 {
                    return Err(format!("unmatched open brace in {kind}"));
                }
                if quoted {
                    return Err(format!("unmatched open quote in {kind}"));
                }
                break p - start;
            }
            match s[p] {
                b'{' if braces != 0 => braces += 1,
                b'}' if braces > 1 => braces -= 1,
                b'}' if braces == 1 => {
                    let size = p - start;
                    p += 1;
                    if p >= s.len() || is_space(s[p]) {
                        break size;
                    }
                    return Err(junk(src, p, kind, "braces"));
                }
                b'\\' => {
                    if braces == 0 {
                        literal = false;
                    }
                    let (_, next) = crate::parser::backslash_at(src, p);
                    p = next - 1;
                }
                b'"' if quoted => {
                    let size = p - start;
                    p += 1;
                    if p >= s.len() || is_space(s[p]) {
                        break size;
                    }
                    return Err(junk(src, p, kind, "quotes"));
                }
                c if is_space(c) && braces == 0 && !quoted => break p - start,
                _ => {}
            }
            p += 1;
        };

        let text = &src[start..start + size];
        out.push(if literal {
            text.to_string()
        } else {
            collapse(text)
        });
        while p < s.len() && is_space(s[p]) {
            p += 1;
        }
    }
}

/// The "followed by junk" diagnostic, which quotes up to twenty bytes of the
/// offending text.
///
/// The quoted text comes from [`crate::list::junk_prefix`] rather than from a
/// second copy of the same walk: the rule is the reference implementation's,
/// and it has a boundary case — a twenty-byte cap landing inside a multi-byte
/// character — that is a panic when it is got wrong.
fn junk(src: &str, at: usize, kind: &str, what: &str) -> String {
    format!(
        "{kind} element in {what} followed by \"{}\" instead of space",
        crate::list::junk_prefix(src, at)
    )
}

/// Resolve the backslash sequences in an element that was not brace-quoted.
fn collapse(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' {
            let (replacement, next) = crate::parser::backslash_at(text, i);
            out.push_str(&replacement);
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

/// How an element has to be written so that splitting the result gives it back.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Quoting {
    /// Verbatim.
    None,
    /// Wrapped in braces.
    Brace,
    /// Backslash before every special character, braces included.
    Escape,
    /// Backslash before every special character except braces. Reached only for
    /// elements whose sole reason to quote is a `]` or an interior `"`.
    Mask,
}

/// Choose the quoting for an element — a port of `TclScanElement` with the
/// reference implementation's `COMPAT` behavior, which is what its `#define
/// COMPAT 1` selects.
fn quoting_of(src: &str, quote_hash: bool) -> Quoting {
    if src.is_empty() {
        return Quoting::Brace;
    }
    let b = src.as_bytes();
    let mut nesting = 0i64;
    let mut forbid_none = false;
    let mut require_escape = false;
    let mut prefer_escape = false;
    let mut prefer_brace = quote_hash && b[0] == b'#';

    // A leading brace or quote would be read as element-delimiting syntax.
    if b[0] == b'{' || b[0] == b'"' {
        forbid_none = true;
        prefer_brace = true;
    }

    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'{' => nesting += 1,
            b'}' => {
                nesting -= 1;
                if nesting < 0 {
                    require_escape = true;
                }
            }
            b']' | b'"' => {
                forbid_none = true;
                prefer_escape = true;
            }
            b'[' | b'$' | b';' => {
                forbid_none = true;
                prefer_brace = true;
            }
            b'\\' => {
                // A final backslash, or one before a newline, would run into or
                // through a closing brace.
                if i + 1 >= b.len() || b[i + 1] == b'\n' {
                    require_escape = true;
                    i += 2;
                    continue;
                }
                if matches!(b[i + 1], b'{' | b'}' | b'\\') {
                    i += 1;
                }
                forbid_none = true;
                prefer_brace = true;
            }
            c if is_space(c) => {
                forbid_none = true;
                prefer_brace = true;
            }
            _ => {}
        }
        i += 1;
    }
    if nesting > 0 {
        require_escape = true;
    }

    if require_escape {
        Quoting::Escape
    } else if !forbid_none {
        Quoting::None
    } else if prefer_escape && !prefer_brace {
        Quoting::Mask
    } else {
        Quoting::Brace
    }
}

/// Write one element — a port of `TclConvertElement`.
///
/// `quote_hash` is false for every element but the first of a list: only there
/// would a leading `#` be read as a comment.
fn quote_element(src: &str, quote_hash: bool) -> String {
    if src.is_empty() {
        return "{}".to_string();
    }
    let mut mode = quoting_of(src, quote_hash);
    let mut out = String::new();
    let mut rest = src;

    if quote_hash && src.starts_with('#') {
        if mode == Quoting::Escape {
            out.push_str("\\#");
            rest = &src[1..];
        } else {
            mode = Quoting::Brace;
        }
    }

    match mode {
        Quoting::None => out.push_str(rest),
        Quoting::Brace => {
            out.push('{');
            out.push_str(rest);
            out.push('}');
        }
        Quoting::Escape | Quoting::Mask => {
            let escape_braces = mode == Quoting::Escape;
            for c in rest.chars() {
                match c {
                    ']' | '[' | '$' | ';' | ' ' | '\\' | '"' => {
                        out.push('\\');
                        out.push(c);
                    }
                    '{' | '}' => {
                        if escape_braces {
                            out.push('\\');
                        }
                        out.push(c);
                    }
                    '\u{c}' => out.push_str("\\f"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    '\u{b}' => out.push_str("\\v"),
                    _ => out.push(c),
                }
            }
        }
    }
    out
}

/// Join elements into a list.
pub(crate) fn join<S: AsRef<str>>(elements: &[S]) -> String {
    let mut out = String::new();
    for (i, e) in elements.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&quote_element(e.as_ref(), i == 0));
    }
    out
}

// ─── glob matching ────────────────────────────────────────────────────────

/// `string match` semantics — a port of `Tcl_StringCaseMatch` in its
/// case-sensitive mode, which is what `array names`, `array get`, `array unset`,
/// `dict keys` and `dict values` filter with.
pub(crate) fn string_match(text: &str, pattern: &str) -> bool {
    matches(
        &text.chars().collect::<Vec<_>>(),
        &pattern.chars().collect::<Vec<_>>(),
    )
}

fn matches(mut s: &[char], mut p: &[char]) -> bool {
    loop {
        let Some(&pc) = p.first() else {
            return s.is_empty();
        };
        if s.is_empty() && pc != '*' {
            return false;
        }

        match pc {
            '*' => {
                while p.first() == Some(&'*') {
                    p = &p[1..];
                }
                if p.is_empty() {
                    return true;
                }
                loop {
                    if matches(s, p) {
                        return true;
                    }
                    if s.is_empty() {
                        return false;
                    }
                    s = &s[1..];
                }
            }
            '?' => {
                p = &p[1..];
                s = &s[1..];
            }
            '[' => {
                p = &p[1..];
                let ch = s[0];
                s = &s[1..];
                loop {
                    match p.first() {
                        None | Some(&']') => return false,
                        Some(&start) => {
                            p = &p[1..];
                            if p.first() == Some(&'-') {
                                p = &p[1..];
                                let Some(&end) = p.first() else {
                                    return false;
                                };
                                p = &p[1..];
                                // `[z-a]` is the same range as `[a-z]`.
                                if (start <= ch && ch <= end) || (end <= ch && ch <= start) {
                                    break;
                                }
                            } else if start == ch {
                                break;
                            }
                        }
                    }
                }
                // Skip to just past the closing bracket. An unclosed set that
                // has already matched succeeds only if the text is exhausted.
                while p.first() != Some(&']') {
                    if p.is_empty() {
                        return s.is_empty();
                    }
                    p = &p[1..];
                }
                p = &p[1..];
            }
            _ => {
                let mut lit = pc;
                if pc == '\\' {
                    p = &p[1..];
                    match p.first() {
                        Some(&c) => lit = c,
                        None => return false,
                    }
                }
                if s[0] != lit {
                    return false;
                }
                p = &p[1..];
                s = &s[1..];
            }
        }
    }
}

// ─── dictionaries ─────────────────────────────────────────────────────────

/// A dict: keys in the order they were first inserted, which is the order
/// `dict keys`, `dict values`, `dict get` with no keys and `dict for` all use.
pub(crate) struct Dict {
    entries: Vec<(String, String)>,
    index: HashMap<String, usize>,
}

impl Dict {
    fn new() -> Dict {
        Dict {
            entries: Vec::new(),
            index: HashMap::new(),
        }
    }

    /// Read a dict from its string form. An odd number of elements is the same
    /// failure as a key with nothing after it.
    pub(crate) fn parse(src: &str) -> Result<Dict, String> {
        let elements = split(src, "dict")?;
        if elements.len() % 2 != 0 {
            return Err("missing value to go with key".to_string());
        }
        let mut d = Dict::new();
        let mut it = elements.into_iter();
        while let (Some(k), Some(v)) = (it.next(), it.next()) {
            d.put(k, v);
        }
        Ok(d)
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.index.get(key).map(|&i| self.entries[i].1.as_str())
    }

    /// Insert or update. An existing key keeps the position it was first given.
    fn put(&mut self, key: String, value: String) {
        match self.index.get(&key) {
            Some(&i) => self.entries[i].1 = value,
            None => {
                self.index.insert(key.clone(), self.entries.len());
                self.entries.push((key, value));
            }
        }
    }

    fn remove(&mut self, key: &str) {
        if let Some(i) = self.index.remove(key) {
            self.entries.remove(i);
            for slot in self.index.values_mut() {
                if *slot > i {
                    *slot -= 1;
                }
            }
        }
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    /// The canonical string form: every key and value written as a list element.
    fn to_list(&self) -> String {
        let mut flat = Vec::with_capacity(self.entries.len() * 2);
        for (k, v) in &self.entries {
            flat.push(k.as_str());
            flat.push(v.as_str());
        }
        join(&flat)
    }
}

/// Walk a key path through nested dicts, returning the innermost dict.
fn trace_path(dict: &str, keys: &[String]) -> Result<Dict, String> {
    let mut current = Dict::parse(dict)?;
    for key in keys {
        let Some(next) = current.get(key) else {
            return Err(format!("key \"{key}\" not known in dictionary"));
        };
        current = Dict::parse(next)?;
    }
    Ok(current)
}

// ─── compile-time lowering ────────────────────────────────────────────────

/// What a variable-name word names.
pub(crate) enum Target {
    Scalar(String),
    /// `a(i)`, where the index may still contain substitutions.
    Elem {
        name: String,
        index: Vec<Part>,
    },
}

/// Resolve a word used as a variable name. Returns `None` when the name is not
/// known at compile time.
///
/// The parentheses are ordinary text here — `set` receives the string `a(i)`
/// and interprets it, so the split is on the *first* `(` with the name ending
/// at the final `)`. `q(x)y` names a scalar, and `p(a(b))` names element `a(b)`
/// of array `p`, both of which the reference implementation agrees with.
pub(crate) fn target_of(word: &Word) -> Option<Target> {
    if let Some(text) = word.as_literal() {
        let Some(open) = text.find('(') else {
            return Some(Target::Scalar(text.to_string()));
        };
        if !text.ends_with(')') {
            return Some(Target::Scalar(text.to_string()));
        }
        let index = &text[open + 1..text.len() - 1];
        return Some(Target::Elem {
            name: text[..open].to_string(),
            index: if index.is_empty() {
                Vec::new()
            } else {
                vec![Part::Lit(index.to_string())]
            },
        });
    }

    // A word with substitutions can still name an element as long as the array
    // name and both parentheses are literal: `a($i)`, `a(k$i.x)`.
    let (Some(Part::Lit(first)), Some(Part::Lit(last))) = (word.parts.first(), word.parts.last())
    else {
        return None;
    };
    let open = first.find('(')?;
    if !last.ends_with(')') {
        return None;
    }
    // A single-part word that reached here cannot be a `Lit`, so there are at
    // least two parts and the slice below is well formed.
    let name = first[..open].to_string();
    let mut index = Vec::new();
    let head = &first[open + 1..];
    if !head.is_empty() {
        index.push(Part::Lit(head.to_string()));
    }
    index.extend(word.parts[1..word.parts.len() - 1].iter().cloned());
    let tail = &last[..last.len() - 1];
    if !tail.is_empty() {
        index.push(Part::Lit(tail.to_string()));
    }
    Some(Target::Elem { name, index })
}

impl Compiler {
    /// Emit the index of `a(i)`, whose parts concatenate like any other word.
    pub(crate) fn index_value(&mut self, index: &[Part]) -> Result<(), CompileError> {
        self.word(&Word {
            parts: index.to_vec(),
            ..Word::default()
        })
    }

    /// Note that `name` is used as an array somewhere in the script, so that the
    /// second compilation pass knows to guard scalar uses of it.
    fn note_array(&mut self, name: &str) {
        self.seen_arrays.insert(name.to_string());
    }

    pub(crate) fn is_array(&self, name: &str) -> bool {
        self.arrays.contains(name)
    }

    /// Where an array variable lives, as the single operand every `array`,
    /// element and `unset` op takes.
    ///
    /// One integer encodes both homes a variable can have, so one op serves a
    /// script's own variable and a procedure's local alike: a global is its
    /// name index, a frame slot is `-(slot + 1)`. The slot case is what makes an
    /// array inside a procedure body *local* — its elements live in the frame,
    /// so two activations of a recursive procedure do not share them and the
    /// array goes away with the frame, which is what tclsh does.
    ///
    /// [`runtime::place_of`](crate::assoc::place_of) decodes it; the negative
    /// half cannot collide with a name index, which is unsigned.
    pub(crate) fn array_place(&mut self, name: &str) -> i64 {
        self.note_array(name);
        self.var_place_operand(name)
    }

    /// The same encoding, without recording the name as an array.
    ///
    /// `dict set` and the scalar guards need a variable's place to read it
    /// without refusing an unset one, and neither makes the name an array —
    /// noting it would make every other mention of it emit a guard.
    fn var_place_operand(&mut self, name: &str) -> i64 {
        match self.var_place(name) {
            Place::Global(idx) => i64::from(idx),
            Place::Slot(slot) => -i64::from(slot) - 1,
        }
    }

    /// Read a scalar variable. The guard is emitted only for names the script
    /// also uses as arrays, so an ordinary `$x` still lowers to a bare `GetVar`
    /// or `GetSlot`.
    ///
    /// A procedure local *can* hold an array, so the guard applies there too:
    /// `$a` on a local array is `can't read "a": variable is array` exactly as
    /// it is on a global one. The read itself goes through
    /// [`Compiler::emit_get_var`], which already picks the slot or the global.
    pub(crate) fn scalar_get(&mut self, name: &str) {
        if !self.is_array(name) {
            self.emit_get_var(name);
            return;
        }
        // The guard reads through the op's own place operand rather than
        // `emit_get_var`: a `GetVar` of a variable that was never set is
        // refused by strict-undef mode before the guard can answer, and the
        // guard owns two answers a bare read cannot give — `variable is array`,
        // and, for a name the script also uses as an array, the unset
        // diagnostic itself.
        let place = self.var_place_operand(name);
        self.push_str(name);
        self.emit(Op::LoadInt(place), 1);
        self.emit(Op::Extended(ext::SCALAR, 0), -1);
    }

    /// Refuse a scalar assignment to a variable that holds an array.
    pub(crate) fn scalar_set_guard(&mut self, name: &str) {
        if !self.is_array(name) {
            return;
        }
        // Same place operand, and for the same reason twice over: `set b 5`
        // emits this guard *before* the assignment, so a refusing read here
        // would refuse every first assignment to a name used as an array.
        let place = self.var_place_operand(name);
        self.push_str(name);
        self.emit(Op::LoadInt(place), 1);
        self.emit(Op::Extended(ext::SCALAR, 1), -2);
    }

    /// `$a(i)`.
    pub(crate) fn elem_get(&mut self, name: &str, index: &[Part]) -> Result<(), CompileError> {
        let place = self.array_place(name);
        self.push_str(name);
        self.index_value(index)?;
        self.emit(Op::LoadInt(place), 1);
        self.emit(Op::Extended(ext::ELEM_GET, 0), -2);
        Ok(())
    }

    /// `set a(i) v`, which yields the value it assigned.
    pub(crate) fn elem_set(
        &mut self,
        name: &str,
        index: &[Part],
        value: &Word,
    ) -> Result<(), CompileError> {
        let place = self.array_place(name);
        self.push_str(name);
        self.index_value(index)?;
        self.word(value)?;
        self.emit(Op::LoadInt(place), 1);
        self.emit(Op::Extended(ext::ELEM_SET, 0), -3);
        Ok(())
    }

    /// `incr a(i) ?by?`.
    pub(crate) fn elem_incr(
        &mut self,
        name: &str,
        index: &[Part],
        by: Option<&Word>,
    ) -> Result<(), CompileError> {
        let place = self.array_place(name);
        self.push_str(name);
        self.index_value(index)?;
        match by {
            Some(w) => self.word(w)?,
            None => {
                self.emit(Op::LoadInt(1), 1);
            }
        }
        self.emit(Op::LoadInt(place), 1);
        self.emit(Op::Extended(ext::ELEM_INCR, 0), -3);
        Ok(())
    }

    /// `unset ?-nocomplain? ?--? ?name ...?`.
    pub(crate) fn cmd_unset(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let mut i = 0;
        let mut complain = true;
        while let Some(text) = args.get(i).and_then(|w| w.as_literal()) {
            match text {
                "-nocomplain" => complain = false,
                "--" => {
                    i += 1;
                    break;
                }
                _ => break,
            }
            i += 1;
        }

        for word in &args[i..] {
            let Some(target) = target_of(word) else {
                return self.error("variable name must be a literal in this phase");
            };
            match target {
                Target::Scalar(name) => {
                    // A local is a frame slot rather than a global-table entry,
                    // and the op reaches either through the place it is handed.
                    let place = match self.var_place(&name) {
                        Place::Global(idx) => i64::from(idx),
                        Place::Slot(slot) => -i64::from(slot) - 1,
                    };
                    self.push_str(&name);
                    self.emit(Op::LoadInt(place), 1);
                    self.emit(Op::LoadInt(complain as i64), 1);
                    self.emit(Op::Extended(ext::UNSET_VAR, 0), -3);
                }
                Target::Elem { name, index } => {
                    let place = self.array_place(&name);
                    self.push_str(&name);
                    self.index_value(&index)?;
                    self.emit(Op::LoadInt(place), 1);
                    self.emit(Op::LoadInt(complain as i64), 1);
                    self.emit(Op::Extended(ext::UNSET_ELEM, 0), -4);
                }
            }
        }
        self.push_empty();
        Ok(())
    }

    /// `array subcommand arrayName ?arg ...?`.
    pub(crate) fn cmd_array(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let Some(sub) = args.first() else {
            return self.error("wrong # args: should be \"array subcommand ?arg ...?\"");
        };
        let sub = resolve(self.literal_of(sub, "array subcommand")?, ARRAY_SUBCOMMANDS).map_err(
            |msg| CompileError {
                msg,
                line: self.line,
            },
        )?;
        if !matches!(sub, "exists" | "get" | "names" | "set" | "size" | "unset") {
            return self.error(format!("array {sub} is not supported yet"));
        }

        let Some(array) = args.get(1) else {
            return self.error(format!(
                "wrong # args: should be \"array {sub} {}\"",
                array_usage(sub)
            ));
        };
        let name = self.literal_of(array, "array name")?.to_string();
        let slot = self.array_place(&name);
        let rest = &args[2..];

        match (sub, rest.len()) {
            ("exists", 0) => {
                self.emit(Op::LoadInt(slot), 1);
                self.emit(Op::Extended(ext::ARR_EXISTS, 0), 0);
            }
            ("size", 0) => {
                self.emit(Op::LoadInt(slot), 1);
                self.emit(Op::Extended(ext::ARR_SIZE, 0), 0);
            }
            ("set", 1) => {
                self.push_str(&name);
                self.word(&rest[0])?;
                self.emit(Op::LoadInt(slot), 1);
                // Which name a refusal quotes depends on where the *command*
                // sits, not on where the variable does: inside a procedure body
                // tclsh always names the variable, and at the top level it names
                // the first element it was about to write. Measured — a global
                // reached through `global` from inside a body takes the body's
                // wording, so the enclosing scope is what decides.
                let in_body = u8::from(self.scope.is_some());
                self.emit(Op::Extended(ext::ARR_SET, in_body), -2);
            }
            ("get", 0..=1) => {
                self.pattern_args(rest, "-glob")?;
                self.emit(Op::LoadInt(slot), 1);
                self.emit(Op::Extended(ext::ARR_GET, 0), -3);
            }
            ("unset", 0..=1) => {
                self.pattern_args(rest, "-glob")?;
                self.emit(Op::LoadInt(slot), 1);
                self.emit(Op::Extended(ext::ARR_UNSET, 0), -3);
            }
            ("names", 0..=2) => {
                let (mode, pattern) = match rest {
                    [] => ("-glob", rest),
                    [_] => ("-glob", rest),
                    [mode, _] => (self.literal_of(mode, "array names mode")?, &rest[1..]),
                    _ => unreachable!("arity checked by the match arm"),
                };
                let mode = mode.to_string();
                if !matches!(mode.as_str(), "-exact" | "-glob" | "-regexp") {
                    return self.error(format!(
                        "bad option \"{mode}\": must be -exact, -glob, or -regexp"
                    ));
                }
                if mode == "-regexp" {
                    return self
                        .error("array names -regexp needs regexp support, which is not built yet");
                }
                self.pattern_args(pattern, &mode)?;
                self.emit(Op::LoadInt(slot), 1);
                self.emit(Op::Extended(ext::ARR_NAMES, 0), -3);
            }
            _ => {
                return self.error(format!(
                    "wrong # args: should be \"array {sub} {}\"",
                    array_usage(sub)
                ))
            }
        }
        Ok(())
    }

    /// Push the `mode`, `pattern` and "was a pattern given" operands shared by
    /// the filtering `array` subcommands.
    fn pattern_args(&mut self, pattern: &[Word], mode: &str) -> Result<(), CompileError> {
        self.push_str(mode);
        match pattern {
            [w] => {
                self.word(w)?;
                self.emit(Op::LoadInt(1), 1);
            }
            _ => {
                self.push_empty();
                self.emit(Op::LoadInt(0), 1);
            }
        }
        Ok(())
    }

    /// `dict subcommand arg ?arg ...?`.
    pub(crate) fn cmd_dict(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let Some(sub) = args.first() else {
            return self.error("wrong # args: should be \"dict subcommand ?arg ...?\"");
        };
        let sub =
            resolve(self.literal_of(sub, "dict subcommand")?, DICT_SUBCOMMANDS).map_err(|msg| {
                CompileError {
                    msg,
                    line: self.line,
                }
            })?;
        let rest = &args[1..];

        match sub {
            "create" => {
                if !rest.len().is_multiple_of(2) {
                    return self.error("wrong # args: should be \"dict create ?key value ...?\"");
                }
                self.variadic(rest, ext::DICT_CREATE)
            }
            "get" => {
                if rest.is_empty() {
                    return self.error("wrong # args: should be \"dict get dictionary ?key ...?\"");
                }
                self.variadic(rest, ext::DICT_GET)
            }
            "exists" => {
                if rest.len() < 2 {
                    return self
                        .error("wrong # args: should be \"dict exists dictionary key ?key ...?\"");
                }
                self.variadic(rest, ext::DICT_EXISTS)
            }
            "remove" => {
                if rest.is_empty() {
                    return self
                        .error("wrong # args: should be \"dict remove dictionary ?key ...?\"");
                }
                self.variadic(rest, ext::DICT_REMOVE)
            }
            "merge" => self.variadic(rest, ext::DICT_MERGE),
            "keys" | "values" => {
                let op = if sub == "keys" {
                    ext::DICT_KEYS
                } else {
                    ext::DICT_VALUES
                };
                let (dict, pattern) = match rest {
                    [d] => (d, None),
                    [d, p] => (d, Some(p)),
                    _ => {
                        return self.error(format!(
                            "wrong # args: should be \"dict {sub} dictionary ?pattern?\""
                        ))
                    }
                };
                self.word(dict)?;
                self.pattern_args(pattern.map_or(&[], std::slice::from_ref), "-glob")?;
                self.emit(Op::Extended(op, 0), -3);
                Ok(())
            }
            "size" => {
                let [dict] = rest else {
                    return self.error("wrong # args: should be \"dict size dictionary\"");
                };
                self.word(dict)?;
                self.emit(Op::Extended(ext::DICT_SIZE, 0), 0);
                Ok(())
            }
            "set" => {
                let [name, keys @ .., value] = rest else {
                    return self.error(
                        "wrong # args: should be \"dict set dictVarName key ?key ...? value\"",
                    );
                };
                if keys.is_empty() {
                    return self.error(
                        "wrong # args: should be \"dict set dictVarName key ?key ...? value\"",
                    );
                }
                let Some(Target::Scalar(name)) = target_of(name) else {
                    return self.error("dict set into an array element is not supported yet");
                };
                self.push_str(&name);
                // `dict set` creates the variable when it does not exist, so
                // its read of the current value must tolerate absence where a
                // bare `$d` refuses it. The place operand reads without
                // refusing, and reaches a frame slot as well as a global.
                let place = self.var_place_operand(&name);
                self.emit(Op::LoadInt(place), 1);
                for key in keys {
                    self.word(key)?;
                }
                self.word(value)?;
                // The count covers the keys and the value; the variable name
                // and its current value stay below them.
                self.emit(Op::LoadInt(keys.len() as i64 + 1), 1);
                self.emit(Op::Extended(ext::DICT_SET, 0), -(keys.len() as i32 + 3));
                self.emit(Op::Dup, 1);
                self.emit_set_var(&name);
                Ok(())
            }
            "for" => {
                let [vars, dict, body] = rest else {
                    return self
                        .error("wrong # args: should be \"dict for {keyVarName valueVarName} dictionary script\"");
                };
                self.dict_for(vars, dict, body)
            }
            other => self.error(format!("dict {other} is not supported yet")),
        }
    }

    /// Emit `n` argument words followed by their count, then the op that reads
    /// them. Every variadic `dict` subcommand takes its operands this way.
    fn variadic(&mut self, args: &[Word], op: u16) -> Result<(), CompileError> {
        for w in args {
            self.word(w)?;
        }
        self.emit(Op::LoadInt(args.len() as i64), 1);
        self.emit(Op::Extended(op, 0), -(args.len() as i32));
        Ok(())
    }

    /// `dict for {k v} $d {body}` — a cursor over the key/value pairs, which the
    /// VM's own `ArrayLen`/`ArrayGet` walk.
    fn dict_for(&mut self, vars: &Word, dict: &Word, body: &Word) -> Result<(), CompileError> {
        let text = self.literal_of(vars, "dict for variable list")?;
        let names = split(text, "list").map_err(|msg| CompileError {
            msg,
            line: self.line,
        })?;
        let [key_name, value_name] = names.as_slice() else {
            return self.error("must have exactly two variable names");
        };
        let (key_name, value_name) = (key_name.clone(), value_name.clone());

        // Hidden globals, named so that no Tcl variable name can collide.
        let tag = self.b.current_pos();
        let pairs = self.b.add_name(&format!("\u{0}dict for pairs {tag}"));
        let cursor = self.b.add_name(&format!("\u{0}dict for cursor {tag}"));

        self.word(dict)?;
        self.emit(Op::Extended(ext::DICT_PAIRS, 0), 0);
        self.emit(Op::SetVar(pairs), -1);
        self.emit(Op::LoadInt(0), 1);
        self.emit(Op::SetVar(cursor), -1);

        let script = self.body_of(body)?;
        self.rotated_loop(
            |c| {
                c.scalar_set_guard(&key_name);
                c.emit(Op::GetVar(cursor), 1);
                c.emit(Op::ArrayGet(pairs), 0);
                c.emit_set_var(&key_name);

                c.scalar_set_guard(&value_name);
                c.emit(Op::GetVar(cursor), 1);
                c.emit(Op::LoadInt(1), 1);
                c.emit(Op::Add, -1);
                c.emit(Op::ArrayGet(pairs), 0);
                c.emit_set_var(&value_name);

                c.emit_body(&script)
            },
            |c| {
                c.emit(Op::GetVar(cursor), 1);
                c.emit(Op::LoadInt(2), 1);
                c.emit(Op::Add, -1);
                c.emit(Op::SetVar(cursor), -1);
                Ok(())
            },
            |c| {
                c.emit(Op::GetVar(cursor), 1);
                c.emit(Op::ArrayLen(pairs), 1);
                c.emit(Op::NumLt, -1);
                Ok(())
            },
        )?;
        // `dict for` has no value of its own.
        self.push_empty();
        Ok(())
    }
}

pub(crate) const ARRAY_SUBCOMMANDS: &[&str] = &[
    "anymore",
    "default",
    "donesearch",
    "exists",
    "for",
    "get",
    "names",
    "nextelement",
    "set",
    "size",
    "startsearch",
    "statistics",
    "unset",
];

pub(crate) const DICT_SUBCOMMANDS: &[&str] = &[
    "append",
    "create",
    "exists",
    "filter",
    "for",
    "get",
    "getdef",
    "getwithdefault",
    "incr",
    "info",
    "keys",
    "lappend",
    "map",
    "merge",
    "remove",
    "replace",
    "set",
    "size",
    "unset",
    "update",
    "values",
    "with",
];

/// Resolve a possibly abbreviated subcommand, as the ensemble machinery does.
fn resolve(word: &str, table: &'static [&'static str]) -> Result<&'static str, String> {
    if let Some(exact) = table.iter().copied().find(|s| *s == word) {
        return Ok(exact);
    }
    let mut hits = table.iter().copied().filter(|s| s.starts_with(word));
    match (hits.next(), hits.next()) {
        (Some(only), None) => Ok(only),
        _ => Err(format!(
            "unknown or ambiguous subcommand \"{word}\": must be {}, or {}",
            table[..table.len() - 1].join(", "),
            table[table.len() - 1]
        )),
    }
}

fn array_usage(sub: &str) -> &'static str {
    match sub {
        "exists" => "arrayName",
        "get" => "arrayName ?pattern?",
        "names" => "arrayName ?mode? ?pattern?",
        "set" => "arrayName list",
        "size" => "arrayName",
        _ => "arrayName ?pattern?",
    }
}

// ─── runtime ──────────────────────────────────────────────────────────────

/// The associative extension ops. Operands are popped in reverse of the order
/// the compiler pushed them; every one of them is documented at its `ext`
/// constant.
pub(crate) fn extension(vm: &mut VM, id: u16, arg: u8) -> Result<(), String> {
    match id {
        ext::ELEM_GET => {
            let place = place_of(vm);
            let index = pop_str(vm);
            let name = pop_str(vm);
            let value = match peek(vm, place) {
                Some(Value::Hash(map)) => map.get(&index).cloned().ok_or_else(|| {
                    format!("can't read \"{name}({index})\": no such element in array")
                })?,
                Some(Value::Undef) | None => {
                    return Err(format!("can't read \"{name}({index})\": no such variable"))
                }
                Some(_) => {
                    return Err(format!(
                        "can't read \"{name}({index})\": variable isn't array"
                    ))
                }
            };
            vm.push(value);
            Ok(())
        }
        ext::ELEM_SET => {
            let place = place_of(vm);
            let value = vm.pop();
            let index = pop_str(vm);
            let name = pop_str(vm);
            element_map(vm, place)
                .ok_or_else(|| format!("can't set \"{name}({index})\": variable isn't array"))?
                .insert(index, value.clone());
            vm.push(value);
            Ok(())
        }
        ext::ELEM_INCR => {
            let place = place_of(vm);
            let by = tcl_int(&vm.pop())?;
            let index = pop_str(vm);
            let name = pop_str(vm);
            let map = element_map(vm, place)
                .ok_or_else(|| format!("can't set \"{name}({index})\": variable isn't array"))?;
            // A missing element counts as zero, as a missing scalar does.
            let current = match map.get(&index) {
                Some(v) => tcl_int(v)?,
                None => 0,
            };
            let next = current
                .checked_add(by)
                .ok_or("integer value too large to represent")?;
            map.insert(index, Value::Int(next));
            vm.push(Value::Int(next));
            Ok(())
        }
        ext::UNSET_ELEM => {
            let complain = pop_int(vm) != 0;
            let place = place_of(vm);
            let index = pop_str(vm);
            let name = pop_str(vm);
            let missing = match crate::runtime::var_cell(vm, place) {
                Some(Value::Hash(map)) => map.remove(&index).is_none(),
                Some(Value::Undef) | None => true,
                Some(_) => {
                    return if complain {
                        Err(format!(
                            "can't unset \"{name}({index})\": variable isn't array"
                        ))
                    } else {
                        Ok(())
                    }
                }
            };
            if missing && complain {
                return Err(format!(
                    "can't unset \"{name}({index})\": no such element in array"
                ));
            }
            Ok(())
        }
        ext::UNSET_VAR => {
            let complain = pop_int(vm) != 0;
            let place = place_of(vm);
            let name = pop_str(vm);
            match crate::runtime::var_cell(vm, place) {
                Some(v) if *v != Value::Undef => {
                    *v = Value::Undef;
                    Ok(())
                }
                _ if complain => Err(format!("can't unset \"{name}\": no such variable")),
                _ => Ok(()),
            }
        }
        ext::SCALAR => {
            let place = place_of(vm);
            let name = pop_str(vm);
            let value = peek(vm, place).cloned().unwrap_or(Value::Undef);
            if matches!(value, Value::Hash(_)) {
                let verb = if arg == 1 { "set" } else { "read" };
                return Err(format!("can't {verb} \"{name}\": variable is array"));
            }
            if arg != 1 {
                // The read half owns the unset diagnostic for these names,
                // because the guard replaced the `GetVar` that would have
                // raised it. `arg == 1` is the assignment guard, where an unset
                // variable is exactly what is about to be created.
                if matches!(value, Value::Undef) {
                    return Err(format!("can't read \"{name}\": no such variable"));
                }
                vm.push(value);
            }
            Ok(())
        }

        ext::ARR_EXISTS => {
            let place = place_of(vm);
            let exists = matches!(peek(vm, place), Some(Value::Hash(_)));
            vm.push(Value::Int(exists as i64));
            Ok(())
        }
        ext::ARR_SIZE => {
            let place = place_of(vm);
            let size = match peek(vm, place) {
                Some(Value::Hash(map)) => map.len() as i64,
                _ => 0,
            };
            vm.push(Value::Int(size));
            Ok(())
        }
        ext::ARR_NAMES => {
            let place = place_of(vm);
            let filter = pop_filter(vm);
            let mut names = selected(vm, place, &filter);
            names.sort();
            vm.push(Value::Str(Arc::new(join(&names))));
            Ok(())
        }
        ext::ARR_GET => {
            let place = place_of(vm);
            let filter = pop_filter(vm);
            let mut names = selected(vm, place, &filter);
            names.sort();
            let mut flat = Vec::with_capacity(names.len() * 2);
            for name in names {
                let value = match peek(vm, place) {
                    Some(Value::Hash(map)) => map.get(&name).map(to_tcl_string).unwrap_or_default(),
                    _ => String::new(),
                };
                flat.push(name);
                flat.push(value);
            }
            vm.push(Value::Str(Arc::new(join(&flat))));
            Ok(())
        }
        ext::ARR_UNSET => {
            let place = place_of(vm);
            let filter = pop_filter(vm);
            match filter {
                // No pattern: the whole variable goes.
                None => {
                    if let Some(v @ Value::Hash(_)) = crate::runtime::var_cell(vm, place) {
                        *v = Value::Undef;
                    }
                }
                Some(_) => {
                    let doomed = selected(vm, place, &filter);
                    if let Some(Value::Hash(map)) = crate::runtime::var_cell(vm, place) {
                        for name in doomed {
                            map.remove(&name);
                        }
                    }
                }
            }
            vm.push(Value::Str(Arc::new(String::new())));
            Ok(())
        }
        ext::ARR_SET => {
            let place = place_of(vm);
            let list = pop_str(vm);
            let name = pop_str(vm);
            let elements = split(&list, "list")?;
            if elements.len() % 2 != 0 {
                return Err("list must have an even number of elements".to_string());
            }
            // An existing scalar is refused, and the diagnostic names the first
            // element being written — or the variable itself when there is none.
            if !matches!(
                peek(vm, place),
                Some(Value::Hash(_)) | Some(Value::Undef) | None
            ) {
                // `arg` is 1 when the command was compiled inside a procedure
                // body, where tclsh names the variable however many elements
                // were given; at the top level it names the first of them.
                return Err(match elements.first().filter(|_| arg != 1) {
                    Some(key) => format!("can't set \"{name}({key})\": variable isn't array"),
                    None => format!("can't array set \"{name}\": variable isn't array"),
                });
            }
            let map = element_map(vm, place).expect("scalar case refused above");
            let mut it = elements.into_iter();
            while let (Some(k), Some(v)) = (it.next(), it.next()) {
                map.insert(k, Value::Str(Arc::new(v)));
            }
            vm.push(Value::Str(Arc::new(String::new())));
            Ok(())
        }

        ext::DICT_CREATE => {
            let args = pop_args(vm);
            let mut d = Dict::new();
            let mut it = args.into_iter();
            while let (Some(k), Some(v)) = (it.next(), it.next()) {
                d.put(k, v);
            }
            push_str(vm, d.to_list());
            Ok(())
        }
        ext::DICT_GET => {
            let mut args = pop_args(vm);
            let dict = args.remove(0);
            if args.is_empty() {
                // No keys: the pairs as a list, which is the canonical form.
                push_str(vm, Dict::parse(&dict)?.to_list());
                return Ok(());
            }
            let last = args.pop().expect("at least one key");
            let inner = trace_path(&dict, &args)?;
            let value = inner
                .get(&last)
                .ok_or_else(|| format!("key \"{last}\" not known in dictionary"))?;
            push_str(vm, value.to_string());
            Ok(())
        }
        ext::DICT_EXISTS => {
            let mut args = pop_args(vm);
            let dict = args.remove(0);
            let last = args.pop().expect("arity checked at compile time");
            // Unlike `dict get`, a malformed dict along the path is simply a
            // miss rather than an error.
            let found = match trace_path(&dict, &args) {
                Ok(inner) => inner.get(&last).is_some(),
                Err(_) => false,
            };
            vm.push(Value::Int(found as i64));
            Ok(())
        }
        ext::DICT_REMOVE => {
            let mut args = pop_args(vm);
            let dict = args.remove(0);
            let mut d = Dict::parse(&dict)?;
            for key in &args {
                d.remove(key);
            }
            push_str(vm, d.to_list());
            Ok(())
        }
        ext::DICT_MERGE => {
            let args = pop_args(vm);
            let Some((first, rest)) = args.split_first() else {
                push_str(vm, String::new());
                return Ok(());
            };
            let mut merged = Dict::parse(first)?;
            let mut touched = false;
            for other in rest {
                for (k, v) in Dict::parse(other)?.entries {
                    merged.put(k, v);
                    touched = true;
                }
            }
            // The reference implementation returns the first argument itself
            // when nothing was merged into it, so its spelling survives.
            push_str(
                vm,
                if touched {
                    merged.to_list()
                } else {
                    first.clone()
                },
            );
            Ok(())
        }
        ext::DICT_KEYS | ext::DICT_VALUES => {
            let filter = pop_filter(vm);
            let dict = pop_str(vm);
            let d = Dict::parse(&dict)?;
            let chosen: Vec<String> = d
                .entries
                .iter()
                .map(|(k, v)| if id == ext::DICT_KEYS { k } else { v })
                .filter(|s| match &filter {
                    Some((_, pattern)) => string_match(s, pattern),
                    None => true,
                })
                .cloned()
                .collect();
            push_str(vm, join(&chosen));
            Ok(())
        }
        ext::DICT_SIZE => {
            let dict = pop_str(vm);
            let size = Dict::parse(&dict)?.len() as i64;
            vm.push(Value::Int(size));
            Ok(())
        }
        ext::DICT_SET => {
            let mut args = pop_args(vm);
            let value = args.pop().expect("value operand");
            let keys = args;
            let place = place_of(vm);
            let current = peek(vm, place).cloned().unwrap_or(Value::Undef);
            let name = pop_str(vm);
            if matches!(current, Value::Hash(_)) {
                return Err(format!("can't set \"{name}\": variable is array"));
            }
            push_str(vm, dict_set(&to_tcl_string(&current), &keys, value)?);
            Ok(())
        }
        ext::DICT_PAIRS => {
            let dict = pop_str(vm);
            let d = Dict::parse(&dict)?;
            let mut flat = Vec::with_capacity(d.len() * 2);
            for (k, v) in d.entries {
                flat.push(Value::Str(Arc::new(k)));
                flat.push(Value::Str(Arc::new(v)));
            }
            vm.push(Value::Array(flat));
            Ok(())
        }
        other => Err(format!("unknown extension op {other}")),
    }
}

/// `dict set` down a key path, creating the intermediate dicts it needs.
fn dict_set(dict: &str, keys: &[String], value: String) -> Result<String, String> {
    let mut d = Dict::parse(dict)?;
    let (key, rest) = keys.split_first().expect("at least one key");
    if rest.is_empty() {
        d.put(key.clone(), value);
    } else {
        let inner = d.get(key).unwrap_or("").to_string();
        d.put(key.clone(), dict_set(&inner, rest, value)?);
    }
    Ok(d.to_list())
}

/// The element map of an array variable, creating it when the variable does not
/// exist yet. `None` when the variable holds a scalar.
fn element_map(vm: &mut VM, place: Place) -> Option<&mut HashMap<String, Value>> {
    let cell = crate::runtime::var_cell(vm, place)?;
    if *cell == Value::Undef {
        *cell = Value::Hash(HashMap::new());
    }
    match cell {
        Value::Hash(map) => Some(map),
        _ => None,
    }
}

/// The element names of an array that pass the filter.
fn selected(vm: &VM, place: Place, filter: &Option<(String, String)>) -> Vec<String> {
    let Some(Value::Hash(map)) = peek(vm, place) else {
        return Vec::new();
    };
    map.keys()
        .filter(|k| match filter {
            Some((mode, pattern)) if mode == "-exact" => k.as_str() == pattern,
            Some((_, pattern)) => string_match(k, pattern),
            None => true,
        })
        .cloned()
        .collect()
}

/// Pop the `mode`, `pattern`, "was a pattern given" triple.
fn pop_filter(vm: &mut VM) -> Option<(String, String)> {
    let given = vm.pop().is_truthy();
    let pattern = pop_str(vm);
    let mode = pop_str(vm);
    given.then_some((mode, pattern))
}

/// Pop a count and then that many string operands, restoring their order.
/// Anything the compiler pushed below them stays on the stack.
fn pop_args(vm: &mut VM) -> Vec<String> {
    let n = pop_int(vm).max(0) as usize;
    let mut args: Vec<String> = (0..n).map(|_| pop_str(vm)).collect();
    args.reverse();
    args
}

/// Pop a counted operand the compiler emitted as `LoadInt`.
fn pop_int(vm: &mut VM) -> i64 {
    match vm.pop() {
        Value::Int(i) => i,
        other => to_tcl_string(&other).parse().unwrap_or(0),
    }
}

/// Decode the operand [`Compiler::array_place`] pushed: a name index in the
/// VM's global table, or a frame slot written as `-(slot + 1)`.
pub(crate) fn place_of(vm: &mut VM) -> Place {
    let raw = pop_int(vm);
    if raw < 0 {
        Place::Slot((-raw - 1) as u16)
    } else {
        Place::Global(raw as u16)
    }
}

/// What the variable holds, without creating it.
///
/// Separate from [`crate::runtime::var_cell`], which grows the storage to reach
/// the place: a *read* of a variable that was never set must not allocate one,
/// and `array exists` on an unset name must stay false rather than becoming a
/// variable by being asked about.
pub(crate) fn peek(vm: &VM, place: Place) -> Option<&Value> {
    match place {
        Place::Global(idx) => vm.globals.get(idx as usize),
        Place::Slot(slot) => vm.frames.last().and_then(|f| f.slots.get(slot as usize)),
    }
}

/// Whether `name(index)` is set, for `info exists`.
///
/// Every way of *not* being set is the same answer here — the variable does not
/// exist, it exists and is not an array, or it is an array with no such element
/// — which is why this cannot go through [`ext::ELEM_GET`], whose job is to tell
/// those three apart in the diagnostic.
pub(crate) fn element_is_set(vm: &VM, place: Place, index: &str) -> bool {
    matches!(peek(vm, place), Some(Value::Hash(map)) if map.contains_key(index))
}

fn pop_str(vm: &mut VM) -> String {
    to_tcl_string(&vm.pop())
}

fn push_str(vm: &mut VM, text: String) {
    vm.push(Value::Str(Arc::new(text)));
}

/// Names that were used as arrays anywhere in a script, collected on the
/// compiler's first pass so the second can guard their scalar uses.
pub(crate) type ArrayNames = HashSet<String>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_elements_round_trip_through_quoting() {
        for text in [
            "", "a", "a b", "a{b", "a}b", "a\\", "a\\ b", "{a}", "\"a", "a\"b", "]", "[", "$", "#",
            "a#b", "a\nb", "a\tb", "{a{b}c}", "a(b)",
        ] {
            let joined = join(&[text]);
            let back = split(&joined, "list").expect("valid list");
            assert_eq!(back, vec![text.to_string()], "round trip of {text:?}");
        }
    }

    #[test]
    fn glob_follows_string_match_rules() {
        assert!(string_match("abc", "a*"));
        assert!(string_match("abc", "a?c"));
        assert!(string_match("abc", "a[bc]c"));
        assert!(string_match("a*b", "a\\*b"));
        assert!(!string_match("axb", "a\\*b"));
        assert!(!string_match("abc", "a?"));
        assert!(string_match("", "*"));
        assert!(string_match("abc", "[a-c][a-c][a-c]"));
        assert!(string_match("abc", "[c-a]bc"));
    }

    #[test]
    fn dict_keeps_insertion_order_and_updates_in_place() {
        let d = Dict::parse("b 1 a 2 b 3").expect("parses");
        assert_eq!(d.to_list(), "b 3 a 2");
        assert_eq!(d.len(), 2);
    }
}
