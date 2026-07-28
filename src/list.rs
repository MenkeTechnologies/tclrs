//! Tcl list syntax: the string ⇄ elements conversion.
//!
//! A Tcl list is a string, and every list command starts by re-deriving the
//! elements from it. The two halves here are ports of the reference
//! implementation:
//!
//! * [`split`] follows `TclFindElement`: whitespace separates elements, a
//!   leading brace or quote delimits one, backslash sequences are resolved
//!   everywhere except inside braces, and the diagnostics are the interpreter's
//!   own wording;
//! * [`join`] follows `TclScanElement`/`TclConvertElement`: an element is
//!   emitted bare when it can be, in braces when quoting is needed, and with
//!   backslash escapes when braces cannot express it. The mode selection is not
//!   the obvious one — an element needing quoting only because of `]` or an
//!   internal `"` escapes those characters while leaving its braces alone — and
//!   that historical shape is reproduced rather than tidied, because it is what
//!   tclsh 9.0.4 prints.
//!
//! Also here because they are list-shaped rather than command-shaped: Tcl's
//! index grammar ([`index`]), which every list command that takes a position
//! shares with `string index`, and the glob matcher ([`glob_match`]) that
//! `lsearch` matches with.

use crate::parser;

/// The characters Tcl treats as list-element separators: space and the five
/// ASCII controls `\t\n\v\f\r`. Nothing outside ASCII separates elements, so a
/// no-break space is ordinary text.
pub fn is_space(b: u8) -> bool {
    b == b' ' || (0x09..=0x0d).contains(&b)
}

// ── parsing ──────────────────────────────────────────────────────────────

/// Split a list into its elements.
pub fn split(src: &str) -> Result<Vec<String>, String> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut pos = 0;
    loop {
        while pos < bytes.len() && is_space(bytes[pos]) {
            pos += 1;
        }
        if pos == bytes.len() {
            return Ok(out);
        }
        let (element, next) = find_element(src, pos)?;
        out.push(element);
        pos = next;
    }
}

/// The number of elements without building them.
pub fn length(src: &str) -> Result<usize, String> {
    split(src).map(|v| v.len())
}

/// The most elements the string could hold, counted from its whitespace runs
/// alone — `TclMaxListLength`. The reference implementation uses it as a cheap
/// screen before a real parse, and the answer is observable: it decides whether
/// a bad number is reported as a list or quoted verbatim.
fn max_length(src: &str) -> usize {
    let bytes = src.as_bytes();
    if bytes.is_empty() {
        return 0;
    }
    let mut count = usize::from(!is_space(bytes[0]));
    let mut i = 0;
    while i < bytes.len() {
        if is_space(bytes[i]) {
            count += 1;
            while i < bytes.len() && is_space(bytes[i]) {
                i += 1;
            }
            continue;
        }
        i += 1;
    }
    // No element follows trailing white space.
    count - usize::from(is_space(bytes[bytes.len() - 1]))
}

/// Is this string a list of more than one element? The screen the reference
/// implementation applies before treating a string as a single index
/// (`tclUtil.c`, `TclIndexEncode`: `TclMaxListLength(...) > 1` **and** a parse
/// that yields more than one element), which is what keeps a list of indices
/// distinguishable from one index.
fn is_multi_element(src: &str) -> bool {
    max_length(src) > 1 && split(src).map(|v| v.len() > 1).unwrap_or(false)
}

/// Whether a value that is not the number or boolean a command wanted is named
/// as “a list” rather than quoted verbatim.
///
/// A looser screen than [`is_multi_element`], and deliberately so: the
/// reference implementation asks only whether the string *could* hold several
/// elements and whether it parses at all (`tclObj.c`, `TclSetBooleanFromAny`
/// and `TclNewIntObj`'s error path — `TclMaxListLength(...) > 1` and
/// `Tcl_SplitList == TCL_OK`, with no test on the element count). The
/// difference is observable: `a\ b` is one element whose text holds a space, and
/// `incr x a\ b` reports `expected integer but got a list` while
/// `lindex {a b c} a\ b` reports `bad index "a b"`.
pub fn looks_like_a_list(src: &str) -> bool {
    max_length(src) > 1 && split(src).is_ok()
}

