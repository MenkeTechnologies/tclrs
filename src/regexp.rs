//! `regexp` and `regsub`: Tcl's Advanced Regular Expressions, over the `regex`
//! crate.
//!
//! Tcl 9 matches with Henry Spencer's ARE engine, which is a backtracking
//! matcher and therefore able to express two things a finite-automaton matcher
//! cannot: back-references (`(a+)\1`) and look-ahead (`a(?=b)`). Both work in
//! tclsh 9.0.4 — measured, not assumed — and neither is expressible in the
//! `regex` crate at any price, because that crate's guarantee is linear time
//! and those constructs are what costs it.
//!
//! So this module translates the ARE syntax it *can* express and **refuses**
//! the rest with a Tcl-shaped error, which is this crate's convention
//! everywhere else: a refusal a script can catch beats a match that is quietly
//! wrong. What is refused is listed in `BUGS.md` and reported by name at the
//! point of use.
//!
//! ## What the translation has to correct
//!
//! Two defaults differ silently — the same pattern compiles under both engines
//! and means something else — so both are fixed here and pinned by
//! `tests/regexp_differential.rs`:
//!
//! * **`.` matches a newline in ARE and does not in Rust.** `regexp {a.b}
//!   "a\nb"` is 1 in tclsh. Every translated pattern is therefore prefixed
//!   `(?s)`, and `-linestop` is what turns that back off.
//! * **`-line` is two switches at once.** It is `-lineanchor` *and*
//!   `-linestop`: `^`/`$` gain line semantics (`(?m)`) and `.` loses the
//!   newline (`(?-s)`). Measured separately — `regexp -lineanchor {^b} "a\nb"`
//!   is 1 while `regexp -linestop {a.b} "a\nb"` is 0 — so they are two bits
//!   here rather than one.
//!
//! Indices are the third correction, and it is not about syntax: Tcl counts in
//! characters and Rust reports byte offsets, so `regexp -indices {b} "éb"` is
//! `1 1` in tclsh and would be `2 2` read off a Rust `Match` directly.
//! `CharIndex` does that conversion once per subject.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use fusevm::{Op, Value, VM};
use regex::Regex;

use crate::compiler::{ext as base_ext, CompileError, Compiler};
use crate::parser::Word;
use crate::runtime::{place_at, to_tcl_string, var_cell, Shared};

/// Extension opcode ids owned by this module. The base is declared with every
/// other module's in [`crate::compiler::ext`]; [`crate::runtime`] dispatches by
/// range from the highest base down, so this module's arm sits above the string
/// ensemble's.
pub mod ext {
    pub use crate::compiler::ext::REGEXP_BASE as BASE;
    /// `[flags, start, exp, string, place …]` → 1/0, the match count under
    /// `-all`, or the matched text under `-inline`. Writes the match variables
    /// itself, which is why it takes their places rather than their values.
    pub const REGEXP: u16 = BASE;
    /// `[flags, start, exp, string, subSpec, place?]` → the substituted string,
    /// or the substitution count when a variable name was given.
    pub const REGSUB: u16 = BASE + 1;
    /// `switch -regexp` with `-matchvar` and/or `-indexvar`:
    /// `[string, exp, nocase, given, matchplace, indexplace]` → "1"/"0", the
    /// same answer `cmd_string::ext::SWITCH_MATCH` gives, with the capture
    /// information written into the two places when the clause matched.
    pub const SWITCH_VARS: u16 = BASE + 2;
    /// The same two places, emptied: `[given, matchplace, indexplace]`. What
    /// `switch`'s `default` clause does, since no pattern ran for it.
    pub const SWITCH_CLEAR: u16 = BASE + 3;
}

// Switch bits, packed into one operand by the compiler because every switch is
// a literal word: nothing here has to be re-parsed when the op runs.
const F_NOCASE: i64 = 1;
const F_ALL: i64 = 1 << 1;
const F_INLINE: i64 = 1 << 2;
const F_INDICES: i64 = 1 << 3;
const F_LINEANCHOR: i64 = 1 << 4;
const F_LINESTOP: i64 = 1 << 5;
const F_EXPANDED: i64 = 1 << 6;
/// `regsub`'s variable-name argument was given, so the result is a count.
const F_INTO_VAR: i64 = 1 << 7;
/// `regsub -command`: the third word is a command prefix, not a `subSpec`.
const F_COMMAND: i64 = 1 << 8;

// ── compiling ────────────────────────────────────────────────────────────

/// The command names this module claims, for the REPL's completion and for the
/// `every_listed_command_compiles` check.
pub const COMMANDS: &[&str] = &["regexp", "regsub"];

