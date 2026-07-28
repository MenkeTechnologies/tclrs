//! Tcl lists: splitting one into elements, and formatting values back into
//! one.
//!
//! A Tcl list is a string, so both directions are pure text transformations.
//! Splitting is what reads a `proc` argument specifier and the braced
//! `{pattern body ...}` form of `switch`; formatting is what builds the value
//! of a procedure's variadic `args` parameter, which `proc(n)` specifies as
//! being assembled "as if the list command had been used".
//!
//! The formatting side is ported from `Tcl_ScanElement` / `Tcl_ConvertElement`
//! in tclsh 9.0.4's `generic/tclUtil.c`, including the `COMPAT` branches that
//! source builds with (`#define COMPAT 1`) — they are why `a"b` formats as
//! `a\"b` rather than `{a"b}`.

/// Split a Tcl list into its elements.
///
/// Elements are separated by whitespace; braces and double quotes group one
/// element, and a backslash escapes the following character. Braces suppress
/// backslash processing exactly as they do in a script, apart from a
/// backslash-newline, which the list parser leaves alone.
pub fn split(text: &str) -> Result<Vec<String>, String> {
    let src = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;

    loop {
        while i < src.len() && is_space(src[i]) {
            i += 1;
        }
        if i >= src.len() {
            return Ok(out);
        }
        let (element, next) = match src[i] {
            b'{' => braced_element(src, i)?,
            b'"' => quoted_element(src, i)?,
            _ => bare_element(src, i),
        };
        out.push(element);
        i = next;
        // Only whitespace may separate two elements.
        if i < src.len() && !is_space(src[i]) {
            return Err(format!(
                "list element in {} followed by \"{}\" instead of space",
                if src[i - 1] == b'}' {
                    "braces"
                } else {
                    "quotes"
                },
                char_at(src, i)
            ));
        }
    }
}

fn braced_element(src: &[u8], at: usize) -> Result<(String, usize), String> {
    let mut depth = 1usize;
    let mut i = at + 1;
    let mut out = String::new();
    while i < src.len() {
        match src[i] {
            b'{' => {
                depth += 1;
                out.push('{');
                i += 1;
            }
            b'}' => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    return Ok((out, i));
                }
                out.push('}');
            }
            // A backslash inside braces protects only a brace from counting
            // toward the nesting, and both characters stay in the element.
            b'\\' if i + 1 < src.len() => {
                out.push('\\');
                push_char(&mut out, src, i + 1);
                i += 1 + utf8_len(src[i + 1]);
            }
            _ => {
                push_char(&mut out, src, i);
                i += utf8_len(src[i]);
            }
        }
    }
    Err("unmatched open brace in list".to_string())
}

fn quoted_element(src: &[u8], at: usize) -> Result<(String, usize), String> {
    let mut i = at + 1;
    let mut out = String::new();
    while i < src.len() {
        match src[i] {
            b'"' => return Ok((out, i + 1)),
            b'\\' if i + 1 < src.len() => {
                i = escape(src, i, &mut out);
            }
            _ => {
                push_char(&mut out, src, i);
                i += utf8_len(src[i]);
            }
        }
    }
    Err("unmatched open quote in list".to_string())
}

fn bare_element(src: &[u8], at: usize) -> (String, usize) {
    let mut i = at;
    let mut out = String::new();
    while i < src.len() && !is_space(src[i]) {
        if src[i] == b'\\' && i + 1 < src.len() {
            i = escape(src, i, &mut out);
        } else {
            push_char(&mut out, src, i);
            i += utf8_len(src[i]);
        }
    }
    (out, i)
}

