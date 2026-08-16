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

use crate::cmd_scope::Link;
use crate::compiler::{ext, CompileError, Compiler, Place};
use crate::parser::{Part, Word};
use crate::runtime::{tcl_int, to_tcl_string, Shared, TclError};

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

    /// Where an array variable lives, as a [`Place`], recording the name as an
    /// array the way [`Compiler::array_place`] does. For the ops that want the
    /// place's two halves separately rather than as one integer.
    pub(crate) fn array_place_of(&mut self, name: &str) -> Place {
        self.note_array(name);
        self.var_place(name)
    }

    /// The same encoding, without recording the name as an array.
    ///
    /// `dict set` and the scalar guards need a variable's place to read it
    /// without refusing an unset one, and neither makes the name an array —
    /// noting it would make every other mention of it emit a guard.
    pub(crate) fn var_place_operand(&mut self, name: &str) -> i64 {
        self.var_place(name).encode()
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
        self.emit_elem_get(name, index, false)
    }

    /// The same read, answering with the empty string where `$a(i)` would refuse.
    ///
    /// `lappend a(i) x` and `append a(i) x` on an element that does not exist yet
    /// create it, exactly as they do for a scalar, so the read they are built out
    /// of has to tolerate absence. A variable that exists and is *not* an array
    /// is still refused: that is a different answer, and tclsh gives it.
    pub(crate) fn elem_get_tolerant(
        &mut self,
        name: &str,
        index: &[Part],
    ) -> Result<(), CompileError> {
        self.emit_elem_get(name, index, true)
    }

    fn emit_elem_get(
        &mut self,
        name: &str,
        index: &[Part],
        tolerant: bool,
    ) -> Result<(), CompileError> {
        let place = self.array_place(name);
        self.push_str(name);
        self.index_value(index)?;
        self.emit(Op::LoadInt(place), 1);
        self.emit(Op::Extended(ext::ELEM_GET, u8::from(tolerant)), -2);
        Ok(())
    }

    /// Store the value already on the stack into `a(i)`, leaving what was
    /// stored — the same result `set a(i) v` yields.
    ///
    /// The operands [`ext::ELEM_SET`] wants sit *under* the value, and the value
    /// is already on top, so each is pushed and swapped into place. That is what
    /// lets an element be the variable of a command whose value is computed
    /// before the variable is known — `lappend a(i) x`, `lassign … a(i)`, a
    /// `foreach` variable — without a second op that only differs in operand
    /// order.
    pub(crate) fn elem_store(&mut self, name: &str, index: &[Part]) -> Result<(), CompileError> {
        let place = self.array_place(name);
        self.push_str(name);
        self.emit(Op::Swap, 0);
        self.index_value(index)?;
        self.emit(Op::Swap, 0);
        self.emit(Op::LoadInt(place), 1);
        self.emit(Op::Extended(ext::ELEM_SET, 0), -3);
        Ok(())
    }

    /// Store the value on the stack into whatever `text` names — a scalar or an
    /// array element — leaving the stack as it was found.
    ///
    /// The commands that assign to a *list* of variables written as one word
    /// (`foreach`, `lmap`, `lassign`) reach an element through here: the name
    /// arrives as text, and `a(i)` is an element there exactly as it is anywhere
    /// else a variable name is written.
    pub(crate) fn store_named(&mut self, text: &str) -> Result<(), CompileError> {
        match target_of(&Word {
            parts: vec![Part::Lit(text.to_string())],
            ..Word::default()
        }) {
            Some(Target::Elem { name, index }) => {
                self.elem_store(&name, &index)?;
                self.emit(Op::Pop, -1);
                Ok(())
            }
            _ => {
                self.emit_set_var(text);
                Ok(())
            }
        }
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
                // `unset $n` resolves its variable when it runs. An `a(i)`
                // spelling the name happens to carry is an element there too,
                // which is what the op's own split makes of it.
                self.dyn_unset(word, complain)?;
                continue;
            };
            match target {
                Target::Scalar(name) => {
                    // A local is a frame slot rather than a global-table entry,
                    // and the op reaches either through the place it is handed.
                    let place = self.var_place(&name).encode();
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
                    // `Tcl_GetIndexFromObj` is reached from the command's own
                    // implementation, so tclsh reports a bad option when the
                    // command runs: `if {0} {array names a -bogus x}` costs a
                    // script nothing there and `catch` answers 1. Deferrable for
                    // the same reason `wrong # args` is — nothing has been
                    // emitted yet, so the refusal becomes code in the branch.
                    return Err(self.deferrable_err(format!(
                        "bad option \"{mode}\": must be -exact, -glob, or -regexp"
                    )));
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
            "map" => {
                let [vars, dict, body] = rest else {
                    return self
                        .error("wrong # args: should be \"dict map {keyVarName valueVarName} dictionary script\"");
                };
                self.dict_map(vars, dict, body)
            }
            "update" => {
                // `DictUpdateCmd` wants `objc >= 5` and an odd `objc`
                // (`generic/tclDictObj.c:3500`); counted from here that is a
                // variable, at least one key/variable pair, and a script.
                let usage = "wrong # args: should be \"dict update dictVarName key varName ?key varName ...? script\"";
                let [name, pairs @ .., body] = rest else {
                    return self.error(usage);
                };
                if pairs.is_empty() || !pairs.len().is_multiple_of(2) {
                    return self.error(usage);
                }
                let (name, body) = (name.clone(), body.clone());
                let pairs = pairs.to_vec();
                self.dict_update(&name, &pairs, &body)
            }
            "with" => {
                // `DictWithCmd` wants `objc >= 3` (`generic/tclDictObj.c:3658`);
                // counted from here that is a variable, any number of path keys,
                // and a script.
                let [name, path @ .., body] = rest else {
                    return self.error(
                        "wrong # args: should be \"dict with dictVarName ?key ...? script\"",
                    );
                };
                let (name, body) = (name.clone(), body.clone());
                let path = path.to_vec();
                self.dict_with(&name, &path, &body)
            }
            "incr" => {
                let (name, key, by) = match rest {
                    [name, key] => (name, key, None),
                    [name, key, by] => (name, key, Some(by)),
                    _ => {
                        return self.error(
                            "wrong # args: should be \"dict incr dictVarName key ?increment?\"",
                        )
                    }
                };
                let Some(Target::Scalar(name)) = target_of(name) else {
                    return self.error("dict incr into an array element is not supported yet");
                };
                self.push_str(&name);
                // Same reach as `dict set`: the place operand reads the current
                // value without refusing an unset variable, and finds a frame
                // slot as readily as a global.
                let place = self.var_place_operand(&name);
                self.emit(Op::LoadInt(place), 1);
                self.word(key)?;
                match by {
                    Some(by) => self.word(by)?,
                    // `dict incr d k` is `dict incr d k 1`.
                    None => self.push_str("1"),
                }
                self.emit(Op::Extended(ext::DICT_INCR, 0), -3);
                self.emit(Op::Dup, 1);
                self.emit_set_var(&name);
                Ok(())
            }
            "replace" => {
                // The pairs are loose arguments, so an odd count is the usage
                // error rather than "missing value to go with key".
                if rest.is_empty() || !(rest.len() - 1).is_multiple_of(2) {
                    return self.error(
                        "wrong # args: should be \"dict replace dictionary ?key value ...?\"",
                    );
                }
                self.variadic(rest, ext::DICT_REPLACE)
            }
            "getdef" | "getwithdefault" => {
                if rest.len() < 3 {
                    return self.error(format!(
                        "wrong # args: should be \"dict {sub} dictionary ?key ...? key default\""
                    ));
                }
                self.variadic(rest, ext::DICT_GETDEF)
            }
            "unset" => {
                let [name, keys @ ..] = rest else {
                    return self
                        .error("wrong # args: should be \"dict unset dictVarName key ?key ...?\"");
                };
                if keys.is_empty() {
                    return self
                        .error("wrong # args: should be \"dict unset dictVarName key ?key ...?\"");
                }
                self.dict_in_place(name, keys, ext::DICT_UNSET, "dict unset")
            }
            "lappend" | "append" => {
                let [name, key, values @ ..] = rest else {
                    return self.error(format!(
                        "wrong # args: should be \"dict {sub} dictVarName key ?value ...?\""
                    ));
                };
                let op = if sub == "lappend" {
                    ext::DICT_LAPPEND
                } else {
                    ext::DICT_APPEND
                };
                let mut operands = Vec::with_capacity(values.len() + 1);
                operands.push(key.clone());
                operands.extend_from_slice(values);
                self.dict_in_place(name, &operands, op, &format!("dict {sub}"))
            }
            "filter" => {
                let [dict, which, patterns @ ..] = rest else {
                    return self.error(
                        "wrong # args: should be \"dict filter dictionary filterType ?arg ...?\"",
                    );
                };
                // Which filter it is decides how many arguments are legal, so
                // the type has to be a literal here; `script` needs a body run
                // per pair, which this frontend does not lower yet.
                let which = match self.literal_of(which, "dict filter type")? {
                    "key" => 0,
                    "value" => 1,
                    "script" => {
                        let [vars, body] = patterns else {
                            return self.error(
                                "wrong # args: should be \"dict filter dictionary script {keyVarName valueVarName} filterScript\"",
                            );
                        };
                        let (vars, body) = (vars.clone(), body.clone());
                        let dict = dict.clone();
                        return self.dict_filter_script(&dict, &vars, &body);
                    }
                    // The reference implementation only reaches this when the
                    // command runs, so `catch {dict filter {a 1} bogus}` is a
                    // caught error there rather than a script that refuses to
                    // compile. Marked so the failure becomes code.
                    other => {
                        let msg =
                            format!("bad filterType \"{other}\": must be key, script, or value");
                        return Err(self.deferrable_err(msg));
                    }
                };
                self.word(dict)?;
                self.push_str(&which.to_string());
                for p in patterns {
                    self.word(p)?;
                }
                // The count covers the dict and the filter type as well, since
                // the handler pops all of its operands through it.
                self.emit(Op::LoadInt(patterns.len() as i64 + 2), 1);
                self.emit(
                    Op::Extended(ext::DICT_FILTER, 0),
                    -(patterns.len() as i32 + 2),
                );
                Ok(())
            }
            other => self.error(format!("dict {other} is not supported yet")),
        }
    }

    /// The shape every `dict` subcommand that *updates a variable* shares:
    /// push the name, push where it lives, push the operands and their count,
    /// run the op, and store what it produced back into the variable. The
    /// variable is reached by place rather than by value because these
    /// subcommands create it when it does not exist, so the read must tolerate
    /// an absent variable where a bare `$d` refuses one.
    fn dict_in_place(
        &mut self,
        name: &Word,
        operands: &[Word],
        op: u16,
        what: &str,
    ) -> Result<(), CompileError> {
        let Some(Target::Scalar(name)) = target_of(name) else {
            return self.error(format!("{what} into an array element is not supported yet"));
        };
        self.push_str(&name);
        let place = self.var_place_operand(&name);
        self.emit(Op::LoadInt(place), 1);
        for w in operands {
            self.word(w)?;
        }
        self.emit(Op::LoadInt(operands.len() as i64), 1);
        self.emit(Op::Extended(op, 0), -(operands.len() as i32 + 2));
        self.emit(Op::Dup, 1);
        self.emit_set_var(&name);
        Ok(())
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

    /// `dict for {k v} $d {body}` — a walk over the key/value pairs.
    fn dict_for(&mut self, vars: &Word, dict: &Word, body: &Word) -> Result<(), CompileError> {
        let names = self.dict_each_vars("dict for", vars)?;
        self.dict_each_init(dict)?;
        let script = self.body_of(body)?;
        self.dict_each(&names, |c| c.emit_body(&script))?;
        // `dict for` has no value of its own.
        self.dict_each_step(Step::Discard, 0);
        Ok(())
    }

    /// `dict update d k v ?k v …? {body}` — the pairs become variables for the
    /// body, and go back into the dictionary however the body ended.
    ///
    /// The write-back is a `finally`, not an ending: `DictUpdateCmd` evaluates
    /// the body with `FinalizeDictUpdate` already pushed as its NRE callback
    /// (`generic/tclDictObj.c:3539`), so an error, a `break`, a `return` and an
    /// ordinary result all reach it and all restore the body's own outcome
    /// afterwards. [`Compiler::finally_region`] is that shape; this supplies the
    /// two halves it runs.
    ///
    /// The variable names are literals here. tclsh reads them off the
    /// substituted words, so `dict update d a $vn {…}` is ordinary there and is
    /// refused here — the same wall every computed variable name meets in this
    /// frontend, and the one `set $name 1` meets.
    fn dict_update(
        &mut self,
        name: &Word,
        pairs: &[Word],
        body: &Word,
    ) -> Result<(), CompileError> {
        let Some(Target::Scalar(dict_name)) = target_of(name) else {
            return self.error("dict update on an array element is not supported yet");
        };
        let script = self.body_of(body)?;
        self.push_str(&dict_name);
        // By place, not by value: the write-back has to reach the same variable
        // the binding read, and a place reaches a frame slot as readily as a
        // global.
        let place = self.var_place_operand(&dict_name);
        self.emit(Op::LoadInt(place), 1);
        for pair in pairs.chunks(2) {
            let [key, var] = pair else {
                unreachable!("the pair count was checked by the caller");
            };
            self.word(key)?;
            let var_name = self.var_name_of(var)?;
            // The name rides too: the binding refuses an array in tclsh's own
            // wording, `can't set "x": variable is array` (measured).
            self.push_str(&var_name);
            let var_place = self.var_place_operand(&var_name);
            self.emit(Op::LoadInt(var_place), 1);
        }
        let count = pairs.len() / 2;
        self.emit(Op::LoadInt(count as i64), 1);
        self.emit(
            Op::Extended(ext::DICT_UPDATE_BIND, 0),
            -(3 * count as i32 + 2),
        );
        self.finally_region(ext::DICT_UPDATE_END, |c| c.emit_body_value(&script))
    }

    /// `dict with d ?key …? {body}` — every key of the dictionary becomes a
    /// variable for the body, and every one of them goes back on the way out.
    ///
    /// The same `finally` shape as `dict update`, for the same reason:
    /// `DictWithCmd` evaluates the body with `FinalizeDictWith` already pushed as
    /// its NRE callback (`generic/tclDictObj.c:3689`), so an error, a `break`, a
    /// `continue`, a `return` and an ordinary result all reach the write-back and
    /// all keep the body's own outcome afterwards.
    ///
    /// What `dict update` does not have is the *binding*. Its variable names are
    /// words of the command and are known while the script is read; these are the
    /// dictionary's keys, so nothing about them is known until it runs. That is
    /// resolved where it can be — [`crate::cmd_scope::dict_with_home`], the same
    /// resolution a computed `upvar` target gets. A key whose name a *procedure*
    /// body never mentions has no slot the compiler could have assigned, so it
    /// is given one at run time
    /// ([`crate::cmd_scope::runtime_slot_alloc`]) and is a local of that
    /// activation like any other: a nested script assigns it, `info locals`
    /// lists it, a second `dict with` over the same key finds the same one, and
    /// it dies with the frame. Refusing such a key was never an option — a
    /// record with a field the body does not read is ordinary code — and
    /// carrying its value in the command's record instead was right only for a
    /// body that leaves it alone.
    fn dict_with(
        &mut self,
        name: &Word,
        path: &[Word],
        body: &Word,
    ) -> Result<(), CompileError> {
        let Some(Target::Scalar(dict_name)) = target_of(name) else {
            return self.error("dict with on an array element is not supported yet");
        };
        let script = self.body_of(body)?;
        self.push_str(&dict_name);
        // By place, and for the reason `dict update` takes one: the write-back
        // has to reach the variable the binding read, and a place reaches a
        // frame slot as readily as a global.
        let place = self.var_place_operand(&dict_name);
        self.emit(Op::LoadInt(place), 1);
        for key in path {
            self.word(key)?;
        }
        self.emit(Op::LoadInt(path.len() as i64), 1);
        self.emit(
            Op::Extended(ext::DICT_WITH_BIND, 0),
            -(path.len() as i32 + 2),
        );
        self.finally_region(ext::DICT_WITH_END, |c| c.emit_body_value(&script))
    }

    /// `dict map {k v} $d {body}` — `dict for` that collects.
    ///
    /// Each iteration puts the body's *result* under the key the key variable
    /// holds when the body has finished, which is what `DictMapLoopCallback`
    /// reads (`generic/tclDictObj.c:3005-3012`) — so a body that reassigns `$k`
    /// changes the key the pair lands under, as it does in tclsh 9.0.4
    /// (measured: `dict map {k v} {a 1} {set k Z; set v}` is `Z 1`).
    ///
    /// A `break` throws the whole accumulation away rather than keeping what it
    /// had: the callback resets the result and leaves without ever setting it to
    /// the accumulator (`:2995-3003`). `dict filter` — the same walk otherwise —
    /// keeps its accumulation on a `break`, which is why the two endings differ.
    fn dict_map(&mut self, vars: &Word, dict: &Word, body: &Word) -> Result<(), CompileError> {
        let names = self.dict_each_vars("dict map", vars)?;
        self.dict_each_init(dict)?;
        let script = self.body_of(body)?;
        let key = names.0.clone();
        self.dict_each(&names, |c| {
            c.emit_body_value(&script)?;
            // The key is read *after* the body, from the variable the body may
            // have reassigned.
            c.emit_get_var(&key);
            c.dict_each_step(Step::Collect, -2);
            Ok(())
        })?;
        self.dict_each_step(Step::MapResult, 0);
        Ok(())
    }

    /// `dict filter $d script {k v} {body}` — `dict for` that keeps the pairs
    /// whose body answers true.
    ///
    /// The pair kept is the dictionary's own key and value, not what the two
    /// variables hold afterwards (`generic/tclDictObj.c:3410-3412`), and a
    /// `break` ends the walk while *keeping* what it has collected — the arm at
    /// `:3414` resets only the interpreter result and falls through to the
    /// `TCL_OK` ending.
    fn dict_filter_script(
        &mut self,
        dict: &Word,
        vars: &Word,
        body: &Word,
    ) -> Result<(), CompileError> {
        let names = self.dict_each_vars("dict filter", vars)?;
        self.dict_each_init(dict)?;
        let script = self.body_of(body)?;
        self.dict_each(&names, |c| {
            c.emit_body_value(&script)?;
            // The body's value is a Tcl boolean, refused in `expr`'s own
            // wording when it is not one.
            c.emit(Op::Extended(ext::BOOL, 0), 0);
            c.dict_each_step(Step::Keep, -1);
            Ok(())
        })?;
        self.dict_each_step(Step::FilterResult, 0);
        Ok(())
    }

    /// The two variable names one of these walks assigns.
    fn dict_each_vars(
        &mut self,
        what: &str,
        vars: &Word,
    ) -> Result<(String, String), CompileError> {
        let text = self.literal_of(vars, &format!("{what} variable list"))?;
        let names = split(text, "list").map_err(|msg| CompileError {
            msg,
            line: self.line,
        })?;
        let [key, value] = names.as_slice() else {
            // The reference interpreter only reaches this when the command runs
            // — `TclCompileDictForCmd` declines to compile a walk whose variable
            // list is not a pair and leaves it to `DictForNRCmd` — so
            // `catch {dict for {k} {a 1} {}}` catches it there. Marked so the
            // failure becomes code rather than a refusal to compile.
            return Err(self.deferrable_err("must have exactly two variable names".to_string()));
        };
        Ok((key.clone(), value.clone()))
    }

    /// Put the walk's state on the stack, where it stays for the whole loop.
    fn dict_each_init(&mut self, dict: &Word) -> Result<(), CompileError> {
        self.word(dict)?;
        self.emit(Op::Extended(ext::DICT_PAIRS, 0), 0);
        self.dict_each_step(Step::Init, 0);
        Ok(())
    }

    fn dict_each_step(&mut self, step: Step, delta: i32) {
        self.emit(Op::Extended(ext::DICT_EACH, step as u8), delta);
    }

    /// The loop: assign the two variables from the pair the cursor is on, run
    /// `body`, and step past that pair.
    fn dict_each<F>(&mut self, names: &(String, String), body: F) -> Result<(), CompileError>
    where
        F: FnOnce(&mut Self) -> Result<(), CompileError>,
    {
        let (key, value) = names.clone();
        self.rotated_loop(
            |c| {
                c.dict_each_step(Step::Take, 2);
                c.scalar_set_guard(&value);
                c.emit_set_var(&value);
                c.scalar_set_guard(&key);
                c.emit_set_var(&key);
                body(c)
            },
            |c| {
                c.dict_each_step(Step::Advance, 0);
                Ok(())
            },
            |c| {
                c.dict_each_step(Step::More, 1);
                Ok(())
            },
        )
    }
}