/// Compile `regexp` or `regsub`.
///
/// Switches are read here, from literal words, the way `lsearch` and `lsort`
/// read theirs: a switch that is not a literal is not a switch, it is the
/// pattern. `--` ends them, which is how `regexp -- {-x}` matches a subject
/// beginning with a dash.
pub(crate) fn compile(c: &mut Compiler, name: &str, args: &[Word]) -> Result<(), CompileError> {
    let regsub = name == "regsub";
    let usage = if regsub {
        "regsub ?-option ...? exp string subSpec ?varName?"
    } else {
        "regexp ?-option ...? exp string ?matchVar? ?subMatchVar ...?"
    };

    let mut flags: i64 = 0;
    let mut start: Option<&Word> = None;
    let mut i = 0;
    while i < args.len() {
        let Some(text) = args[i].as_literal() else {
            break;
        };
        if !text.starts_with('-') || text == "-" {
            break;
        }
        i += 1;
        match text {
            "--" => break,
            "-nocase" => flags |= F_NOCASE,
            "-all" => flags |= F_ALL,
            "-expanded" => flags |= F_EXPANDED,
            "-line" => flags |= F_LINEANCHOR | F_LINESTOP,
            "-lineanchor" => flags |= F_LINEANCHOR,
            "-linestop" => flags |= F_LINESTOP,
            "-inline" if !regsub => flags |= F_INLINE,
            "-indices" if !regsub => flags |= F_INDICES,
            // `regsub -command`: `subSpec` stops being a replacement template
            // and becomes a command prefix, invoked once per match with the
            // whole match and every subexpression appended as arguments. Its
            // result replaces the match verbatim — `&` and `\1` are ordinary
            // characters in it (`Tcl_RegsubObjCmd`, `generic/tclCmdMZ.c`).
            "-command" if regsub => flags |= F_COMMAND,
            "-start" => {
                let Some(value) = args.get(i) else {
                    return c.error(format!("wrong # args: should be \"{usage}\""));
                };
                i += 1;
                start = Some(value);
            }
            // `regexp -about` is an option this frontend does not implement, so
            // it is named rather than reported as a bad one — the rule `switch
            // -matchvar` already follows. Reporting it as bad said `bad option
            // "-about": must be … -about …`, which contradicts itself in one
            // line. For `regsub` it *is* a bad option and falls through below,
            // which is tclsh's answer there too (measured).
            //
            // What it waits on is the second element of its result. The first
            // is the subexpression count and is easy; the second is a list of
            // `re_info` bits — `REG_UEMPTYMATCH`, `REG_ULOCALE`, `REG_UUNPORT`
            // and the rest (`generic/tclRegexp.c:644-659`) — that `regcomp.c`
            // sets about *itself* as it builds an NFA. A different engine can
            // only infer them, and the result is one list: half of it right and
            // half of it guessed is a wrong list, not a partial one.
            "-about" if !regsub => {
                c.push_str(
                    "regexp -about is not supported yet: its second element is the reference \
                     engine's own compile-time telemetry, which this engine can only infer",
                );
                c.emit(Op::Extended(base_ext::ERROR, 0), -1);
                c.push_empty();
                return Ok(());
            }
            // An unknown switch is a *runtime* error, as it is in tclsh: the
            // script compiles and the error is raised — and catchable — when
            // the command runs. `lsearch` behaves the same way here, and the
            // difference is visible: `catch {regexp -bogus a b}` returns 1
            // rather than taking the whole script down while compiling.
            //
            // The wording is the interpreter's, `-about` included: the list is
            // what `regexp` accepts, and this crate not implementing one of
            // them does not shorten it.
            other => {
                c.push_str(&format!(
                    "bad option \"{other}\": must be {}",
                    if regsub {
                        "-all, -command, -expanded, -line, -linestop, -lineanchor, -nocase, -start, or --"
                    } else {
                        "-all, -about, -indices, -inline, -expanded, -line, -linestop, -lineanchor, -nocase, -start, or --"
                    }
                ));
                c.emit(Op::Extended(base_ext::ERROR, 0), -1);
                c.push_empty();
                return Ok(());
            }
        }
    }

    let rest = &args[i..];
    // `regexp` takes exp + string + any number of match variables; `regsub`
    // takes exp + string + subSpec + at most one variable name.
    let (fixed, max_vars) = if regsub { (3, 1) } else { (2, usize::MAX) };
    if rest.len() < fixed || rest.len() - fixed > max_vars {
        return c.error(format!("wrong # args: should be \"{usage}\""));
    }
    let vars = &rest[fixed..];
    if flags & F_INLINE != 0 && !vars.is_empty() {
        return c.error("regexp match variables not allowed when using -inline");
    }
    if regsub && !vars.is_empty() {
        flags |= F_INTO_VAR;
    }

    // The match variables are resolved before anything is emitted. A name the
    // script computed is a refusal this command has not yet written code for,
    // which is what lets `Compiler::command` defer it — `if {0} {regexp a b $v}`
    // then costs the script nothing, as it costs tclsh nothing.
    let var_names = vars
        .iter()
        .map(|word| c.var_name_of(word))
        .collect::<Result<Vec<_>, _>>()?;

    c.emit(Op::LoadInt(flags), 1);
    match start {
        Some(word) => c.word(word)?,
        None => {
            c.emit(Op::LoadInt(0), 1);
        }
    }
    for word in &rest[..fixed] {
        c.word(word)?;
    }
    // A variable travels as where it lives, not as its value: the op assigns to
    // it. Encoded one operand per variable — the index shifted up by one with
    // the frame-slot bit at the bottom — so the operand count stays the arity.
    for name in &var_names {
        let encoded = c.place_operand(name);
        c.emit(Op::LoadInt(encoded), 1);
    }

    let operands = 2 + fixed + vars.len();
    let Ok(argc) = u8::try_from(operands) else {
        return c.error("too many match variables");
    };
    let id = if regsub { ext::REGSUB } else { ext::REGEXP };
    c.emit(Op::Extended(id, argc), 1 - operands as i32);
    Ok(())
}