/// Locate the element starting at `from`, which must index a non-space byte.
/// Returns the element's value and the offset of the next element.
fn find_element(src: &str, from: usize) -> Result<(String, usize), String> {
    let bytes = src.as_bytes();
    let limit = bytes.len();
    let mut pos = from;
    let mut open_braces: i64 = 0;
    let mut in_quotes = false;
    // False once a backslash outside braces makes the element's value differ
    // from the substring holding it.
    let mut literal = true;

    match bytes[pos] {
        b'{' => {
            open_braces = 1;
            pos += 1;
        }
        b'"' => {
            in_quotes = true;
            pos += 1;
        }
        _ => {}
    }
    let start = pos;
    let mut size = None;

    while pos < limit {
        match bytes[pos] {
            // An open brace only nests when the element is brace-delimited.
            b'{' => {
                if open_braces != 0 {
                    open_braces += 1;
                }
            }
            b'}' => {
                if open_braces > 1 {
                    open_braces -= 1;
                } else if open_braces == 1 {
                    size = Some(pos - start);
                    pos += 1;
                    if pos < limit && !is_space(bytes[pos]) {
                        return Err(junk_after(src, pos, "braces"));
                    }
                    break;
                }
            }
            b'\\' => {
                if open_braces == 0 {
                    literal = false;
                }
                // The whole escape is stepped over, so neither the space a
                // backslash-newline folds to nor an escaped brace can end the
                // element.
                pos = parser::backslash_at(src, pos).1 - 1;
            }
            b'"' if in_quotes => {
                size = Some(pos - start);
                pos += 1;
                if pos < limit && !is_space(bytes[pos]) {
                    return Err(junk_after(src, pos, "quotes"));
                }
                break;
            }
            b if is_space(b) && open_braces == 0 && !in_quotes => {
                size = Some(pos - start);
                break;
            }
            _ => {}
        }
        pos += 1;
    }

    let size = match size {
        Some(size) => size,
        None => {
            if open_braces != 0 {
                return Err("unmatched open brace in list".to_string());
            }
            if in_quotes {
                return Err("unmatched open quote in list".to_string());
            }
            pos - start
        }
    };

    while pos < limit && is_space(bytes[pos]) {
        pos += 1;
    }
    let raw = &src[start..start + size];
    let value = if literal {
        raw.to_string()
    } else {
        collapse(raw)
    };
    Ok((value, pos))
}

/// The interpreter reports up to twenty characters of whatever followed a
/// close-brace or close-quote where a separator belonged.
fn junk_after(src: &str, pos: usize, what: &str) -> String {
    let bytes = src.as_bytes();
    let mut end = pos;
    while end < bytes.len() && !is_space(bytes[end]) && end < pos + 20 {
        end += 1;
    }
    format!(
        "list element in {what} followed by \"{}\" instead of space",
        &src[pos..end]
    )
}

/// Resolve every backslash sequence in an element's text.
fn collapse(raw: &str) -> String {
    let mut out = String::new();
    let mut pos = 0;
    while pos < raw.len() {
        if raw.as_bytes()[pos] == b'\\' {
            let (text, next) = parser::backslash_at(raw, pos);
            out.push_str(&text);
            pos = next;
        } else {
            let ch = raw[pos..].chars().next().expect("char boundary");
            out.push(ch);
            pos += ch.len_utf8();
        }
    }
    out
}

// ── formatting ───────────────────────────────────────────────────────────

/// How an element has to be written to survive [`split`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// The element is its own representation.
    None,
    /// Wrap in braces.
    Brace,
    /// Escape every special character, braces included.
    Escape,
    /// Escape every special character except braces. Reached only when the
    /// element needs quoting solely because of `]` or an internal `"`.
    Mask,
}

/// Format elements as a canonical list: single spaces between elements, and
/// each element quoted no more than it must be.
pub fn join<S: AsRef<str>>(elements: &[S]) -> String {
    let mut out = String::new();
    for (i, element) in elements.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&quote(element.as_ref(), i == 0));
    }
    out
}

