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

/// The byte offset of the first element of `src` that will not parse, past the
/// white space ahead of it, or `None` when every element parses. The walk
/// `string is list -failindex` and `string is dict -failindex` make over
/// `TclFindElement` (`generic/tclCmdMZ.c`, `StringIsCmd`).
pub(crate) fn unparsable_element(src: &str) -> Option<usize> {
    let bytes = src.as_bytes();
    let mut pos = 0;
    loop {
        while pos < bytes.len() && is_space(bytes[pos]) {
            pos += 1;
        }
        if pos == bytes.len() {
            return None;
        }
        match find_element(src, pos) {
            Ok((_, next)) => pos = next,
            Err(_) => return Some(pos),
        }
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
/// A looser screen than `is_multi_element`, and deliberately so: the
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

/// The text the interpreter quotes when a separator was expected: whatever
/// followed a close-brace or close-quote, up to twenty bytes.
///
/// Twenty *bytes*, as in the reference implementation — its own loop is
/// `while ((p2 < limit) && !TclIsSpaceProc(*p2) && (p2 < p+20))` in
/// `TclFindElement` — but never a partial character. A continuation byte is not
/// a space, so the walk runs straight through a multi-byte character and the
/// twenty-byte cap can land inside one; slicing there is a panic, and it took
/// the process down from a script as ordinary as
/// `llength {"a"xxxxxxxxxxxxxxxxxxxé}`. Backing up to the boundary drops the
/// partial character, which is what tclsh 9.0.4 prints for that script too
/// (nineteen `x` and nothing after them, measured).
///
/// One implementation for both callers: `assoc.rs` parses dictionary and array
/// elements with the same rule and reported the same panic from its own copy.
pub(crate) fn junk_prefix(src: &str, at: usize) -> &str {
    let bytes = src.as_bytes();
    let mut end = at;
    while end < bytes.len() && !is_space(bytes[end]) && end < at + 20 {
        end += 1;
    }
    while end > at && !src.is_char_boundary(end) {
        end -= 1;
    }
    &src[at..end]
}

/// The interpreter reports up to twenty characters of whatever followed a
/// close-brace or close-quote where a separator belonged.
fn junk_after(src: &str, pos: usize, what: &str) -> String {
    format!(
        "list element in {what} followed by \"{}\" instead of space",
        junk_prefix(src, pos)
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

/// The same grammar as [`parse_int`], refusing a value too wide for an `i64`
/// rather than saturating to one.
///
/// The two callers want opposite things and both are right. An *index* saturates
/// — `lindex {a b c} 99999999999999999999` is out of range whether the index is
/// that number or `i64::MAX`, and tclsh answers the empty string for it. An
/// operand of `lsort -integer` or `lsearch -integer` must not: tclsh raises
/// `integer value too large to represent` there, and saturating would sort by a
/// value the script never wrote.
pub fn parse_int_exact(text: &str) -> Option<i64> {
    let parsed = parse_int(text)?;
    // Saturation is the only way `parse_int` reaches either bound from digits,
    // so a value at one is either a genuine `i64::MIN`/`MAX` or an overflow;
    // re-reading the digits tells the two apart.
    if parsed == i64::MAX || parsed == i64::MIN {
        let trimmed = trim_space(text);
        let digits = trimmed.trim_start_matches(['-', '+']).replace('_', "");
        let magnitude = digits.trim_start_matches('0');
        let bound = if parsed == i64::MAX {
            "9223372036854775807"
        } else {
            "9223372036854775808"
        };
        // Only a decimal spelling is compared: a radix one is re-parsed by the
        // same accumulator and would need its own bound.
        if !trimmed.contains(['x', 'X', 'o', 'O', 'b', 'B'])
            && (magnitude.len() > bound.len()
                || (magnitude.len() == bound.len() && magnitude > bound))
        {
            return None;
        }
    }
    Some(parsed)
}

/// Tcl's double syntax. Integers are doubles too, so the integer grammar is
/// tried first and everything else goes through Rust's parser, which accepts
/// the same decimal, exponent, `Inf` and `NaN` spellings.
pub fn parse_double(text: &str) -> Option<f64> {
    if let Some(i) = parse_int(text) {
        return Some(i as f64);
    }
    let body = trim_space(text);
    let unsigned = body.strip_prefix(['-', '+']).unwrap_or(body);
    if let Some(nan) = crate::runtime::parse_nan(unsigned, body.starts_with('-')) {
        return Some(nan);
    }
    body.parse::<f64>().ok()
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
    match parse_int_exact(text) {
        Some(i) => Ok(i),
        // A spelling that is a perfectly good integer but too wide is its own
        // diagnostic in tclsh, not `expected integer but got …`: `lsort
        // -integer {99999999999999999999 5}` raises this rather than sorting.
        None if parse_int(text).is_some() => {
            Err("integer value too large to represent".to_string())
        }
        None => Err(number_error("integer", text)),
    }
}

/// A double operand, or the interpreter's diagnostic for one that is not.
pub fn double(text: &str) -> Result<f64, String> {
    parse_double(text).ok_or_else(|| number_error("floating-point number", text))
}

/// `expected integer but got "x"` — except that a value which is itself a list
/// of several elements is reported as “a list”, and a long one is cut at fifty
/// bytes.
pub(crate) fn number_error(kind: &str, text: &str) -> String {
    if is_multi_element(text) {
        return format!("expected {kind} but got a list");
    }
    let mut end = text.len().min(50);
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("expected {kind} but got \"{}\"", &text[..end])
}

/// `TCL_INDEX_NONE`, as [`index_encode`] answers it: a position no element has.
pub(crate) const INDEX_NONE: i64 = -1;
/// `TCL_INDEX_END`, as [`index_encode`] answers it: the last element.
pub(crate) const INDEX_END: i64 = -2;
/// `TCL_INDEX_START`, as [`index_encode`] answers it: the first element.
pub(crate) const INDEX_START: i64 = 0;

/// `TclIndexEncode` (`generic/tclUtil.c:3832`): the compile-time encoding of a
/// literal index, which the bytecode compiler uses to fold a command whose
/// index words are known. `before` and `after` stand for an index before the
/// first element and one past the last; an index in the encodable range comes
/// back as itself, and `end-N` as `INDEX_END - N`.
///
/// `None` is `TCL_ERROR`: the text is no index at all, or it is one the 64-bit
/// encoding has no room for — a plain index from 2³¹ up to `WIDE_MAX - 2`, or
/// `end-N` with `N` from 2³¹ - 1 up to `LIST_MAX`. The compiler then leaves the
/// command to run, and the index is read again then.
///
/// `end+1` is one of those: `GetEndOffsetFromObj` stores it as `WIDE_MAX`, a
/// non-negative offset, so it is read down the purely numeric path as the
/// number `2 * INT_MAX + 1`, which is out of range. `end+2` is `WIDE_MAX - 1`
/// and becomes `after`.
pub(crate) fn index_encode(text: &str, before: i64, after: i64) -> Option<i64> {
    const INT_MAX: i64 = i32::MAX as i64;
    // The "end value" `TclIndexEncode` hands `GetWideForIndex`.
    const END_VALUE: i64 = 2 * INT_MAX;
    // `LIST_MAX` on a 64-bit build: `(TCL_SIZE_MAX - offsetof(ListStore,
    // slots)) / sizeof(Tcl_Obj *)`, the slots following four eight-byte fields
    // and an `int`, padded to 40 bytes.
    const LIST_MAX: i64 = (i64::MAX - 40) / 8;

    let (wide, numeric) = wide_for_index(text, END_VALUE)?;
    let index = if numeric {
        if wide > INT_MAX && wide < i64::MAX - 1 {
            return None;
        }
        if wide > INT_MAX {
            after
        } else if wide < 0 {
            before
        } else {
            wide
        }
    } else {
        if wide > END_VALUE - LIST_MAX && wide <= INT_MAX {
            return None;
        }
        if wide > END_VALUE {
            after
        } else if wide <= INT_MAX {
            before
        } else {
            // `(int)wide`: `end` itself is `2 * INT_MAX`, which is -2.
            i64::from(wide as i32)
        }
    };
    Some(index)
}

/// `GetWideForIndex` with an `endValue` that is not -1, and whether
/// `TclIndexEncode` then reads the answer as a plain number — an integer, or an
/// index expression whose stored offset is not negative — rather than as one
/// counted from `end`.
fn wide_for_index(text: &str, end_value: i64) -> Option<(i64, bool)> {
    use crate::runtime::{parse_number, Num};
    match parse_number(trim_space(text)) {
        Ok(Num::Int(i)) => return Some((if i < 0 { -1 } else { i }, true)),
        Ok(Num::Big(b)) => {
            let negative = b.sign() == num_bigint::Sign::Minus;
            return Some((if negative { i64::MIN } else { i64::MAX }, true));
        }
        _ => {}
    }
    let offset = end_offset_value(text)?;
    let wide = if offset == i64::MAX {
        end_value + 1
    } else if offset == i64::MIN {
        -1
    } else if offset < 0 {
        end_value + offset + 1
    } else {
        offset
    };
    Some((wide, offset >= 0))
}

/// The offset `GetEndOffsetFromObj` (`generic/tclUtil.c:3532`) stores for a
/// non-numeric index: `WIDE_MAX` is `end+1`, `WIDE_MAX - 1` any later
/// `end+N`, -1 is `end`, `-N - 1` is `end-N`, `WIDE_MIN` is before the start,
/// and a non-negative value is an `integer±integer` sum.
fn end_offset_value(text: &str) -> Option<i64> {
    use crate::runtime::{parse_number, Num};
    use num_bigint::BigInt;
    // An integer-only parse: a double, a NaN or anything else is no index.
    let integer = |s: &str| match parse_number(trim_space(s)) {
        Ok(Num::Int(i)) => Some(BigInt::from(i)),
        Ok(Num::Big(b)) => Some(b),
        _ => None,
    };
    let truncate = |v: &BigInt| {
        i64::try_from(v).unwrap_or(if v.sign() == num_bigint::Sign::Minus {
            i64::MIN
        } else {
            i64::MAX
        })
    };
    let bytes = text.as_bytes();
    if !text.starts_with('e') {
        if is_multi_element(text) {
            return None;
        }
        let at = int_prefix_end(text);
        if at >= bytes.len() || (bytes[at] != b'+' && bytes[at] != b'-') {
            return None;
        }
        let (left, right) = (integer(&text[..at])?, integer(&text[at + 1..])?);
        let minus = bytes[at] == b'-';
        let offset = match (i64::try_from(&left), i64::try_from(&right)) {
            // Both wide: wide arithmetic, saturating at either bound.
            (Ok(w1), Ok(w2)) if !(minus && w2 == i64::MIN) => {
                let w2 = if minus { -w2 } else { w2 };
                w1.saturating_add(w2)
            }
            // A bignum on either side, or `- WIDE_MIN`: the exact sum, as
            // `Tcl_ExprObj` computes it, truncated to the wide range.
            _ => truncate(&if minus { left - right } else { left + right }),
        };
        return Some(if offset == -1 {
            i64::MIN
        } else if offset < 0 {
            i64::MIN + 1
        } else {
            offset
        });
    }
    if bytes.len() < 3 || bytes.len() == 4 || !text.starts_with("end") {
        return None;
    }
    if bytes.len() == 3 {
        return Some(-1);
    }
    if (bytes[3] != b'-' && bytes[3] != b'+') || is_space(bytes[4]) {
        return None;
    }
    let minus = bytes[3] == b'-';
    let value = integer(&text[4..])?;
    let offset = match i64::try_from(&value) {
        Ok(v) => {
            let v = if minus {
                if v == i64::MIN {
                    i64::MAX
                } else {
                    -v
                }
            } else {
                v
            };
            if v == 1 {
                i64::MAX
            } else if v > 1 {
                i64::MAX - 1
            } else if v != i64::MIN {
                v - 1
            } else {
                v
            }
        }
        // A bignum offset saturates in the direction it points.
        Err(_) => {
            let negative = value.sign() == num_bigint::Sign::Minus;
            if negative == minus {
                i64::MAX
            } else {
                i64::MIN
            }
        }
    };
    Some(offset)
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