// ── translating ARE to the regex crate's syntax ──────────────────────────

/// Translate an ARE pattern, or refuse it by name.
///
/// The refusals are the constructs `regex` cannot express at all. Everything
/// else is either identical between the two syntaxes or rewritten here; a
/// construct neither this function nor `regex` understands comes back as
/// `regex`'s own error, reworded to the interpreter's.
fn translate(are: &str, flags: i64) -> Result<String, String> {
    // The directors, which must be the very first characters of the pattern.
    // `***=` makes the rest a literal string; `***:` says "this is an ARE",
    // which is what it already is.
    if let Some(literal) = are.strip_prefix("***=") {
        return Ok(format!("{}{}", prefix(flags), regex::escape(literal)));
    }
    let body = are.strip_prefix("***:").unwrap_or(are);

    let mut out = String::with_capacity(body.len() + 8);
    let chars: Vec<char> = body.chars().collect();
    let mut i = 0;
    // Bracket expressions have their own sub-grammar: `\` is not an escape
    // inside one in ARE, and `[[.x.]]` / `[[=x=]]` only exist there.
    let mut in_class = false;
    // How many quantifiers the atom just emitted already carries. ARE allows
    // one, plus a single `?` after it meaning non-greedy, and calls anything
    // more `invalid quantifier operand` — `a**`, `a?*`, `a{2}{3}`, `a*??` are
    // all errors in tclsh while `regex` accepts every one of them with a
    // different meaning. Measured against tclsh 9.0.3.
    let mut quantifiers = 0u8;
    // Whether the previous character opened a group, in which case a `?` is
    // the start of `(?:`, `(?i)` and friends rather than a quantifier.
    let mut after_open = false;
    while i < chars.len() {
        let ch = chars[i];
        if in_class {
            // `[.` and `[=` open a collating element or an equivalence class,
            // neither of which `regex` has.
            if ch == '[' && matches!(chars.get(i + 1), Some('.') | Some('=')) {
                return Err(refusal(if chars[i + 1] == '.' {
                    "a collating element ([. .])"
                } else {
                    "an equivalence class ([= =])"
                }));
            }
            if ch == ']' {
                // The whole bracket expression is one atom, and a quantifier
                // may follow it: `[*]*` is a legal ARE.
                in_class = false;
                quantifiers = 0;
            }
            out.push(ch);
            i += 1;
            continue;
        }
        let opened = std::mem::take(&mut after_open);
        match ch {
            // A quantifier, and the place ARE's one-quantifier-per-atom rule is
            // enforced. `?` is two things at once: a quantifier of its own, and
            // the non-greedy marker on the quantifier before it — which is why
            // it is the one that may follow another and still be legal.
            '*' | '+' | '?' if !(ch == '?' && opened) => {
                let allowed = if ch == '?' { 1 } else { 0 };
                if quantifiers > allowed {
                    return Err(quantifier_operand());
                }
                quantifiers += 1;
                out.push(ch);
                i += 1;
            }
            // A bound is a quantifier too, and is emitted whole so the digits
            // inside it are not read as atoms of their own.
            '{' if is_bound(&chars[i..]) => {
                if quantifiers > 0 {
                    return Err(quantifier_operand());
                }
                let Some(close) = chars[i..].iter().position(|&c| c == '}') else {
                    // Malformed: let the engine report it, which `regerror`
                    // turns into `braces {} not balanced`.
                    out.push(ch);
                    i += 1;
                    continue;
                };
                out.extend(&chars[i..=i + close]);
                i += close + 1;
                quantifiers = 1;
            }
            '[' => {
                in_class = true;
                quantifiers = 0;
                out.push(ch);
                i += 1;
                // A `]` immediately after the opening bracket (or after a
                // negating `^`) is a literal, not the close.
                if chars.get(i) == Some(&'^') {
                    out.push('^');
                    i += 1;
                }
                if chars.get(i) == Some(&']') {
                    out.push_str("\\]");
                    i += 1;
                }
            }
            '(' => {
                match chars.get(i + 1) {
                    // Look-ahead is a backtracking construct. ARE has no
                    // look-behind at all — tclsh answers `invalid quantifier
                    // operand` for `(?<=a)b` — so only these two are refused.
                    Some('?') if matches!(chars.get(i + 2), Some('=') | Some('!')) => {
                        return Err(refusal("look-ahead ((?= ) or (?! ))"));
                    }
                    Some('?') if chars.get(i + 2) == Some(&'<') => {
                        return Err(refusal("look-behind ((?< ))"));
                    }
                    _ => {}
                }
                quantifiers = 0;
                after_open = true;
                out.push(ch);
                i += 1;
            }
            // ARE's bound is `{m}`, `{m,}` or `{m,n}` with decimal digits and
            // nothing else between the braces (`re_syntax(n)`). A `{` that does
            // not begin one is an ordinary character there — `regexp {a{} "a{"`
            // is 1 in tclsh — while `regex` reads `a{`, `a{,2}` and `a{x}` as
            // malformed repetitions and `a{ 2}` as `a{2}`. Escaping the ones
            // that are not bounds is what restores the ARE reading.
            '{' if !is_bound(&chars[i..]) => {
                quantifiers = 0;
                out.push_str("\\{");
                i += 1;
            }
            '\\' => {
                quantifiers = 0;
                let Some(&next) = chars.get(i + 1) else {
                    // A trailing backslash: let the engine report it.
                    out.push(ch);
                    i += 1;
                    continue;
                };
                match next {
                    // A back-reference. `\0` is not one — it is the whole match
                    // in a substitution and an octal escape in a pattern — so
                    // only 1-9 are refused.
                    '1'..='9' => return Err(refusal("a back-reference (\\1 … \\9)")),
                    // Word boundaries. `\y` is the plain one; `\m` and `\M` are
                    // its two halves, which need look-around to express.
                    'y' => {
                        out.push_str("\\b");
                        i += 2;
                        continue;
                    }
                    'Y' => {
                        out.push_str("\\B");
                        i += 2;
                        continue;
                    }
                    'm' => return Err(refusal("a word-start boundary (\\m)")),
                    'M' => return Err(refusal("a word-end boundary (\\M)")),
                    // ARE's end-of-string; `regex` spells it `\z`.
                    'Z' => {
                        out.push_str("\\z");
                        i += 2;
                        continue;
                    }
                    _ => {
                        out.push(ch);
                        out.push(next);
                        i += 2;
                        continue;
                    }
                }
            }
            _ => {
                quantifiers = 0;
                out.push(ch);
                i += 1;
            }
        }
    }
    Ok(format!("{}{}", prefix(flags), out))
}