/// One element's list representation. A leading `#` needs quoting only in the
/// first element, where it would otherwise start a comment if the list were
/// evaluated as a script.
pub fn quote(src: &str, quote_hash: bool) -> String {
    convert(src, scan(src, quote_hash), quote_hash)
}

fn scan(src: &str, quote_hash: bool) -> Mode {
    if src.is_empty() {
        return Mode::Brace;
    }
    let bytes = src.as_bytes();
    let mut nesting: i64 = 0;
    let mut forbid_none = false;
    let mut require_escape = false;
    let mut prefer_escape = false;
    let mut prefer_brace = quote_hash && bytes[0] == b'#';

    if bytes[0] == b'{' || bytes[0] == b'"' {
        forbid_none = true;
        prefer_brace = true;
    }

    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => nesting += 1,
            b'}' => {
                nesting -= 1;
                if nesting < 0 {
                    // Unbalanced: braces cannot hold this element.
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
                if i + 1 == bytes.len() {
                    // A final backslash would escape the closing brace.
                    require_escape = true;
                } else if bytes[i + 1] == b'\n' {
                    // Braces keep the newline, which would not read back.
                    require_escape = true;
                    i += 1;
                } else {
                    if matches!(bytes[i + 1], b'{' | b'}' | b'\\') {
                        i += 1;
                    }
                    forbid_none = true;
                    prefer_brace = true;
                }
            }
            b if is_space(b) => {
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
        Mode::Escape
    } else if !forbid_none {
        Mode::None
    } else if prefer_escape && !prefer_brace {
        Mode::Mask
    } else {
        Mode::Brace
    }
}

fn convert(src: &str, mode: Mode, quote_hash: bool) -> String {
    if src.is_empty() {
        return "{}".to_string();
    }
    let mut out = String::new();
    let mut mode = mode;
    let mut rest = src;
    if quote_hash && src.starts_with('#') {
        if mode == Mode::Escape {
            out.push_str("\\#");
            rest = &src[1..];
        } else {
            mode = Mode::Brace;
        }
    }
    match mode {
        Mode::None => out.push_str(rest),
        Mode::Brace => {
            out.push('{');
            out.push_str(rest);
            out.push('}');
        }
        Mode::Escape | Mode::Mask => {
            for ch in rest.chars() {
                match ch {
                    ']' | '[' | '$' | ';' | ' ' | '\\' | '"' => {
                        out.push('\\');
                        out.push(ch);
                    }
                    '{' | '}' => {
                        if mode == Mode::Escape {
                            out.push('\\');
                        }
                        out.push(ch);
                    }
                    '\u{c}' => out.push_str("\\f"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    '\u{b}' => out.push_str("\\v"),
                    _ => out.push(ch),
                }
            }
        }
    }
    out
}

// ── numbers and indices ──────────────────────────────────────────────────

/// Tcl's integer syntax: optional surrounding whitespace, an optional sign, one
/// of the radix prefixes `0b 0o 0d 0x`, and digits that may be separated by
/// underscores. Values beyond `i64` saturate, which is where the reference
/// implementation switches to arbitrary precision — the two agree for every
/// index, since both ends are far outside any list.
pub fn parse_int(text: &str) -> Option<i64> {
    let text = trim_space(text);
    let (negative, body) = match text.as_bytes().first()? {
        b'-' => (true, &text[1..]),
        b'+' => (false, &text[1..]),
        _ => (false, text),
    };
    let (radix, digits) = match body.get(..2) {
        Some("0b") | Some("0B") => (2, &body[2..]),
        Some("0o") | Some("0O") => (8, &body[2..]),
        Some("0d") | Some("0D") => (10, &body[2..]),
        Some("0x") | Some("0X") => (16, &body[2..]),
        _ => (10, body),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') {
        return None;
    }
    let mut value: i64 = 0;
    let mut last_was_underscore = false;
    for ch in digits.chars() {
        if ch == '_' {
            if last_was_underscore {
                return None;
            }
            last_was_underscore = true;
            continue;
        }
        last_was_underscore = false;
        let digit = ch.to_digit(radix)? as i64;
        value = value
            .checked_mul(radix as i64)
            .and_then(|v| v.checked_add(digit))
            .unwrap_or(i64::MAX);
    }
    Some(if negative { -value } else { value })
}

/// Tcl's double syntax. Integers are doubles too, so the integer grammar is
/// tried first and everything else goes through Rust's parser, which accepts
/// the same decimal, exponent, `Inf` and `NaN` spellings.
pub fn parse_double(text: &str) -> Option<f64> {
    if let Some(i) = parse_int(text) {
        return Some(i as f64);
    }
    trim_space(text).parse::<f64>().ok()
}

fn trim_space(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_ascii() && is_space(c as u8))
}

/// Where the integer at the front of `text` ends, so an index expression can
/// find the operator that must follow it.
fn int_prefix_end(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() && is_space(bytes[i]) {
        i += 1;
    }
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    let radix = match text.get(i..i + 2) {
        Some("0b") | Some("0B") => 2,
        Some("0o") | Some("0O") => 8,
        Some("0d") | Some("0D") => 10,
        Some("0x") | Some("0X") => 16,
        _ => 0,
    };
    if radix != 0 {
        i += 2;
    }
    let radix = if radix == 0 { 10 } else { radix };
    while i < bytes.len() && ((bytes[i] as char).is_digit(radix) || bytes[i] == b'_') {
        i += 1;
    }
    i
}

/// An integer operand, or the interpreter's diagnostic for one that is not.
pub fn wide(text: &str) -> Result<i64, String> {
    parse_int(text).ok_or_else(|| number_error("integer", text))
}

/// A double operand, or the interpreter's diagnostic for one that is not.
pub fn double(text: &str) -> Result<f64, String> {
    parse_double(text).ok_or_else(|| number_error("floating-point number", text))
}

/// `expected integer but got "x"` — except that a value which is itself a list
/// of several elements is reported as “a list”, and a long one is cut at fifty
/// bytes.
fn number_error(kind: &str, text: &str) -> String {
    if is_multi_element(text) {
        return format!("expected {kind} but got a list");
    }
    let mut end = text.len().min(50);
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("expected {kind} but got \"{}\"", &text[..end])
}

/// Resolve an index against a list, with `end_value` the index `end` names —
/// one less than the length for commands that address an element, the length
/// itself for `linsert`, which can address the position after the last one.
///
/// Every value below zero comes back as -1. The reference implementation keeps
/// several distinct negative encodings, but no command distinguishes them: each
/// one clamps to the start of the list or reports no match.
pub fn index(text: &str, end_value: i64) -> Result<i64, String> {
    if let Some(value) = parse_int(text) {
        return Ok(if value < 0 { -1 } else { value });
    }
    end_offset(text, end_value)
}

/// The non-integer index forms: `end`, `end±integer`, and `integer±integer`.
fn end_offset(text: &str, end_value: i64) -> Result<i64, String> {
    let bad = || {
        Err(format!(
            "bad index \"{text}\": must be integer?[+-]integer? or end?[+-]integer?"
        ))
    };

    // `offset` uses the reference implementation's encoding: -1 is `end`, -n is
    // `end-(n-1)`, i64::MAX is `end+1`, i64::MAX-1 is any larger `end+n`, and a
    // non-negative value is a plain index.
    let offset;

    if !text.starts_with('e') {
        // A value that is a list of several elements is never an index; that
        // is what keeps a list of indices distinguishable from one index.
        if is_multi_element(text) {
            return bad();
        }
        // `integer±integer`. The operator sits immediately after the first
        // integer, and the second integer runs to the end: `1+2+3` is not an
        // index, because `2+3` is not an integer.
        let bytes = text.as_bytes();
        let at = int_prefix_end(text);
        if at >= bytes.len() || (bytes[at] != b'+' && bytes[at] != b'-') {
            return bad();
        }
        let (Some(left), Some(right)) = (parse_int(&text[..at]), parse_int(&text[at + 1..])) else {
            return bad();
        };
        let right = if bytes[at] == b'-' {
            right.saturating_neg()
        } else {
            right
        };
        let sum = left.saturating_add(right);
        offset = if sum < 0 { i64::MIN } else { sum };
    } else {
        let bytes = text.as_bytes();
        // `starts_with`, not `&text[..3]`: byte 3 may be inside a character —
        // `lindex {a b c} e€a` — and slicing there aborts the process where the
        // reference interpreter reports `bad index`.
        if bytes.len() < 3 || bytes.len() == 4 || !text.starts_with("end") {
            return bad();
        }
        if bytes.len() == 3 {
            offset = -1;
        } else {
            if bytes[3] != b'-' && bytes[3] != b'+' {
                return bad();
            }
            if is_space(bytes[4]) {
                return bad();
            }
            let Some(value) = parse_int(&text[4..]) else {
                return bad();
            };
            let value = if bytes[3] == b'-' {
                value.saturating_neg()
            } else {
                value
            };
            offset = if value == 1 {
                i64::MAX
            } else if value > 1 {
                i64::MAX - 1
            } else {
                value - 1
            };
        }
    }

    let resolved = if offset == i64::MAX {
        end_value.saturating_add(1)
    } else if offset == i64::MIN {
        -1
    } else if offset < 0 {
        end_value.saturating_add(offset).saturating_add(1)
    } else {
        offset
    };
    Ok(if resolved < 0 { -1 } else { resolved })
}

// ── glob matching ────────────────────────────────────────────────────────

/// Tcl's glob matcher, `Tcl_StringCaseMatch`, case-sensitively: `*` matches any
/// run, `?` one character, `[…]` a set or range (either way round), and a
/// backslash makes the next character literal. An unterminated `[…]` matches
/// only when both pattern and subject run out together.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let s: Vec<char> = text.chars().collect();
    matches(&p, &s)
}