/// One step of the walk [`ext::DICT_EACH`] performs, in the op's inline operand.
#[derive(Clone, Copy)]
pub(crate) enum Step {
    /// `[pairs]` → the walk's state.
    Init = 0,
    /// `[state]` → `[state, 1]` while a pair remains.
    More = 1,
    /// `[state]` → `[state, key, value]` for the pair the cursor is on.
    Take = 2,
    /// `[state]` → `[state]`, past that pair.
    Advance = 3,
    /// `dict map`: `[state, value, key]` → `[state]`, the pair recorded.
    Collect = 4,
    /// `dict filter`: `[state, keep]` → `[state]`, the pair at the cursor
    /// recorded when `keep` is true.
    Keep = 5,
    /// `[state]` → the accumulation, or the empty string when a `break` left
    /// the walk unfinished.
    MapResult = 6,
    /// `[state]` → the accumulation, whatever ended the walk.
    FilterResult = 7,
    /// `[state]` → the empty string. What `dict for` answers.
    Discard = 8,
}

impl Step {
    fn of(arg: u8) -> Option<Step> {
        Some(match arg {
            0 => Step::Init,
            1 => Step::More,
            2 => Step::Take,
            3 => Step::Advance,
            4 => Step::Collect,
            5 => Step::Keep,
            6 => Step::MapResult,
            7 => Step::FilterResult,
            8 => Step::Discard,
            _ => return None,
        })
    }
}