/// Whether `chars`, which starts at a `{`, begins an ARE bound.
///
/// A digit is what commits to one. `{m}`, `{m,}` and `{m,n}` are the three
/// forms, and `regcomp` reports what is wrong with a *malformed* bound rather
/// than falling back to a literal — `a{1,` is `braces {} not balanced` in tclsh
/// and `a{2,1}` is `invalid repetition count(s)`, both errors. Anything else
/// after the brace never was a bound: `a{`, `a{,2}`, `a{x}` and `a{ 2}` all
/// match a literal `{` there, where `regex` would read the last two as
/// repetitions.
fn is_bound(chars: &[char]) -> bool {
    chars.get(1).is_some_and(char::is_ascii_digit)
}

/// The inline flags every translated pattern carries.
///
/// `(?s)` is the one that is not optional: ARE's `.` matches a newline and
/// Rust's does not, so the default has to be restored on every pattern and
/// `-linestop` is what removes it again.
fn prefix(flags: i64) -> String {
    let mut f = String::from("(?");
    if flags & F_NOCASE != 0 {
        f.push('i');
    }
    if flags & F_EXPANDED != 0 {
        f.push('x');
    }
    if flags & F_LINEANCHOR != 0 {
        f.push('m');
    }
    if flags & F_LINESTOP == 0 {
        f.push('s');
    }
    if f == "(?" {
        return String::new();
    }
    f.push(')');
    f
}

/// `REG_BADRPT`, the error a second quantifier on one atom raises.
fn quantifier_operand() -> String {
    "cannot compile regular expression pattern: invalid quantifier operand".to_string()
}

/// The wording for a construct this crate will not approximate.
fn refusal(what: &str) -> String {
    format!("{what} is not supported yet: the regular expression engine here matches in linear time, which back-references and look-around cannot")
}

thread_local! {
    /// Compiled patterns, keyed by the translated source. A `regexp` in a loop
    /// compiles its pattern once; without this it would compile per iteration,
    /// which is the cost that makes a matcher unusable in a script.
    ///
    /// The entry is an `Arc<Regex>` and *not* a `Regex`, which is not a detail:
    /// a `regex::Regex` carries a pool of per-search scratch caches, and the
    /// lazy DFA it builds while matching lives in one of them. Handing out a
    /// `clone()` of the `Regex` hands out a fresh, empty pool, so every call
    /// determinized the pattern again from nothing — 236 samples in
    /// `ByteClassRepresentatives::next` and the whole `regex_automata::hybrid`
    /// state machinery under a four-second profile of a matching loop, with the
    /// pattern compilation itself nowhere in it. Sharing one `Regex` through an
    /// `Arc` is what the crate is built for: 200,000 matches of
    /// `^[a-z]+([0-9]+)$` went from 7.507 s of CPU to 2.177 s, a debug build
    /// measured against tclsh 9.0.4's 0.374 s for the same script.
    /// Keyed by the pattern *as the script wrote it* together with the flags,
    /// not by what [`translate`] makes of it, so that the ARE-to-`regex`
    /// rewrite is paid once per pattern as well. A pattern that does not
    /// translate is not stored, so its refusal is raised on every call and not
    /// only the first.
    static CACHE: RefCell<HashMap<(i64, String), Arc<Regex>>> = RefCell::new(HashMap::new());
}

/// How many compiled patterns one thread keeps. At the limit the whole cache is
/// dropped rather than one entry chosen, which is [`crate::cache::ChunkCache`]'s
/// rule and holds for the same reason: a script that reaches it is building
/// fresh patterns, where no eviction order would have kept the useful one.
const CACHE_CAPACITY: usize = 1024;