/// A backslash sequence outside braces. The list parser recognises the same
/// escapes a script does; the ones that matter for round-tripping a formatted
/// element are `\n`, `\t`, `\r`, `\f`, `\v` and a backslash before a literal.
fn escape(src: &[u8], at: usize, out: &mut String) -> usize {
    let next = src[at + 1];
    let simple = match next {
        b'a' => Some('\u{7}'),
        b'b' => Some('\u{8}'),
        b'f' => Some('\u{c}'),
        b'n' => Some('\n'),
        b'r' => Some('\r'),
        b't' => Some('\t'),
        b'v' => Some('\u{b}'),
        _ => None,
    };
    match simple {
        Some(c) => {
            out.push(c);
            at + 2
        }
        None => {
            push_char(out, src, at + 1);
            at + 1 + utf8_len(next)
        }
    }
}

/// Format `value` as one element of a list. `first` marks the element that
/// starts the list, where a leading `#` has to be quoted so the list does not
/// read back as a comment.
pub fn quote_element(value: &str, first: bool) -> String {
    let mut conversion = scan(value, first);
    let bytes = value.as_bytes();

    if bytes.is_empty() {
        return "{}".to_string();
    }
    let mut out = String::new();
    let mut rest = value;
    if first && bytes[0] == b'#' {
        if conversion == Convert::Escape {
            out.push_str("\\#");
            rest = &value[1..];
        } else {
            conversion = Convert::Brace;
        }
    }
    match conversion {
        Convert::None => {
            out.push_str(rest);
            out
        }
        Convert::Brace => {
            out.push('{');
            out.push_str(rest);
            out.push('}');
            out
        }
        // `Escape` backslashes every special character; `Mask` — the mode Tcl
        // picks when only `]` or `"` forced quoting — leaves braces alone.
        mode => {
            for c in rest.chars() {
                match c {
                    ']' | '[' | '$' | ';' | ' ' | '\\' | '"' => {
                        out.push('\\');
                        out.push(c);
                    }
                    '{' | '}' => {
                        if mode == Convert::Escape {
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
            out
        }
    }
}

/// Join values into a list, quoting each as `Tcl_ConvertElement` would.
pub fn join(values: &[String]) -> String {
    let mut out = String::new();
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&quote_element(v, i == 0));
    }
    out
}

/// How an element has to be formatted, mirroring tclUtil.c's `CONVERT_*`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Convert {
    /// The literal text is already a valid element.
    None,
    /// Wrap it in braces.
    Brace,
    /// Backslash every special character, braces included.
    Escape,
    /// Backslash every special character but the braces, which are balanced.
    Mask,
}

/// Decide the formatting mode — the port of `TclScanElement`.
fn scan(value: &str, quote_hash: bool) -> Convert {
    let src = value.as_bytes();
    if src.is_empty() {
        return Convert::Brace;
    }

    let mut nesting = 0isize;
    let mut forbid_none = false;
    let mut require_escape = false;
    let mut prefer_escape = false;
    let mut prefer_brace = quote_hash && src[0] == b'#';

    if src[0] == b'{' || src[0] == b'"' {
        forbid_none = true;
        prefer_brace = true;
    }

    let mut i = 0;
    while i < src.len() {
        match src[i] {
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
                if i + 1 >= src.len() {
                    require_escape = true;
                } else if src[i + 1] == b'\n' {
                    require_escape = true;
                    i += 1;
                } else {
                    if matches!(src[i + 1], b'{' | b'}' | b'\\') {
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
        return Convert::Escape;
    }
    if forbid_none {
        if prefer_escape && !prefer_brace {
            return Convert::Mask;
        }
        return Convert::Brace;
    }
    Convert::None
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

fn push_char(out: &mut String, src: &[u8], at: usize) {
    let end = (at + utf8_len(src[at])).min(src.len());
    match std::str::from_utf8(&src[at..end]) {
        Ok(s) => out.push_str(s),
        Err(_) => out.push(char::REPLACEMENT_CHARACTER),
    }
}

fn char_at(src: &[u8], at: usize) -> char {
    let end = (at + utf8_len(src[at])).min(src.len());
    std::str::from_utf8(&src[at..end])
        .ok()
        .and_then(|s| s.chars().next())
        .unwrap_or(char::REPLACEMENT_CHARACTER)
}

fn utf8_len(lead: u8) -> usize {
    match lead {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}