/// The walk's state as it sits on the VM stack: the flattened pairs, then the
/// index of the pair being visited, then what has been collected.
fn dict_each_op(vm: &mut VM, arg: u8) -> Result<(), String> {
    const CORRUPT: &str = "corrupt dict walk state";
    let Some(step) = Step::of(arg) else {
        return Err(CORRUPT.to_string());
    };
    if let Step::Init = step {
        let Value::Array(pairs) = vm.pop() else {
            return Err(CORRUPT.to_string());
        };
        vm.push(Value::array(vec![
            Value::Array(pairs),
            Value::Int(0),
            Value::Str(Arc::new(String::new())),
        ]));
        return Ok(());
    }
    // Everything else reads the state where it sits, under whatever the step
    // consumes, so that a `break` unwinding the stack to the loop's entry depth
    // leaves it exactly where the result step will look.
    let taken = match step {
        Step::Collect => 2,
        Step::Keep => 1,
        _ => 0,
    };
    let mut popped = Vec::with_capacity(taken);
    for _ in 0..taken {
        popped.push(vm.pop());
    }
    let Some(Value::Array(state)) = vm.stack.last_mut() else {
        return Err(CORRUPT.to_string());
    };
    let [Value::Array(pairs), Value::Int(cursor), Value::Str(acc)] = Arc::make_mut(state).as_mut_slice() else {
        return Err(CORRUPT.to_string());
    };
    let at = *cursor as usize;
    match step {
        Step::Init => unreachable!("handled above"),
        Step::More => {
            let more = at < pairs.len();
            vm.push(Value::Int(more as i64));
        }
        Step::Take => {
            let key = pairs.get(at).cloned().unwrap_or(Value::Undef);
            let value = pairs.get(at + 1).cloned().unwrap_or(Value::Undef);
            vm.push(key);
            vm.push(value);
        }
        Step::Advance => *cursor += 2,
        Step::Collect => {
            // Pushed value first, then key, so the key came off first.
            let [key, value] = popped.as_slice() else {
                return Err(CORRUPT.to_string());
            };
            let next = dict_set(acc, &[to_tcl_string(key)], to_tcl_string(value))?;
            *acc = Arc::new(next);
        }
        Step::Keep => {
            let keep = popped.first().is_some_and(|v| tcl_int(v).unwrap_or(0) != 0);
            if keep {
                let key = pairs.get(at).cloned().unwrap_or(Value::Undef);
                let value = pairs.get(at + 1).cloned().unwrap_or(Value::Undef);
                let next = dict_set(acc, &[to_tcl_string(&key)], to_tcl_string(&value))?;
                *acc = Arc::new(next);
            }
        }
        Step::MapResult | Step::FilterResult | Step::Discard => {
            // A walk that ran out of pairs left the cursor past the end; a
            // `break` left it on the pair it stopped at, and that is the one
            // ending whose accumulation `dict map` throws away.
            let finished = at >= pairs.len();
            let result = match step {
                Step::Discard => String::new(),
                Step::MapResult if !finished => String::new(),
                _ => acc.to_string(),
            };
            vm.pop();
            vm.push(Value::Str(Arc::new(result)));
        }
    }
    Ok(())
}