/// The reference interpreter's name for a rejected pattern.
///
/// tclsh reports a bad pattern with one of `regcomp`'s own `REG_*` strings
/// (`generic/regex/regerrs.h`), which name the *construct* — `parentheses ()
/// not balanced` — where `regex` names the parse state it was in — `unclosed
/// group`. The two engines detect the same defects, so the detail is
/// translated back to the interpreter's word for it. Every row below was read
/// off tclsh 9.0.3 for the pattern in its comment.
///
/// A complaint with no row here keeps `regex`'s own text: it is a construct the
/// two engines do not classify the same way, and inventing a `REG_*` name for
/// it would be a wrong answer rather than a missing one.
fn regerror(detail: &str) -> &str {
    match detail {
        // `a[`, `[a-\`
        "unclosed character class" => "brackets [] not balanced",
        // `(a`, `a)`, `(?`
        "unclosed group" | "unopened group" => "parentheses () not balanced",
        // `*`, `+a`, `?a`, `**`
        "repetition operator missing expression" => "invalid quantifier operand",
        // `a{2,1}`
        "invalid repetition count range, the start must be <= the end" => {
            "invalid repetition count(s)"
        }
        // `[z-a]`
        "invalid character class range, the start must be <= the end" => "invalid character range",
        // `(?i`
        "expected flag but got end of regex" => "invalid embedded option",
        // `a{1,`
        "unclosed counted repetition" => "braces {} not balanced",
        other => other,
    }
}

fn compiled(are: &str, flags: i64) -> Result<Arc<Regex>, String> {
    let key = (flags, are.to_string());
    if let Some(re) = CACHE.with(|cache| cache.borrow().get(&key).map(Arc::clone)) {
        return Ok(re);
    }
    let translated = translate(are, flags)?;
    CACHE.with(|cache| {
        let re = Regex::new(&translated).map_err(|e| {
            // The interpreter's wording, with the engine's own complaint as the
            // detail — reworded from a multi-line report to one line.
            let detail = e.to_string();
            let first = detail
                .lines()
                .find(|l| l.trim_start().starts_with("error:"))
                .map(|l| l.trim_start().trim_start_matches("error:").trim())
                .unwrap_or("syntax error");
            format!(
                "cannot compile regular expression pattern: {}",
                regerror(first)
            )
        })?;
        let re = Arc::new(re);
        let mut cache = cache.borrow_mut();
        if cache.len() >= CACHE_CAPACITY {
            cache.clear();
        }
        cache.insert(key, Arc::clone(&re));
        Ok(re)
    })
}

// ── running ──────────────────────────────────────────────────────────────

/// Byte offsets of every character boundary in a subject, so a `regex` match's
/// byte span can be reported as Tcl's character indices.
struct CharIndex {
    /// `byte_of[i]` is where character `i` starts; the last entry is the length.
    byte_of: Vec<usize>,
}

impl CharIndex {
    fn new(s: &str) -> CharIndex {
        let mut byte_of: Vec<usize> = s.char_indices().map(|(b, _)| b).collect();
        byte_of.push(s.len());
        CharIndex { byte_of }
    }

    /// The character index of a byte offset.
    fn char_at(&self, byte: usize) -> usize {
        match self.byte_of.binary_search(&byte) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        }
    }

    /// The byte offset of a character index, clamped to the subject.
    fn byte_at(&self, ch: usize) -> usize {
        *self
            .byte_of
            .get(ch)
            .unwrap_or(self.byte_of.last().unwrap_or(&0))
    }

    fn chars(&self) -> usize {
        self.byte_of.len() - 1
    }
}

/// Whether `pattern` matches anywhere in `subject`, for the commands that take
/// a regular expression without being one: `lsearch -regexp` and
/// `switch -regexp`. `nocase` is their `-nocase`.
///
/// The pattern goes through the same translation and the same cache as
/// `regexp`'s, so a construct refused there is refused here with one wording.
pub(crate) fn matches_anywhere(pattern: &str, subject: &str, nocase: bool) -> Result<bool, String> {
    let flags = if nocase { F_NOCASE } else { 0 };
    Ok(compiled(pattern, flags)?.is_match(subject))
}

/// Execute one of this module's ops.
pub(crate) fn extension(vm: &mut VM, id: u16, argc: u8) -> Result<(), String> {
    let mut operands = Vec::with_capacity(argc as usize);
    for _ in 0..argc {
        operands.push(vm.pop());
    }
    operands.reverse();
    match id {
        ext::REGEXP => run_regexp(vm, &operands),
        // Without an interpreter `-command` has nothing to call. Reaching here
        // with it set would mean a caller outside the interpreter's own op
        // closure ran a `regsub`, so it is refused rather than silently
        // substituting the command prefix as if it were a `subSpec`.
        ext::REGSUB => run_regsub(None, vm, &operands),
        ext::SWITCH_VARS => run_switch_vars(vm, &operands),
        ext::SWITCH_CLEAR => run_switch_clear(vm, &operands),
        other => Err(format!("unknown regexp op {other}")),
    }
}