fn matches(pattern: &[char], text: &[char]) -> bool {
    let mut pattern = pattern;
    let mut text = text;
    loop {
        let Some(&p) = pattern.first() else {
            return text.is_empty();
        };
        if text.is_empty() && p != '*' {
            return false;
        }

        if p == '*' {
            while pattern.first() == Some(&'*') {
                pattern = &pattern[1..];
            }
            let Some(&next) = pattern.first() else {
                return true;
            };
            loop {
                // Skip ahead to a plausible start when the pattern continues
                // with an ordinary character.
                if next != '[' && next != '?' && next != '\\' {
                    while let Some(&c) = text.first() {
                        if c == next {
                            break;
                        }
                        text = &text[1..];
                    }
                }
                if matches(pattern, text) {
                    return true;
                }
                if text.is_empty() {
                    return false;
                }
                text = &text[1..];
            }
        }

        if p == '?' {
            pattern = &pattern[1..];
            text = &text[1..];
            continue;
        }

        if p == '[' {
            pattern = &pattern[1..];
            let ch = text[0];
            text = &text[1..];
            loop {
                match pattern.first() {
                    None | Some(&']') => return false,
                    Some(&start) => {
                        pattern = &pattern[1..];
                        if pattern.first() == Some(&'-') {
                            pattern = &pattern[1..];
                            let Some(&end) = pattern.first() else {
                                return false;
                            };
                            pattern = &pattern[1..];
                            if (start <= ch && ch <= end) || (end <= ch && ch <= start) {
                                break;
                            }
                        } else if start == ch {
                            break;
                        }
                    }
                }
            }
            while pattern.first() != Some(&']') {
                if pattern.is_empty() {
                    return text.is_empty();
                }
                pattern = &pattern[1..];
            }
            pattern = &pattern[1..];
            continue;
        }

        if p == '\\' {
            pattern = &pattern[1..];
            if pattern.is_empty() {
                return false;
            }
        }

        if pattern[0] != text[0] {
            return false;
        }
        pattern = &pattern[1..];
        text = &text[1..];
    }
}