/// The record [`ext::DICT_UPDATE_BIND`] builds and [`ext::DICT_UPDATE_END`]
/// consumes: where the dictionary variable lives, under what name, and which
/// key goes back from which variable.
///
/// It rides the VM stack for the reason the `dict for` cursor does — a hidden
/// global gave one call site one record, and `dict update` nests: the write-back
/// of an inner one would have been handed the outer one's keys.
struct DictUpdate {
    name: String,
    place: Place,
    /// `(key, where that key's variable lives)`, in the order written.
    bindings: Vec<(String, Place)>,
}

impl DictUpdate {
    /// The record as one stack value: name, place, then each key beside its
    /// variable's place.
    fn encode(&self) -> Value {
        let mut items = Vec::with_capacity(2 + self.bindings.len() * 2);
        items.push(Value::Str(Arc::new(self.name.clone())));
        items.push(Value::Int(self.place.encode()));
        for (key, place) in &self.bindings {
            items.push(Value::Str(Arc::new(key.clone())));
            items.push(Value::Int(place.encode()));
        }
        Value::array(items)
    }

    fn decode(value: &Value) -> Option<DictUpdate> {
        let Value::Array(items) = value else {
            return None;
        };
        let [name, place, rest @ ..] = items.as_slice() else {
            return None;
        };
        let bindings = rest
            .chunks_exact(2)
            .map(|pair| {
                let place = tcl_int(&pair[1]).ok()?;
                Some((to_tcl_string(&pair[0]), Place::decode(place)))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(DictUpdate {
            name: to_tcl_string(name),
            place: Place::decode(tcl_int(place).ok()?),
            bindings,
        })
    }
}

/// [`ext::DICT_UPDATE_BIND`]: read the dictionary, give each key's value to its
/// variable, and leave the record the write-back will need.
///
/// `DictUpdateCmd` (`generic/tclDictObj.c:3490-3541`) in the same order: the
/// variable must exist and must hold a dictionary, and only then is anything
/// assigned — a key the dictionary does not have *unsets* its variable rather
/// than emptying it, which is what makes `info exists` false inside the body.
fn dict_update_bind(vm: &mut VM) -> Result<(), String> {
    let count = pop_int(vm).max(0) as usize;
    let mut bound: Vec<(String, String, Place)> = Vec::with_capacity(count);
    for _ in 0..count {
        let place = place_of(vm);
        let var = pop_str(vm);
        let key = pop_str(vm);
        bound.push((key, var, place));
    }
    bound.reverse();
    let place = place_of(vm);
    let name = pop_str(vm);

    // `Tcl_ObjGetVar2(…, TCL_LEAVE_ERR_MSG)`: an absent variable is refused
    // here, unlike every other `dict` subcommand that writes one.
    let current = match peek(vm, place) {
        Some(Value::Hash(_)) => return Err(format!("can't read \"{name}\": variable is array")),
        Some(value) if *value != Value::Undef => to_tcl_string(value),
        _ => return Err(format!("can't read \"{name}\": no such variable")),
    };
    let dict = Dict::parse(&current)?;

    for (key, var, var_place) in &bound {
        match dict.get(key) {
            Some(value) => {
                let value = Value::Str(Arc::new(value.to_string()));
                let Some(cell) = crate::runtime::var_cell(vm, *var_place) else {
                    return Err(format!("can't set \"{var}\": no frame to set it in"));
                };
                if matches!(cell, Value::Hash(_)) {
                    return Err(format!("can't set \"{var}\": variable is array"));
                }
                *cell = value;
            }
            // `Tcl_UnsetVar2(…, 0)`: no flag, so a variable that was not there
            // is not an error either.
            None => {
                if let Some(cell) = crate::runtime::var_cell(vm, *var_place) {
                    *cell = Value::Undef;
                }
            }
        }
    }

    let record = DictUpdate {
        name,
        place,
        bindings: bound
            .into_iter()
            .map(|(key, _, place)| (key, place))
            .collect(),
    };
    vm.push(record.encode());
    Ok(())
}

/// [`ext::DICT_UPDATE_END`]: put the variables back into the dictionary,
/// whatever ended the body.
///
/// `FinalizeDictUpdate` (`generic/tclDictObj.c:3545-3596`), including both of
/// its silent paths: a dictionary variable that no longer exists means the whole
/// write-back is dropped (`:3564-3570`), and a *variable* that no longer exists
/// means its key leaves the dictionary (`:3580-3582`) — which is also what an
/// `array set` over that variable does, since the read that fails is the same
/// one (measured: the key is removed, not set to the array's list form).
fn dict_update_end(vm: &mut VM, above: u8) -> Result<(), String> {
    let record = take_record(vm, above)?;
    let Some(record) = DictUpdate::decode(&record) else {
        return Err("corrupt dict update record".to_string());
    };

    let current = match peek(vm, record.place) {
        Some(value) if !matches!(value, Value::Hash(_)) && *value != Value::Undef => {
            to_tcl_string(value)
        }
        // No dictionary variable to write back to: everything is dropped, and
        // silently — the body's own result stands.
        _ => return Ok(()),
    };
    // "Double-check that it is still a dictionary": a body that made it
    // something else fails the command here, replacing whatever the body left.
    let mut dict = Dict::parse(&current)?;

    for (key, place) in &record.bindings {
        match peek(vm, *place) {
            Some(value) if !matches!(value, Value::Hash(_)) && *value != Value::Undef => {
                dict.put(key.clone(), to_tcl_string(value));
            }
            _ => dict.remove(key),
        }
    }

    let written = Value::Str(Arc::new(dict.to_list()));
    match crate::runtime::var_cell(vm, record.place) {
        Some(cell) => *cell = written,
        None => return Err(format!("can't set \"{}\": no frame to set it in", record.name)),
    }
    Ok(())
}

/// What one key of a `dict with` binding turned into.
enum Bound {
    /// The key named a variable that could be reached, and it was assigned. The
    /// write-back reads it back through the same link.
    Var(Link),
    /// The key named a variable a procedure body never mentions, so it has no
    /// frame slot and no compiled op in that body can read or write it. Nothing
    /// was assigned; the value is carried here so the write-back can put the key
    /// back unchanged, which is what tclsh does for a key whose variable the body
    /// left alone (`generic/tclDictObj.c:3939`) — including when the body removed
    /// the key from the dictionary, which does *not* keep it out.
    Kept(String),
}

/// The record [`ext::DICT_WITH_BIND`] builds and [`ext::DICT_WITH_END`]
/// consumes: where the dictionary variable lives, under what name, which path
/// leads to the sub-dictionary that was opened out, and what became of each of
/// its keys.
///
/// It rides the VM stack for the reason `dict update`'s does — `dict with`
/// nests, over the same dictionary as readily as over another, and a hidden
/// global would hand an inner write-back the outer one's keys.
struct DictWith {
    name: String,
    place: Place,
    /// The path words, evaluated once when the command ran. tclsh keeps the list
    /// it built from them and the finalizer walks *that*
    /// (`generic/tclDictObj.c:3685`), so a path word with a side effect happens
    /// once however the body ends.
    path: Vec<String>,
    /// `(key, what it bound to)`, in the dictionary's own order — which is the
    /// order `TclDictWithInit` appends to `keysPtr` (`:3808-3816`) and therefore
    /// the order the write-back puts them back in.
    keys: Vec<(String, Bound)>,
}

impl DictWith {
    fn encode(&self) -> Value {
        let path = self
            .path
            .iter()
            .map(|k| Value::Str(Arc::new(k.clone())))
            .collect();
        let keys = self
            .keys
            .iter()
            .map(|(key, bound)| {
                let payload = match bound {
                    Bound::Var(link) => link.encode(),
                    Bound::Kept(value) => Value::Str(Arc::new(value.clone())),
                };
                Value::array(vec![Value::Str(Arc::new(key.clone())), payload])
            })
            .collect();
        Value::array(vec![
            Value::Str(Arc::new(self.name.clone())),
            Value::Int(self.place.encode()),
            Value::array(path),
            Value::array(keys),
        ])
    }

    fn decode(value: &Value) -> Option<DictWith> {
        let Value::Array(items) = value else {
            return None;
        };
        let [name, place, Value::Array(path), Value::Array(keys)] = items.as_slice() else {
            return None;
        };
        let keys = keys
            .iter()
            .map(|entry| {
                let Value::Array(pair) = entry else {
                    return None;
                };
                let [key, payload] = pair.as_slice() else {
                    return None;
                };
                // A link is a tagged `Value::Array` and a kept value is a string,
                // so the two cannot be mistaken for each other: the tag holds a
                // NUL, which no list a script built can start with.
                let bound = match Link::decode(payload) {
                    Some(link) => Bound::Var(link),
                    None => Bound::Kept(to_tcl_string(payload)),
                };
                Some((to_tcl_string(key), bound))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(DictWith {
            name: to_tcl_string(name),
            place: Place::decode(tcl_int(place).ok()?),
            path: path.iter().map(to_tcl_string).collect(),
            keys,
        })
    }
}

/// [`ext::DICT_WITH_BIND`]: open the dictionary out into variables named by its
/// own keys, and leave the record the write-back will need.
///
/// `DictWithCmd` and `TclDictWithInit` (`generic/tclDictObj.c:3649-3818`) in the
/// same order: the variable must exist, the path — when there is one — is walked
/// in read mode so a missing step is an error before anything is assigned, and
/// then every key of the sub-dictionary is assigned to the variable its own name
/// spells.
pub(crate) fn dict_with_bind(interp: &Shared, vm: &mut VM) -> Result<(), TclError> {
    let count = pop_int(vm).max(0) as usize;
    let mut path: Vec<String> = (0..count).map(|_| pop_str(vm)).collect();
    path.reverse();
    let place = place_of(vm);
    let name = pop_str(vm);

    // `Tcl_ObjGetVar2(…, TCL_LEAVE_ERR_MSG)` (`:3667`): an absent variable is
    // refused, exactly as `dict update`'s binding refuses one.
    let current = match peek(vm, place) {
        Some(Value::Hash(_)) => {
            return Err(TclError::plain(format!(
                "can't read \"{name}\": variable is array"
            )))
        }
        Some(value) if *value != Value::Undef => to_tcl_string(value),
        _ => {
            return Err(TclError::plain(format!(
                "can't read \"{name}\": no such variable"
            )))
        }
    };

    // `TclTraceDictPath(…, DICT_PATH_READ)` (`:3787`): a step the dictionary
    // does not have is an error here, where the write-back's own walk treats the
    // same absence as "drop it".
    let mut leaf = Dict::parse(&current).map_err(TclError::plain)?;
    for key in &path {
        let Some(next) = leaf.get(key) else {
            return Err(TclError::plain(format!(
                "key \"{key}\" not known in dictionary"
            )));
        };
        leaf = Dict::parse(next).map_err(TclError::plain)?;
    }

    let mut keys = Vec::with_capacity(leaf.len());
    for (key, value) in &leaf.entries {
        let Some(link) = crate::cmd_scope::dict_with_home(interp, vm, key)? else {
            keys.push((key.clone(), Bound::Kept(value.clone())));
            continue;
        };
        let assigned = Value::Str(Arc::new(value.clone()));
        match crate::cmd_scope::write_link(vm, &link) {
            // Assigning a scalar over a variable that holds an array is refused
            // in tclsh's own wording, and the refusal happens *during* the
            // binding, so the keys before it stay assigned (measured).
            Some(cell) if matches!(cell, Value::Hash(_)) => {
                return Err(TclError::plain(format!(
                    "can't set \"{key}\": variable is array"
                )))
            }
            Some(cell) => *cell = assigned,
            None => {
                return Err(TclError::plain(format!(
                    "can't set \"{key}\": variable isn't array"
                )))
            }
        }
        keys.push((key.clone(), Bound::Var(link)));
    }

    let record = DictWith {
        name,
        place,
        path,
        keys,
    };
    vm.push(record.encode());
    Ok(())
}

/// [`ext::DICT_WITH_END`]: put the variables back into the dictionary, whatever
/// ended the body.
///
/// `FinalizeDictWith` and `TclDictWithFinish` (`generic/tclDictObj.c:3696-3963`),
/// with all three of their quiet paths kept:
///
/// * a dictionary variable that no longer exists drops the whole write-back
///   (`:3875-3877`),
/// * a path that no longer leads anywhere drops it too (`:3912-3917`) — the
///   write-back's walk is `DICT_PATH_EXISTS`, where the binding's was
///   `DICT_PATH_READ`,
/// * and a *variable* that no longer exists takes its key out of the dictionary
///   (`:3929-3930`), which is the one way a `dict with` body can remove one.
///
/// Every other key is put back, including one the body deleted from the
/// dictionary: the keys are the list the binding recorded, not whatever the
/// dictionary holds now, so `dict with d {dict unset d k}` leaves `k` where it
/// was (measured against tclsh 9.0.4).
pub(crate) fn dict_with_end(vm: &mut VM, above: u8) -> Result<(), TclError> {
    let record = take_record(vm, above).map_err(TclError::plain)?;
    let Some(record) = DictWith::decode(&record) else {
        return Err(TclError::plain("corrupt dict with record"));
    };

    let current = match peek(vm, record.place) {
        Some(value) if !matches!(value, Value::Hash(_)) && *value != Value::Undef => {
            to_tcl_string(value)
        }
        _ => return Ok(()),
    };
    // "Double-check that it is still a dictionary" (`:3883`): a body that made
    // it something else fails the command here, replacing whatever the body
    // left.
    let mut root = Dict::parse(&current).map_err(TclError::plain)?;

    // Walk to the leaf, keeping the parents so the chain can be rebuilt: a
    // dictionary is its string here, so there is no shared sub-object to update
    // in place the way `InvalidateDictChain` does.
    let mut chain: Vec<(Dict, String)> = Vec::with_capacity(record.path.len());
    for key in &record.path {
        let Some(next) = root.get(key) else {
            // The path stopped leading anywhere while the body ran. Dropped, and
            // silently — the body's own result stands.
            return Ok(());
        };
        let child = Dict::parse(next).map_err(TclError::plain)?;
        chain.push((root, key.clone()));
        root = child;
    }

    for (key, bound) in &record.keys {
        match bound {
            Bound::Kept(value) => root.put(key.clone(), value.clone()),
            Bound::Var(link) => match crate::cmd_scope::read_link(vm, link) {
                Some(value) if !matches!(value, Value::Hash(_)) && *value != Value::Undef => {
                    root.put(key.clone(), to_tcl_string(value));
                }
                // A variable that is gone — or that became an array, which the
                // scalar read `Tcl_ObjGetVar2(key, NULL, 0)` cannot answer — is
                // "an instruction to remove the key".
                _ => root.remove(key),
            },
        }
    }

    while let Some((mut parent, key)) = chain.pop() {
        parent.put(key, root.to_list());
        root = parent;
    }

    let written = Value::Str(Arc::new(root.to_list()));
    match crate::runtime::var_cell(vm, record.place) {
        Some(cell) => *cell = written,
        None => {
            return Err(TclError::plain(format!(
                "can't set \"{}\": no frame to set it in",
                record.name
            )))
        }
    }
    Ok(())
}

/// Take the record a [`Compiler::finally_region`](crate::compiler::Compiler)
/// left under the `above` values the ending put on top of it.
///
/// The stack is the only place a record can live and still be found by both
/// endings: the driver unwinds to the region's entry depth before it resumes the
/// handler, so everything pushed before the region survives at exactly the
/// offset the compiler emitted.
fn take_record(vm: &mut VM, above: u8) -> Result<Value, String> {
    let at = vm
        .stack
        .len()
        .checked_sub(usize::from(above) + 1)
        .ok_or("corrupt finally record")?;
    Ok(vm.stack.remove(at))
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
            let tolerant = arg == 1;
            let empty = || Value::Str(Arc::new(String::new()));
            let value = match peek(vm, place) {
                Some(Value::Hash(map)) => match map.get(&index) {
                    Some(v) => v.clone(),
                    None if tolerant => empty(),
                    None => {
                        return Err(format!(
                            "can't read \"{name}({index})\": no such element in array"
                        ))
                    }
                },
                Some(Value::Undef) | None if tolerant => empty(),
                Some(Value::Undef) | None => {
                    return Err(format!("can't read \"{name}({index})\": no such variable"))
                }
                // A variable that exists and is not an array is refused by the
                // *store* rather than here when the read is the tolerant one:
                // measured, `append b(1) x` and `lappend b(1) x` on a scalar `b`
                // answer `can't set "b(1)": variable isn't array` in tclsh 9.0.3,
                // because `Tcl_AppendObjCmd`'s read is `TCL_LEAVE_ERR_MSG`-free
                // and the failure surfaces at `Tcl_ObjSetVar2`. Refusing here
                // gave the same complaint under the wrong verb.
                Some(_) if tolerant => empty(),
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
            // `incr` reads before it writes, so a variable that is not an array
            // is refused under `read` — measured, `incr b(1)` on a scalar `b`
            // answers `can't read "b(1)": variable isn't array` in tclsh 9.0.3,
            // where `append b(1) x` answers `can't set`. `TclIncrObjCmd` looks
            // the element up with `TCL_LEAVE_ERR_MSG` before incrementing, which
            // is the difference.
            if matches!(peek(vm, place), Some(v) if !matches!(v, Value::Hash(_) | Value::Undef)) {
                return Err(format!(
                    "can't read \"{name}({index})\": variable isn't array"
                ));
            }
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
            // A link may point at one element of an array, which has to be
            // *removed*; emptying the cell the way a scalar is emptied would
            // leave the key behind. See `cmd_scope::unset_link`.
            if let Place::Link(slot) = place {
                return match crate::cmd_scope::unset_link(vm, slot) {
                    true => Ok(()),
                    false if complain => Err(format!("can't unset \"{name}\": no such variable")),
                    false => Ok(()),
                };
            }
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
            let mut names = selected(vm, place, &filter)?;
            names.sort();
            vm.push(Value::Str(Arc::new(join(&names))));
            Ok(())
        }
        ext::ARR_GET => {
            let place = place_of(vm);
            let filter = pop_filter(vm);
            let mut names = selected(vm, place, &filter)?;
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
                    let doomed = selected(vm, place, &filter)?;
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
        ext::DICT_INCR => {
            let by = pop_str(vm);
            let key = pop_str(vm);
            let place = place_of(vm);
            let current = peek(vm, place).cloned().unwrap_or(Value::Undef);
            let name = pop_str(vm);
            if matches!(current, Value::Hash(_)) {
                return Err(format!("can't set \"{name}\": variable is array"));
            }
            push_str(vm, dict_incr(&to_tcl_string(&current), &key, &by)?);
            Ok(())
        }
        ext::DICT_EACH => dict_each_op(vm, arg),
        ext::DICT_UPDATE_BIND => dict_update_bind(vm),
        ext::DICT_UPDATE_END => dict_update_end(vm, arg),
        ext::DICT_PAIRS => {
            let dict = pop_str(vm);
            let d = Dict::parse(&dict)?;
            let mut flat = Vec::with_capacity(d.len() * 2);
            for (k, v) in d.entries {
                flat.push(Value::Str(Arc::new(k)));
                flat.push(Value::Str(Arc::new(v)));
            }
            vm.push(Value::array(flat));
            Ok(())
        }
        ext::DICT_REPLACE => {
            let mut args = pop_args(vm);
            let dict = args.remove(0);
            let mut d = Dict::parse(&dict)?;
            let mut it = args.into_iter();
            while let (Some(k), Some(v)) = (it.next(), it.next()) {
                d.put(k, v);
            }
            push_str(vm, d.to_list());
            Ok(())
        }
        ext::DICT_GETDEF => {
            let mut args = pop_args(vm);
            let dict = args.remove(0);
            let default = args.pop().expect("arity checked at compile time");
            let last = args.pop().expect("arity checked at compile time");
            // A missing step anywhere along the path is the default, not an
            // error — that is the whole difference from `dict get`. A dict that
            // does not parse is still an error, as it is there.
            let mut current = Dict::parse(&dict)?;
            for key in &args {
                match current.get(key) {
                    Some(next) => current = Dict::parse(next)?,
                    None => {
                        push_str(vm, default);
                        return Ok(());
                    }
                }
            }
            push_str(
                vm,
                current
                    .get(&last)
                    .map_or(default, |found| found.to_string()),
            );
            Ok(())
        }
        ext::DICT_UNSET => {
            let keys = pop_args(vm);
            let (current, _name) = dict_variable(vm)?;
            push_str(vm, dict_unset(&current, &keys)?);
            Ok(())
        }
        ext::DICT_LAPPEND | ext::DICT_APPEND => {
            let mut args = pop_args(vm);
            let key = args.remove(0);
            let (current, _name) = dict_variable(vm)?;
            let mut d = Dict::parse(&current)?;
            let existing = d.get(&key).unwrap_or("").to_string();
            let updated = if id == ext::DICT_LAPPEND {
                let mut elements = crate::list::split(&existing)?;
                elements.extend(args);
                crate::list::join(&elements)
            } else {
                let mut text = existing;
                for piece in args {
                    text.push_str(&piece);
                }
                text
            };
            d.put(key, updated);
            push_str(vm, d.to_list());
            Ok(())
        }
        ext::DICT_FILTER => {
            let mut args = pop_args(vm);
            let dict = args.remove(0);
            let on_value = args.remove(0) == "1";
            let d = Dict::parse(&dict)?;
            let mut kept = Dict::new();
            for (k, v) in d.entries {
                let subject = if on_value { &v } else { &k };
                if args.iter().any(|p| string_match(subject, p)) {
                    kept.put(k, v);
                }
            }
            push_str(vm, kept.to_list());
            Ok(())
        }
        other => Err(format!("unknown extension op {other}")),
    }
}

/// The `[name, place]` pair every in-place `dict` subcommand pushes below its
/// operands: the variable's current value as a string, and its name. An array
/// is refused with the reference implementation's wording, and a variable that
/// does not exist reads as the empty dict rather than as a failure.
fn dict_variable(vm: &mut VM) -> Result<(String, String), String> {
    let place = place_of(vm);
    let current = peek(vm, place).cloned().unwrap_or(Value::Undef);
    let name = pop_str(vm);
    if matches!(current, Value::Hash(_)) {
        return Err(format!("can't set \"{name}\": variable is array"));
    }
    Ok((to_tcl_string(&current), name))
}

/// `dict unset` down a key path. Every step but the last must resolve to a
/// dict; the last key is simply removed, and removing a key that is not there
/// is not an error.
fn dict_unset(dict: &str, keys: &[String]) -> Result<String, String> {
    let mut d = Dict::parse(dict)?;
    let (key, rest) = keys.split_first().expect("at least one key");
    if rest.is_empty() {
        // Removing the last key is not an error when it is not there — only a
        // key that the path has to walk *through* must exist.
        d.remove(key);
    } else {
        let Some(inner) = d.get(key).map(str::to_string) else {
            return Err(format!("key \"{key}\" not known in dictionary"));
        };
        d.put(key.clone(), dict_unset(&inner, rest)?);
    }
    Ok(d.to_list())
}

/// `dict incr`: the key's value plus the increment, the key created at zero when
/// it is absent.
///
/// A missing key counts as 0 rather than as an error, so `dict incr d fresh 7`
/// leaves `fresh 7` — measured against tclsh, along with the promotion past an
/// `i64` that [`crate::runtime::incr_text`] does.
fn dict_incr(dict: &str, key: &str, by: &str) -> Result<String, String> {
    let mut d = Dict::parse(dict)?;
    let current = d.get(key).unwrap_or("0").to_string();
    let sum = crate::runtime::incr_text(&current, by)?;
    d.put(key.to_string(), sum);
    Ok(d.to_list())
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

/// [`element_map`] for the ops outside this module that reach one element of an
/// array variable — the list commands, whose variable may be written `a(i)`.
pub(crate) fn elements_of(vm: &mut VM, place: Place) -> Option<&mut HashMap<String, Value>> {
    element_map(vm, place)
}

/// The element map of an array variable, creating it when the variable does not
/// exist yet. `None` when the variable holds a scalar.
pub(crate) fn element_map(vm: &mut VM, place: Place) -> Option<&mut HashMap<String, Value>> {
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
///
/// Fallible because of `-regexp`: the pattern is compiled, and a pattern that
/// will not compile is the command's error rather than an empty answer —
/// `array names a -regexp {a[}` reports `cannot compile regular expression
/// pattern: …` in tclsh, the same wording `regexp` gives for the same pattern.
fn selected(
    vm: &VM,
    place: Place,
    filter: &Option<(String, String)>,
) -> Result<Vec<String>, String> {
    let Some(Value::Hash(map)) = peek(vm, place) else {
        return Ok(Vec::new());
    };
    let mut names = Vec::new();
    for k in map.keys() {
        let keep = match filter {
            Some((mode, pattern)) if mode == "-exact" => k.as_str() == pattern,
            // `Tcl_RegExpMatch`: the pattern is searched for anywhere in the
            // name, not anchored to it, and case is never folded — `array
            // names` has no `-nocase`.
            Some((mode, pattern)) if mode == "-regexp" => {
                crate::regexp::matches_anywhere(pattern, k, false)?
            }
            Some((_, pattern)) => string_match(k, pattern),
            None => true,
        };
        if keep {
            names.push(k.clone());
        }
    }
    Ok(names)
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

/// Decode the operand [`Compiler::array_place`] pushed. [`Place::encode`] is the
/// other half; the three ranges it uses are stated there.
pub(crate) fn place_of(vm: &mut VM) -> Place {
    Place::decode(pop_int(vm))
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
        // A linked name is read through the descriptor its slot holds — still a
        // borrow of storage the VM owns, so an element read of an `upvar`'d
        // array costs no more than one of a local array does. A link that was
        // never made reads as nothing at all.
        Place::Link(slot) => {
            let link = crate::cmd_scope::link_at(vm, slot)?;
            crate::cmd_scope::read_link(vm, &link)
        }
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