/// One `switch -regexp` clause, with the capture information kept.
///
/// `Tcl_SwitchObjCmd` runs `Tcl_RegExpExecObj` and, on a match, fills
/// `-matchvar` with the matched text of every subexpression and `-indexvar`
/// with their index pairs. A clause that does not match writes nothing at all:
/// the variables keep whatever the previous clause — or the script — left in
/// them.
fn run_switch_vars(vm: &mut VM, operands: &[Value]) -> Result<(), String> {
    let subject = to_tcl_string(operands.first().unwrap_or(&Value::Undef));
    let pattern = to_tcl_string(operands.get(1).unwrap_or(&Value::Undef));
    let nocase = matches!(operands.get(2), Some(Value::Int(1)));
    let given = match operands.get(3) {
        Some(Value::Int(g)) => *g,
        _ => 0,
    };
    let re = compiled(&pattern, if nocase { F_NOCASE } else { 0 })?;
    let Some(caps) = re.captures(&subject) else {
        vm.push(Value::Str(Arc::new("0".to_string())));
        return Ok(());
    };
    let idx = CharIndex::new(&subject);

    if given & 1 != 0 {
        let texts: Vec<String> = (0..caps.len())
            .map(|g| match caps.get(g) {
                Some(m) => subject[m.start()..m.end()].to_string(),
                None => String::new(),
            })
            .collect();
        let place = operands.get(4).ok_or("switch: no -matchvar place")?;
        assign(vm, place, crate::list::join(&texts))?;
    }
    if given & 2 != 0 {
        let pairs: Vec<String> = (0..caps.len())
            .map(|g| switch_indices(caps.get(g).map(|m| (m.start(), m.end())), &idx))
            .collect();
        let place = operands.get(5).ok_or("switch: no -indexvar place")?;
        assign(vm, place, crate::list::join(&pairs))?;
    }
    vm.push(Value::Str(Arc::new("1".to_string())));
    Ok(())
}

/// One index pair as `switch -indexvar` reports it, which is *not* how
/// `regexp -indices` reports the same match.
///
/// `Tcl_SwitchObjCmd` tests `info.matches[j].end > 0` and writes `-1 -1` when
/// it does not hold, so an empty match at the start of the subject is `-1 -1`
/// there while `regexp -indices -inline {} abc` is `0 -1`. Both measured
/// against tclsh 9.0.3; the two really are different rules, so this is a
/// function of its own rather than a call to [`indices`].
fn switch_indices(span: Option<(usize, usize)>, idx: &CharIndex) -> String {
    match span {
        Some((s, e)) if idx.char_at(e) > 0 => {
            format!("{} {}", idx.char_at(s), idx.char_at(e) as i64 - 1)
        }
        _ => "-1 -1".to_string(),
    }
}

/// `switch`'s `default` clause under `-matchvar`/`-indexvar`: both are set to
/// the empty list, because no regular expression ran to fill them.
fn run_switch_clear(vm: &mut VM, operands: &[Value]) -> Result<(), String> {
    let given = match operands.first() {
        Some(Value::Int(g)) => *g,
        _ => 0,
    };
    if given & 1 != 0 {
        let place = operands.get(1).ok_or("switch: no -matchvar place")?.clone();
        assign(vm, &place, String::new())?;
    }
    if given & 2 != 0 {
        let place = operands.get(2).ok_or("switch: no -indexvar place")?.clone();
        assign(vm, &place, String::new())?;
    }
    Ok(())
}

/// `regsub`, with the interpreter its `-command` may need.
///
/// Dispatched from the interpreter's own op closure for the reason `lsort` is:
/// whether a call says `-command` is a property of the call, and the one that
/// does invokes a *command* — `Tcl_EvalObjv`, so the words are arguments and
/// the callee gets a frame of its own rather than the caller's.
pub(crate) fn regsub_op(interp: &Shared, vm: &mut VM, argc: u8) -> Result<(), String> {
    let mut operands = Vec::with_capacity(argc as usize);
    for _ in 0..argc {
        operands.push(vm.pop());
    }
    operands.reverse();
    run_regsub(Some(interp), vm, &operands)
}

/// The switch operand, the `-start` operand, and the two the pattern needs.
fn head(operands: &[Value]) -> Result<(i64, i64, String, String), String> {
    let flags = match operands.first() {
        Some(Value::Int(f)) => *f,
        _ => return Err("regexp: switches missing".to_string()),
    };
    let start = match operands.get(1) {
        Some(Value::Int(n)) => *n,
        Some(other) => crate::list::wide(&to_tcl_string(other))?,
        None => 0,
    };
    let pattern = to_tcl_string(operands.get(2).unwrap_or(&Value::Undef));
    let subject = to_tcl_string(operands.get(3).unwrap_or(&Value::Undef));
    Ok((flags, start, pattern, subject))
}

/// Store a value in the variable an encoded place operand names.
fn assign(vm: &mut VM, encoded: &Value, value: String) -> Result<(), String> {
    let raw = match encoded {
        Value::Int(v) => *v,
        other => return Err(format!("regexp: not a variable place: {other:?}")),
    };
    let place = place_at(&Value::Int(raw >> 1), raw & 1 == 1)?;
    if let Some(cell) = var_cell(vm, place) {
        *cell = Value::Str(Arc::new(value));
    }
    Ok(())
}

/// Where a match loop stops, which is not the same question for the two
/// commands — see [`matches`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// `regexp -all`: the position at the end of the subject is not one that
    /// matches, but an empty subject still gets its one attempt.
    BeforeEnd,
    /// `regsub -all`: the end position matches too.
    PastEnd,
    /// `regsub -all` with an empty pattern, which matches at each character
    /// and nowhere else — so an empty subject matches nowhere at all.
    EachCharacter,
}

/// Every match from `from` on, in the order and at the positions tclsh finds
/// them.
///
/// `Regex::captures_iter` cannot be used for this: its empty-match rule is not
/// Tcl's. Measured against tclsh 9.0.4, where `-` marks each substitution:
///
/// ```text
/// regexp -all {b*} abc   → 3          regsub -all {b*} abc - → -a--c-   (4)
/// regexp -all {x*} ab    → 2          regsub -all {x*} ab -  → -a-b-    (3)
/// regexp -all {x*} ""    → 1          regsub -all {}   ab -  → -a-b     (2)
/// ```
///
/// Two rules come out of that. An empty match advances the cursor one
/// character, a non-empty one advances to its end — and the *last* position,
/// the one at the very end of the subject, is matched by `regsub` but not
/// counted by `regexp`. The third line is the exception that is not a rule: an
/// empty *pattern* — the literal `{}`, not `(?:)` or `a{0}`, which both behave
/// like `x*` — stops where `regexp` stops.
fn matches<'s>(
    re: &Regex,
    subject: &'s str,
    from: usize,
    idx: &CharIndex,
    stop: Stop,
) -> Vec<regex::Captures<'s>> {
    let len = subject.len();
    let mut found = Vec::new();
    let mut pos = from;
    // The empty pattern is the one that does not try a position at all when
    // there is none: `regsub -all {} ""` substitutes zero times, while
    // `regexp -all {} ""` still counts one match.
    if stop == Stop::EachCharacter && len == 0 {
        return found;
    }
    while let Some(caps) = re.captures_at(subject, pos) {
        let whole = caps.get(0).expect("group 0 always participates");
        let (s, e) = (whole.start(), whole.end());
        found.push(caps);
        pos = if e > s {
            e
        } else if s >= len {
            // An empty match at the very end has nowhere to advance to, and
            // leaving `pos` where it is would search the same position for
            // ever. One past the length ends both loop conditions below.
            len + 1
        } else {
            // One *character* past an empty match, not one byte: a byte step
            // would land inside a multi-byte character and the next search
            // would panic on a non-boundary offset.
            let ch = idx.char_at(s);
            idx.byte_at(ch + 1)
        };
        match stop {
            Stop::PastEnd if pos > len => break,
            Stop::BeforeEnd | Stop::EachCharacter if pos >= len => break,
            _ => {}
        }
    }
    found
}

/// One index pair, in Tcl's inclusive form. An unmatched group is `-1 -1`.
fn indices(span: Option<(usize, usize)>, idx: &CharIndex) -> String {
    match span {
        Some((s, e)) if e > s => format!("{} {}", idx.char_at(s), idx.char_at(e) - 1),
        // An empty match reports its end before its start, as tclsh does.
        Some((s, _)) => format!("{} {}", idx.char_at(s), idx.char_at(s) as i64 - 1),
        None => "-1 -1".to_string(),
    }
}

fn run_regexp(vm: &mut VM, operands: &[Value]) -> Result<(), String> {
    let (flags, start, pattern, subject) = head(operands)?;
    let places = &operands[4.min(operands.len())..];
    let re = compiled(&pattern, flags)?;
    let idx = CharIndex::new(&subject);

    // A negative `-start` is 0, and one past the end matches nothing at all.
    let from_char = start.max(0) as usize;
    if from_char > idx.chars() {
        return finish_no_match(vm, flags, places);
    }
    let from_byte = idx.byte_at(from_char);

    // Anchors stay relative to the whole subject — `regexp -start 1 {^b} ab` is
    // 0 in tclsh — so the search runs over the entire string and matches before
    // the offset are dropped, rather than matching a slice of it.
    // Without `-all` only the first match is wanted, and the iteration rule
    // only matters when every match is.
    let all = flags & F_ALL != 0;
    let found = if all {
        matches(&re, &subject, from_byte, &idx, Stop::BeforeEnd)
    } else {
        re.captures_at(&subject, from_byte).into_iter().collect()
    };

    let count = found.len() as i64;
    let mut inline: Vec<String> = Vec::new();
    if flags & F_INLINE != 0 {
        for caps in &found {
            for g in 0..caps.len() {
                let span = caps.get(g).map(|m| (m.start(), m.end()));
                inline.push(if flags & F_INDICES != 0 {
                    indices(span, &idx)
                } else {
                    span.map(|(s, e)| subject[s..e].to_string())
                        .unwrap_or_default()
                });
            }
        }
    }
    let last = found.into_iter().next_back();

    if flags & F_INLINE != 0 {
        vm.push(Value::Str(Arc::new(crate::list::join(&inline))));
        return Ok(());
    }

    let Some(caps) = last else {
        return finish_no_match(vm, flags, places);
    };
    // With `-all` the variables keep the last match, which is what tclsh
    // leaves behind.
    for (i, place) in places.iter().enumerate() {
        let span = caps.get(i).map(|m| (m.start(), m.end()));
        let text = if flags & F_INDICES != 0 {
            indices(span, &idx)
        } else {
            span.map(|(s, e)| subject[s..e].to_string())
                .unwrap_or_default()
        };
        assign(vm, place, text)?;
    }
    vm.push(Value::Int(if flags & F_ALL != 0 { count } else { 1 }));
    Ok(())
}

/// No match: the variables are left alone, and the answer is 0 — or the empty
/// list under `-inline`.
fn finish_no_match(vm: &mut VM, flags: i64, _places: &[Value]) -> Result<(), String> {
    if flags & F_INLINE != 0 {
        vm.push(Value::Str(Arc::new(String::new())));
    } else {
        vm.push(Value::Int(0));
    }
    Ok(())
}

fn run_regsub(interp: Option<&Shared>, vm: &mut VM, operands: &[Value]) -> Result<(), String> {
    let (flags, start, pattern, subject) = head(operands)?;
    let spec = to_tcl_string(operands.get(4).unwrap_or(&Value::Undef));
    let re = compiled(&pattern, flags)?;
    let idx = CharIndex::new(&subject);

    let from_char = start.max(0) as usize;
    let from_byte = if from_char > idx.chars() {
        subject.len()
    } else {
        idx.byte_at(from_char)
    };

    // `regsub -all` substitutes at the end position too, which `regexp -all`
    // does not count — except for the empty pattern, which stops where
    // `regexp` stops. Both measured; see [`matches`].
    let found = if flags & F_ALL != 0 {
        matches(
            &re,
            &subject,
            from_byte,
            &idx,
            if pattern.is_empty() {
                Stop::EachCharacter
            } else {
                Stop::PastEnd
            },
        )
    } else {
        re.captures_at(&subject, from_byte).into_iter().collect()
    };

    // Under `-command` every replacement is the result of a call, and the calls
    // all happen before anything is written back: `at_global` flushes the
    // running chunk's variables out and reprojects them after, so the
    // substituted string cannot be assembled from inside it.
    let replacements = if flags & F_COMMAND != 0 {
        Some(call_replacements(interp, vm, &spec, &found, &subject)?)
    } else {
        None
    };

    let mut out = String::with_capacity(subject.len());
    let mut count: i64 = 0;
    let mut cursor = 0usize;
    for (n, caps) in found.iter().enumerate() {
        let whole = caps.get(0).expect("group 0 always participates");
        out.push_str(&subject[cursor..whole.start()]);
        match &replacements {
            // The command's result is the replacement verbatim: `&` and `\1`
            // are ordinary characters in it, unlike a `subSpec`.
            Some(results) => out.push_str(&results[n]),
            None => expand(&spec, caps, &subject, &mut out),
        }
        cursor = whole.end();
        count += 1;
    }
    out.push_str(&subject[cursor..]);

    if flags & F_INTO_VAR != 0 {
        let place = operands.last().ok_or("regsub: variable place missing")?;
        assign(vm, place, out)?;
        vm.push(Value::Int(count));
    } else {
        vm.push(Value::Str(Arc::new(out)));
    }
    Ok(())
}

/// One replacement per match, each the result of calling `-command`'s prefix.
///
/// `Tcl_RegsubObjCmd` appends the whole match and then every subexpression —
/// including the ones that did not participate, which arrive as empty
/// arguments — to the prefix and invokes the lot with `Tcl_EvalObjv`. The
/// prefix must be a list of at least one element; anything shorter is the
/// command's own error and no substitution happens at all.
fn call_replacements(
    interp: Option<&Shared>,
    vm: &mut VM,
    spec: &str,
    found: &[regex::Captures],
    subject: &str,
) -> Result<Vec<String>, String> {
    let prefix = crate::list::split(spec)?;
    if prefix.is_empty() {
        return Err("command prefix must be a list of at least one element".to_string());
    }
    let Some(interp) = interp else {
        return Err("regsub -command needs an interpreter to call".to_string());
    };
    // Built before the interpreter is entered: the calls may themselves run
    // `regsub`, and the captures borrow the subject either way.
    let calls: Vec<Vec<String>> = found
        .iter()
        .map(|caps| {
            let mut words = prefix.clone();
            for g in 0..caps.len() {
                words.push(match caps.get(g) {
                    Some(m) => subject[m.start()..m.end()].to_string(),
                    None => String::new(),
                });
            }
            words
        })
        .collect();
    crate::runtime::at_global(interp, vm, |interp| {
        calls
            .iter()
            .map(|words| {
                crate::runtime::run_source(interp, &crate::list::join(words))
                    .map(|v| to_tcl_string(&v))
                    .map_err(|e| e.msg)
            })
            .collect()
    })
}

/// Expand a `regsub` replacement: `&` and `\0` are the whole match, `\1`…`\9`
/// are the groups, and a backslash escapes any of them.
fn expand(spec: &str, caps: &regex::Captures, subject: &str, out: &mut String) {
    let chars: Vec<char> = spec.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '&' => {
                if let Some(m) = caps.get(0) {
                    out.push_str(&subject[m.start()..m.end()]);
                }
                i += 1;
            }
            '\\' => match chars.get(i + 1) {
                Some(d @ '0'..='9') => {
                    let g = *d as usize - '0' as usize;
                    if let Some(m) = caps.get(g) {
                        out.push_str(&subject[m.start()..m.end()]);
                    }
                    i += 2;
                }
                Some(&other) => {
                    out.push(other);
                    i += 2;
                }
                None => {
                    out.push('\\');
                    i += 1;
                }
            },
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
}
