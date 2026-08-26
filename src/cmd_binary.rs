//! The `binary` ensemble — `format`, `scan`, `encode` and `decode`.
//!
//! Ported from `generic/tclBinary.c` at tclsh 9.0.4. The shapes that matter and
//! are easy to get subtly wrong, each taken from that file rather than from the
//! manual page:
//!
//! * **A field specifier is a type character, an optional `u` flag and an
//!   optional count, in that order**, with leading blanks skipped — and the
//!   `bad field specifier` diagnostic names the character the *unskipped*
//!   pointer was on, which is why `binary format {c 3} 1 2` reports a blank
//!   rather than the `3` that actually stopped it. `GetFormatSpec` advances the
//!   pointer past the blanks; the caller kept its own copy from before the
//!   call, and that copy is what the message prints.
//! * **`format` runs two passes.** The first resolves every count, checks that
//!   an argument exists for each field that consumes one, and computes the
//!   length; the second converts the values. That ordering is observable:
//!   `binary format b3c x` is `not enough arguments for all format specifiers`
//!   and not `expected binary string but got "x" instead`, because the missing
//!   argument is found in the pass that never looks at the value.
//! * **The result length is a high-water mark, not the cursor.** `@` and `X`
//!   move the cursor without shortening what has been written, so `binary
//!   format {@3X*a1} z` is three bytes with `z` first.
//! * **`scan` stops rather than failing** when the data runs out: the variables
//!   past that point are left alone and the command answers how many were set.
//!   Its argument check is the other way round from `format`'s — too few
//!   variables is an error, too many is not.
//!
//! A byte string here is a string whose every character is below U+0100, which
//! is what the reference interpreter's byte arrays look like read as strings
//! (`binary format c 200` is the one character U+00C8). So this module works in
//! `Vec<u8>` and converts at its edges, with the same refusal
//! `Tcl_GetBytesFromObj` raises for a character above U+00FF.

use std::sync::Arc;

use fusevm::{Op, Value, VM};
use num_bigint::{BigInt, BigUint};

use crate::assoc::{target_of, Target};
use crate::compiler::{CompileError, Compiler};
use crate::list;
use crate::parser::Word;
use crate::runtime::{format_double, place_at, to_tcl_string, var_cell};

/// Extension opcode ids owned by this module.
pub mod ext {
    pub use crate::compiler::ext::BINARY_BASE as BASE;
    /// `[format, arg …, count]` → the byte string.
    pub const FORMAT: u16 = BASE;
    /// `[value, format, (name, in_frame, place) …, count]` → how many variables
    /// were written.
    pub const SCAN: u16 = BASE + 1;
    /// `[option …, data, count]` → the encoded text. The inline operand is the
    /// codec's index in `CODECS`.
    pub const ENCODE: u16 = BASE + 2;
    /// The inverse, with the same stack shape.
    pub const DECODE: u16 = BASE + 3;
}

/// The command names this module claims, for the REPL's completion and the
/// reference page.
pub const COMMANDS: &[&str] = &["binary"];

/// Every subcommand, in the order the interpreter lists them when it rejects
/// one.
pub const SUBCOMMANDS: &[&str] = &["decode", "encode", "format", "scan"];

/// The `encode` / `decode` codecs, in the order the interpreter lists them.
pub const CODECS: &[&str] = &["base64", "hex", "uuencode"];

// ── compiling ────────────────────────────────────────────────────────────

/// Lower `binary …`.
pub(crate) fn compile(c: &mut Compiler, args: &[Word]) -> Result<(), CompileError> {
    let Some(first) = args.first() else {
        return c.error("wrong # args: should be \"binary subcommand ?arg ...?\"");
    };
    let given = c.literal_of(first, "subcommand")?.to_string();
    let Some(sub) = resolve(&given, SUBCOMMANDS) else {
        return c.error(format!(
            "unknown or ambiguous subcommand \"{given}\": must be {}",
            listing(SUBCOMMANDS)
        ));
    };
    let rest = &args[1..];
    match sub {
        "format" => compile_format(c, rest),
        "scan" => compile_scan(c, rest),
        "encode" => compile_codec(c, rest, ext::ENCODE, "encode"),
        _ => compile_codec(c, rest, ext::DECODE, "decode"),
    }
}

/// `binary format formatString ?arg ...?`.
fn compile_format(c: &mut Compiler, args: &[Word]) -> Result<(), CompileError> {
    if args.is_empty() {
        return c.error("wrong # args: should be \"binary format formatString ?arg ...?\"");
    }
    for w in args {
        c.word(w)?;
    }
    c.emit(Op::LoadInt(args.len() as i64 - 1), 1);
    c.emit(Op::Extended(ext::FORMAT, 0), -(args.len() as i32));
    Ok(())
}

/// `binary scan value formatString ?varName ...?`.
///
/// The variables are written by the op rather than by stores emitted after it,
/// for the reason [`crate::cmd_scan`] gives: which of them are written is
/// decided by how far the scan got.
fn compile_scan(c: &mut Compiler, args: &[Word]) -> Result<(), CompileError> {
    if args.len() < 2 {
        return c.error("wrong # args: should be \"binary scan value formatString ?varName ...?\"");
    }
    let mut names = Vec::with_capacity(args.len() - 2);
    for w in &args[2..] {
        match target_of(w) {
            Some(Target::Scalar(name)) => names.push(name),
            _ => return c.error("\"binary scan\" into an array element is not supported yet"),
        }
    }

    c.word(&args[0])?;
    c.word(&args[1])?;
    for name in &names {
        c.push_str(name);
        let place = c.var_place(name);
        c.push_value(Value::Int(i64::from(place.in_frame())));
        c.emit(Op::LoadInt(place.frame_operand()), 1);
    }
    c.emit(Op::LoadInt(names.len() as i64), 1);
    c.emit(Op::Extended(ext::SCAN, 0), -(3 * names.len() as i32 + 2));
    Ok(())
}

/// `binary encode format ?-option value ...? data` and its inverse.
fn compile_codec(c: &mut Compiler, args: &[Word], id: u16, verb: &str) -> Result<(), CompileError> {
    let Some(first) = args.first() else {
        return c.error(format!(
            "wrong # args: should be \"binary {verb} subcommand ?arg ...?\""
        ));
    };
    let given = c.literal_of(first, "subcommand")?.to_string();
    let Some(codec) = CODECS.iter().position(|name| *name == given) else {
        return c.error(format!(
            "unknown subcommand \"{given}\": must be {}",
            listing(CODECS)
        ));
    };
    let rest = &args[1..];
    if rest.is_empty() {
        return c.error(format!(
            "wrong # args: should be \"{}\"",
            usage(verb, codec)
        ));
    }
    for w in rest {
        c.word(w)?;
    }
    c.emit(Op::LoadInt(rest.len() as i64), 1);
    c.emit(Op::Extended(id, codec as u8), -(rest.len() as i32));
    Ok(())
}

/// An ensemble name, resolved by unique prefix as the interpreter resolves one.
fn resolve<'t>(name: &str, table: &[&'t str]) -> Option<&'t str> {
    if let Some(exact) = table.iter().find(|c| **c == name) {
        return Some(exact);
    }
    if name.is_empty() {
        return None;
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

// ── the format string ────────────────────────────────────────────────────

/// A count written after a field's type character.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Count {
    /// None written: one item.
    One,
    /// `*`: as many as the value or the data holds.
    All,
    Num(usize),
}

/// One field specifier, as `GetFormatSpec` reads it.
struct Spec {
    /// The character the format pointer was on *before* the leading blanks
    /// were skipped — what `bad field specifier` names.
    first: char,
    cmd: char,
    unsigned: bool,
    count: Count,
}

/// `GetFormatSpec` (`generic/tclBinary.c:2093`): leading blanks, a type
/// character, an optional `u` flag, an optional count.
fn next_spec(f: &[char], i: &mut usize) -> Option<Spec> {
    let start = *i;
    while f.get(*i) == Some(&' ') {
        *i += 1;
    }
    let cmd = *f.get(*i)?;
    *i += 1;
    let unsigned = f.get(*i) == Some(&'u');
    if unsigned {
        *i += 1;
    }
    let count = if f.get(*i) == Some(&'*') {
        *i += 1;
        Count::All
    } else if f.get(*i).is_some_and(char::is_ascii_digit) {
        let from = *i;
        while f.get(*i).is_some_and(char::is_ascii_digit) {
            *i += 1;
        }
        let text: String = f[from..*i].iter().collect();
        Count::Num(text.parse().unwrap_or(usize::MAX))
    } else {
        Count::One
    };
    Some(Spec {
        first: f[start],
        cmd,
        unsigned,
        count,
    })
}

fn bad_field(spec: &Spec) -> String {
    format!("bad field specifier \"{}\"", spec.first)
}

/// How wide one item of a numeric field is, or `None` when the type is not a
/// numeric one.
fn item_size(cmd: char) -> Option<usize> {
    Some(match cmd {
        'c' => 1,
        's' | 'S' | 't' => 2,
        'i' | 'I' | 'n' | 'f' | 'r' | 'R' => 4,
        'w' | 'W' | 'm' | 'd' | 'q' | 'Q' => 8,
        _ => return None,
    })
}

/// Whether a numeric field's bytes run most significant first.
fn big_endian(cmd: char) -> bool {
    match cmd {
        'S' | 'I' | 'W' | 'R' | 'Q' => true,
        't' | 'n' | 'm' | 'f' | 'd' => cfg!(target_endian = "big"),
        _ => false,
    }
}

/// The bytes a string stands for, refusing a character that is not one.
///
/// A port of `Tcl_GetBytesFromObj`'s loop (`generic/tclBinary.c:512`),
/// including its message. The index it names counts CHARACTERS, which is what
/// the count of bytes written so far comes to on this path. Measured against
/// tclsh 9.0.3, which is where the older "expected code point values below
/// 0xff but value at byte offset N was 0xM" wording was replaced:
///
/// ```text
/// % binary scan \u0100 a* v
/// expected byte sequence but character 0 was 'Ā' (U+000100)
/// ```
pub(crate) fn as_bytes(text: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(text.len());
    for ch in text.chars() {
        let cp = u32::from(ch);
        if cp > 255 {
            return Err(format!(
                "expected byte sequence but character {} was '{ch}' (U+{cp:06X})",
                out.len()
            ));
        }
        out.push(cp as u8);
    }
    Ok(out)
}

/// The string a byte string is read back as: one character per byte, which is
/// the range U+0000–U+00FF the manual page describes.
pub(crate) fn from_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|b| char::from(*b)).collect()
}

// ── binary format ────────────────────────────────────────────────────────

/// The largest result `binary format` will build, so that a count the script
/// wrote cannot ask the allocator for a string no machine has room for.
const MAX_RESULT_BYTES: usize = 2 * 1024 * 1024 * 1024;

/// One field, with its count resolved and the argument it consumes located —
/// what the first of `binary format`'s two passes produces.
struct Planned {
    cmd: char,
    count: usize,
    arg: usize,
    /// The field wrote no count, so a numeric field takes its argument whole
    /// rather than as a list — which is the whole of the difference between
    /// `binary format c {2 5}` (a refusal) and `binary format c1 {2 5}` (one
    /// byte).
    scalar: bool,
}

/// `binary format formatString ?arg ...?`.
fn format(fmt: &str, args: &[String]) -> Result<Vec<u8>, String> {
    let f: Vec<char> = fmt.chars().collect();
    let (plan, length) = plan_format(&f, args)?;
    let mut out = vec![0u8; length];
    let mut at = 0usize;

    for step in plan {
        let count = step.count;
        match step.cmd {
            'a' | 'A' => {
                let pad = if step.cmd == 'A' { b' ' } else { 0 };
                let bytes = low_bytes(&args[step.arg]);
                for slot in 0..count {
                    out[at + slot] = bytes.get(slot).copied().unwrap_or(pad);
                }
                at += count;
            }
            'b' | 'B' => {
                let bits: Vec<char> = args[step.arg].chars().collect();
                let width = count.div_ceil(8);
                for byte in 0..width {
                    let mut value = 0u8;
                    for bit in 0..8 {
                        let index = byte * 8 + bit;
                        if index >= count {
                            break;
                        }
                        let one = match bits.get(index) {
                            Some('1') => true,
                            Some('0') => false,
                            Some(_) => {
                                return Err(format!(
                                    "expected binary string but got \"{}\" instead",
                                    cut(&args[step.arg])
                                ))
                            }
                            // Fewer digits than the count: the rest are zero.
                            None => false,
                        };
                        if one {
                            value |= if step.cmd == 'b' {
                                1 << bit
                            } else {
                                0x80 >> bit
                            };
                        }
                    }
                    out[at + byte] = value;
                }
                at += width;
            }
            'h' | 'H' => {
                let text: Vec<char> = args[step.arg].chars().collect();
                let width = count.div_ceil(2);
                for byte in 0..width {
                    let mut value = 0u8;
                    for half in 0..2 {
                        let index = byte * 2 + half;
                        if index >= count {
                            break;
                        }
                        let digit = match text.get(index) {
                            Some(ch) => ch.to_digit(16).ok_or_else(|| {
                                format!(
                                    "expected hexadecimal string but got \"{}\" instead",
                                    cut(&args[step.arg])
                                )
                            })? as u8,
                            None => 0,
                        };
                        // `H` fills the high half of each byte first, `h` the
                        // low half — the whole difference between the two.
                        let high = (step.cmd == 'H') == (half == 0);
                        value |= if high { digit << 4 } else { digit };
                    }
                    out[at + byte] = value;
                }
                at += width;
            }
            'x' => {
                // Written, not skipped: `X` and `@` can have moved the cursor
                // back over bytes an earlier field stored, and `x` clears them.
                // `binary format {su1X8s0x3} 1 255` is three null bytes in
                // tclsh, not the `01` the first field wrote.
                out[at..at + count].fill(0);
                at += count;
            }
            'X' => at = at.saturating_sub(count),
            '@' => at = count,
            cmd => {
                let size = item_size(cmd).expect("plan_format rejected every other type");
                let values = numeric_items(&args[step.arg], count, step.scalar)?;
                for value in values {
                    write_number(&mut out[at..at + size], cmd, &value)?;
                    at += size;
                }
            }
        }
    }
    Ok(out)
}

/// The first pass: resolve each count, find each field's argument and compute
/// the result's length. Nothing here looks at a value's contents, which is what
/// puts `not enough arguments for all format specifiers` ahead of every
/// conversion diagnostic.
fn plan_format(f: &[char], args: &[String]) -> Result<(Vec<Planned>, usize), String> {
    let mut plan = Vec::new();
    let mut at = 0usize;
    let mut length = 0usize;
    let mut arg = 0usize;
    let mut i = 0usize;

    while let Some(spec) = next_spec(f, &mut i) {
        let consumes = !matches!(spec.cmd, 'x' | 'X' | '@');
        if consumes && arg >= args.len() {
            // Reported for a bad type character too, but only after the type
            // itself has been recognised — `binary format Zc 1` is the bad
            // field, `binary format cZ 1` is as well.
            if item_size(spec.cmd).is_none()
                && !matches!(spec.cmd, 'a' | 'A' | 'b' | 'B' | 'h' | 'H')
            {
                return Err(bad_field(&spec));
            }
            return Err("not enough arguments for all format specifiers".to_string());
        }
        let count = match spec.cmd {
            'a' | 'A' => match spec.count {
                Count::All => args[arg].chars().count(),
                Count::One => 1,
                Count::Num(n) => n,
            },
            'b' | 'B' | 'h' | 'H' => match spec.count {
                Count::All => args[arg].chars().count(),
                Count::One => 1,
                Count::Num(n) => n,
            },
            'x' => match spec.count {
                Count::All => {
                    return Err("cannot use \"*\" in format string with \"x\"".to_string())
                }
                Count::One => 1,
                Count::Num(n) => n,
            },
            'X' => match spec.count {
                // `*`, or a count past the cursor, is the whole way back.
                Count::All => at,
                Count::One => 1,
                Count::Num(n) => n,
            },
            '@' => match spec.count {
                Count::One => return Err("missing count for \"@\" field specifier".to_string()),
                Count::All => length,
                Count::Num(n) => n,
            },
            cmd if item_size(cmd).is_some() => match spec.count {
                Count::One => 1,
                Count::All => list::length(&args[arg])?,
                Count::Num(n) => {
                    if list::length(&args[arg])? < n {
                        return Err("number of elements in list does not match count".to_string());
                    }
                    n
                }
            },
            _ => return Err(bad_field(&spec)),
        };

        let step = Planned {
            cmd: spec.cmd,
            count,
            arg,
            scalar: spec.count == Count::One,
        };
        if consumes {
            arg += 1;
        }
        match spec.cmd {
            'a' | 'A' => at += count,
            'b' | 'B' => at += count.div_ceil(8),
            'h' | 'H' => at += count.div_ceil(2),
            'x' => at += count,
            'X' => at = at.saturating_sub(count),
            '@' => at = count,
            cmd => at += count * item_size(cmd).expect("checked above"),
        }
        if at > length {
            length = at;
        }
        if length > MAX_RESULT_BYTES {
            return Err("max size for a Tcl value exceeded".to_string());
        }
        plan.push(step);
    }
    Ok((plan, length))
}

/// The `count` values a numeric field takes from its argument. A field with no
/// count written takes the argument whole, which is why `binary format c {2 5}`
/// is `expected integer but got a list` rather than two bytes, while `binary
/// format c1 {2 5}` is the first element.
///
/// The lengths were checked by [`plan_format`], so truncating is all that is
/// left to do here.
fn numeric_items(arg: &str, count: usize, scalar: bool) -> Result<Vec<String>, String> {
    if scalar {
        return Ok(vec![arg.to_string()]);
    }
    let mut items = list::split(arg)?;
    items.truncate(count);
    Ok(items)
}

/// Write one number into `slot`, whose length is the field's item size.
///
/// The two diagnostics name an unusable value through [`crate::runtime::named`]
/// — `a list` whenever the text *could* hold several elements — which is the
/// looser of the two screens this frontend carries and the one the reference
/// implementation applies here: `binary format c {a b}` is `expected integer
/// but got a list` even though `{a b}` is a single element whose text happens
/// to hold a blank.
fn write_number(slot: &mut [u8], cmd: char, text: &str) -> Result<(), String> {
    let bytes: Vec<u8> = match cmd {
        'f' | 'r' | 'R' => (double(text)? as f32).to_le_bytes().to_vec(),
        'd' | 'q' | 'Q' => double(text)?.to_le_bytes().to_vec(),
        _ => {
            let value =
                crate::cmd_string::parse_big(text.trim_matches(is_space)).ok_or_else(|| {
                    format!(
                        "expected integer but got {}",
                        crate::runtime::named(text, 50)
                    )
                })?;
            truncated(&value, slot.len() * 8)
        }
    };
    if big_endian(cmd) {
        for (i, b) in bytes.iter().rev().enumerate() {
            slot[i] = *b;
        }
    } else {
        slot.copy_from_slice(&bytes);
    }
    Ok(())
}

/// `value` reduced into `bits` bits, little-endian — the modular truncation
/// every integer field performs, at the arbitrary precision Tcl 9's integers
/// have (`binary format c 99999999999999999999` is that number's low byte, not
/// a refusal).
fn truncated(value: &BigInt, bits: usize) -> Vec<u8> {
    let mask = BigInt::from((BigUint::from(1u8) << bits) - BigUint::from(1u8));
    let pattern = (value & &mask).magnitude().to_bytes_le();
    let mut out = vec![0u8; bits / 8];
    for (i, b) in pattern.iter().take(bits / 8).enumerate() {
        out[i] = *b;
    }
    out
}

/// A floating-point field's value, in the same wording.
fn double(text: &str) -> Result<f64, String> {
    crate::cmd_string::parse_double(text).ok_or_else(|| {
        format!(
            "expected floating-point number but got {}",
            crate::runtime::named(text, 50)
        )
    })
}

/// The low byte of every character, which is what an `a` or `A` field stores.
fn low_bytes(text: &str) -> Vec<u8> {
    text.chars().map(|c| (u32::from(c) & 0xff) as u8).collect()
}

fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r')
}

/// A value as an error message spells it: fifty bytes at most, on a character
/// boundary.
fn cut(text: &str) -> &str {
    let mut end = text.len().min(50);
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

// ── binary scan ──────────────────────────────────────────────────────────

/// What one scan produced: a slot per field that consumes a variable, unfilled
/// where the data ran out.
struct Scanned {
    values: Vec<Option<String>>,
}

/// `binary scan value formatString ?varName ...?`.
fn scan(data: &[u8], fmt: &str, vars: usize) -> Result<Scanned, String> {
    let f: Vec<char> = fmt.chars().collect();

    // The argument check runs over the whole format first, as
    // `ValidateFormat`'s does: too few variables is an error before any of the
    // data is read, and a bad type character is one too.
    let mut wanted = 0usize;
    let mut i = 0usize;
    while let Some(spec) = next_spec(&f, &mut i) {
        match spec.cmd {
            'a' | 'A' | 'C' | 'b' | 'B' | 'h' | 'H' => wanted += 1,
            'x' | 'X' | '@' => {
                if spec.cmd == '@' && spec.count == Count::One {
                    return Err("missing count for \"@\" field specifier".to_string());
                }
            }
            cmd if item_size(cmd).is_some() => wanted += 1,
            _ => return Err(bad_field(&spec)),
        }
    }
    if wanted > vars {
        return Err("not enough arguments for all format specifiers".to_string());
    }

    let mut values = vec![None; wanted];
    let mut slot = 0usize;
    let mut at = 0usize;
    let mut i = 0usize;
    while let Some(spec) = next_spec(&f, &mut i) {
        let left = data.len() - at.min(data.len());
        match spec.cmd {
            'a' | 'A' | 'C' => {
                let count = match spec.count {
                    Count::All => left,
                    Count::One => 1,
                    Count::Num(n) => n,
                };
                if count > left {
                    break;
                }
                let mut taken = &data[at..at + count];
                if spec.cmd == 'A' {
                    while taken.last().is_some_and(|b| *b == b' ' || *b == 0) {
                        taken = &taken[..taken.len() - 1];
                    }
                } else if spec.cmd == 'C' {
                    if let Some(end) = taken.iter().position(|b| *b == 0) {
                        taken = &taken[..end];
                    }
                    while taken.last().is_some_and(|b| *b == b' ') {
                        taken = &taken[..taken.len() - 1];
                    }
                }
                values[slot] = Some(from_bytes(taken));
                slot += 1;
                at += count;
            }
            'b' | 'B' => {
                let count = match spec.count {
                    Count::All => left * 8,
                    Count::One => 1,
                    Count::Num(n) => n,
                };
                if count.div_ceil(8) > left {
                    break;
                }
                let mut text = String::with_capacity(count);
                for bit in 0..count {
                    let byte = data[at + bit / 8];
                    let taken = if spec.cmd == 'b' {
                        byte >> (bit % 8) & 1
                    } else {
                        byte >> (7 - bit % 8) & 1
                    };
                    text.push(if taken == 1 { '1' } else { '0' });
                }
                values[slot] = Some(text);
                slot += 1;
                at += count.div_ceil(8);
            }
            'h' | 'H' => {
                let count = match spec.count {
                    Count::All => left * 2,
                    Count::One => 1,
                    Count::Num(n) => n,
                };
                if count.div_ceil(2) > left {
                    break;
                }
                let mut text = String::with_capacity(count);
                for half in 0..count {
                    let byte = data[at + half / 2];
                    // As in `format`: `H` reads the high half of each byte
                    // first and `h` the low half.
                    let high = (spec.cmd == 'H') == (half % 2 == 0);
                    let digit = if high { byte >> 4 } else { byte & 0xf };
                    text.push(char::from_digit(u32::from(digit), 16).expect("a nibble"));
                }
                values[slot] = Some(text);
                slot += 1;
                at += count.div_ceil(2);
            }
            'x' => {
                let count = match spec.count {
                    Count::All => left,
                    Count::One => 1,
                    Count::Num(n) => n,
                };
                at = (at + count).min(data.len());
            }
            'X' => {
                let count = match spec.count {
                    Count::All => at,
                    Count::One => 1,
                    Count::Num(n) => n,
                };
                at = at.saturating_sub(count);
            }
            '@' => {
                let count = match spec.count {
                    Count::All => data.len(),
                    Count::One => unreachable!("refused by the validation pass"),
                    Count::Num(n) => n,
                };
                at = count.min(data.len());
            }
            cmd => {
                let size = item_size(cmd).expect("the validation pass rejected every other type");
                let count = match spec.count {
                    Count::All => left / size,
                    Count::One => 1,
                    Count::Num(n) => n,
                };
                if count * size > left {
                    break;
                }
                let mut items = Vec::with_capacity(count);
                for item in 0..count {
                    let from = at + item * size;
                    items.push(read_number(&data[from..from + size], cmd, spec.unsigned));
                }
                values[slot] = Some(list::join(&items));
                slot += 1;
                at += count * size;
            }
        }
    }
    Ok(Scanned { values })
}

/// One number read out of `slot`, whose length is the field's item size.
fn read_number(slot: &[u8], cmd: char, unsigned: bool) -> String {
    let mut bytes = slot.to_vec();
    if big_endian(cmd) {
        bytes.reverse();
    }
    match cmd {
        'f' | 'r' | 'R' => {
            let raw = u32::from_le_bytes(bytes.try_into().expect("four bytes"));
            format_double(f64::from(f32::from_bits(raw)))
        }
        'd' | 'q' | 'Q' => {
            let raw = u64::from_le_bytes(bytes.try_into().expect("eight bytes"));
            format_double(f64::from_bits(raw))
        }
        _ => {
            let mut value: u128 = 0;
            for (i, b) in bytes.iter().enumerate() {
                value |= u128::from(*b) << (8 * i);
            }
            let bits = bytes.len() * 8;
            if !unsigned && value >> (bits - 1) & 1 == 1 {
                // Sign extension, which is what makes `binary scan` of a byte
                // with its top bit set answer a negative number.
                (value as i128 - (1i128 << bits)).to_string()
            } else {
                value.to_string()
            }
        }
    }
}

// ── binary encode and binary decode ──────────────────────────────────────

/// The six-bit alphabet of base64, in value order.
const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn encode(codec: usize, data: &[u8], opts: &Options) -> Result<String, String> {
    match CODECS[codec] {
        "hex" => Ok(data.iter().map(|b| format!("{b:02x}")).collect()),
        "base64" => Ok(wrap(&base64_digits(data), opts)),
        _ => Ok(uuencode(data, opts)),
    }
}

fn base64_digits(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for group in data.chunks(3) {
        let mut bits = 0u32;
        for (i, b) in group.iter().enumerate() {
            bits |= u32::from(*b) << (16 - 8 * i);
        }
        for i in 0..4 {
            if i <= group.len() {
                out.push(char::from(BASE64[(bits >> (18 - 6 * i) & 0x3f) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// `-maxlen` and `-wrapchar` applied to an encoded body.
fn wrap(text: &str, opts: &Options) -> String {
    let Some(max) = opts.maxlen.filter(|n| *n > 0) else {
        return text.to_string();
    };
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    for (i, line) in chars.chunks(max).enumerate() {
        if i > 0 {
            out.push_str(&opts.wrapchar);
        }
        out.extend(line);
    }
    out
}

/// uuencode: a length character, then four characters for every three bytes,
/// each holding six bits raised by `0x20`. A short final group emits only the
/// characters its bytes reach into.
fn uuencode(data: &[u8], opts: &Options) -> String {
    let max = opts.maxlen.unwrap_or(61).clamp(5, 85);
    // Each line carries the length character plus four per three bytes.
    let per_line = ((max - 1) / 4) * 3;
    let mut out = String::new();
    for line in data.chunks(per_line.max(1)) {
        out.push(uu_digit(line.len() as u8));
        for group in line.chunks(3) {
            let mut bits = 0u32;
            for (i, b) in group.iter().enumerate() {
                bits |= u32::from(*b) << (16 - 8 * i);
            }
            for i in 0..=group.len() {
                out.push(uu_digit((bits >> (18 - 6 * i) & 0x3f) as u8));
            }
        }
        out.push_str(&opts.wrapchar);
    }
    out
}

/// uuencode's six-bit alphabet: `0x20 +` the value, with zero written as a
/// backquote rather than a space so a line's trailing padding survives.
fn uu_digit(value: u8) -> char {
    if value == 0 {
        '`'
    } else {
        char::from(value + 0x20)
    }
}

fn decode(codec: usize, text: &str, strict: bool) -> Result<Vec<u8>, String> {
    match CODECS[codec] {
        "hex" => decode_hex(text, strict),
        "base64" => decode_base64(text, strict),
        _ => decode_uuencode(text, strict),
    }
}

fn decode_hex(text: &str, strict: bool) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(text.len() / 2);
    let mut pending: Option<u8> = None;
    for (at, ch) in text.chars().enumerate() {
        if !strict && ch.is_whitespace() {
            continue;
        }
        let Some(digit) = ch.to_digit(16) else {
            return Err(format!(
                "invalid hexadecimal digit \"{ch}\" (U+{:06X}) at position {at}",
                u32::from(ch)
            ));
        };
        match pending.take() {
            Some(high) => out.push(high << 4 | digit as u8),
            None => pending = Some(digit as u8),
        }
    }
    // A trailing half-byte is simply dropped: `binary decode hex 6` is the
    // empty string in tclsh, with or without `-strict`.
    Ok(out)
}

/// base64, with the reference implementation's `-strict` rules: padding is
/// legal only where a group can actually end, nothing may follow it, and a
/// group of one leftover character — which encodes no byte at all — is refused.
/// Without `-strict` every character outside the alphabet is passed over, which
/// is what RFC 2045 asks a decoder to do.
fn decode_base64(text: &str, strict: bool) -> Result<Vec<u8>, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::with_capacity(chars.len() / 4 * 3);
    let mut bits = 0u32;
    let mut have = 0u32;
    let mut taken = 0usize;
    for (at, ch) in chars.iter().enumerate() {
        let ch = *ch;
        if ch == '=' {
            if strict {
                // A group holds four characters; padding can only stand for
                // the third or the fourth of them.
                if taken % 4 < 2 {
                    return Err(bad_base64(ch, at));
                }
                // One optional second `=`, and then the end. tclsh names the
                // position just past the first padding character when
                // anything else follows.
                if at + 1 != chars.len() && at + 2 != chars.len() {
                    return Err(bad_base64('=', at + 1));
                }
            }
            break;
        }
        let value = if ch.is_ascii() {
            BASE64.iter().position(|b| char::from(*b) == ch)
        } else {
            None
        };
        let Some(value) = value else {
            if !strict {
                continue;
            }
            return Err(bad_base64(ch, at));
        };
        taken += 1;
        bits = bits << 6 | value as u32;
        have += 6;
        if have >= 8 {
            have -= 8;
            out.push((bits >> have & 0xff) as u8);
        }
    }
    if strict && taken % 4 == 1 {
        // Six bits left over encode nothing, so the last character cannot be
        // part of any group.
        let at = chars.len() - 1;
        return Err(bad_base64(chars[at], at));
    }
    Ok(out)
}

fn bad_base64(ch: char, at: usize) -> String {
    format!(
        "invalid base64 character \"{ch}\" (U+{:06X}) at position {at}",
        u32::from(ch)
    )
}

/// uuencode. A line's first character says how many bytes it carries; a line
/// of zero ends the data.
///
/// `-strict` adds the reference implementation's own length check: a line that
/// a terminator follows must hold the four characters per three bytes the
/// encoding calls for, and `short uuencode data` is what it says otherwise —
/// which its *encoder* does not satisfy, so `binary decode uuencode -strict
/// [binary encode uuencode a]` is that refusal in tclsh 9.0.4 as well.
fn decode_uuencode(text: &str, strict: bool) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let (line, terminated) = match rest.find('\n') {
            Some(end) => {
                let line = &rest[..end];
                rest = &rest[end + 1..];
                (line, true)
            }
            None => {
                let line = rest;
                rest = "";
                (line, false)
            }
        };
        let chars: Vec<char> = line.chars().collect();
        let Some(first) = chars.first() else {
            // An empty line is passed over when the decoder is forgiving; with
            // `-strict` the terminator itself is read as the length character
            // and refused, which is what tclsh reports for a message that
            // starts with a blank line.
            if strict {
                uu_value('\n', true, 0)?;
            }
            continue;
        };
        // The length character is never masked, even when the rest of the line
        // is: `binary decode uuencode abc` names the `a` in tclsh whether or
        // not `-strict` was given.
        let want = usize::from(uu_value(*first, true, 0)?);
        if want == 0 {
            break;
        }
        // With `-strict` a character outside the alphabet is named before the
        // line's length is judged, so `binary decode uuencode -strict YWJj`
        // reports the `j` rather than `short uuencode data`.
        if strict {
            for (at, ch) in chars.iter().enumerate().skip(1) {
                uu_value(*ch, true, at)?;
            }
        }
        // How many characters the declared byte count needs. A line a
        // terminator follows is held to whole four-character groups; the last
        // line of a message, which nothing follows, only has to carry the
        // characters the bytes themselves reach into.
        let needed = if terminated {
            want.div_ceil(3) * 4
        } else {
            want + want.div_ceil(3)
        };
        if strict && chars.len() - 1 < needed {
            return Err("short uuencode data".to_string());
        }
        let mut wrote = 0usize;
        for (group, chunk) in chars[1..].chunks(4).enumerate() {
            let mut value = 0u32;
            for (i, ch) in chunk.iter().enumerate() {
                value |= u32::from(uu_value(*ch, strict, group * 4 + i + 1)?) << (18 - 6 * i);
            }
            for i in 0..3 {
                if wrote < want {
                    out.push((value >> (16 - 8 * i) & 0xff) as u8);
                    wrote += 1;
                }
            }
        }
    }
    Ok(out)
}

/// One uuencode character's six bits.
///
/// `-strict` accepts only the encoding's own alphabet, `0x20`–`0x5f`. Without
/// it the value is masked to six bits, which is what a decoder that has to
/// tolerate a re-flowed message does and what tclsh does here: `binary decode
/// uuencode YWJj` is bytes rather than a refusal, while the same input with
/// `-strict` names the `j`.
fn uu_value(ch: char, strict: bool, at: usize) -> Result<u8, String> {
    match ch {
        '`' | ' ' => Ok(0),
        c if ('!'..='_').contains(&c) => Ok(c as u8 - 0x20),
        c if strict => Err(format!(
            "invalid uuencode character \"{c}\" (U+{:06X}) at position {at}",
            u32::from(c)
        )),
        c if c.is_whitespace() => Ok(0),
        c if c.is_ascii() => Ok((c as u8).wrapping_sub(0x20) & 0x3f),
        c => Err(format!(
            "invalid uuencode character \"{c}\" (U+{:06X}) at position {at}",
            u32::from(c)
        )),
    }
}

/// The options `binary encode` and `binary decode` take.
struct Options {
    maxlen: Option<usize>,
    wrapchar: String,
    strict: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            maxlen: None,
            wrapchar: "\n".to_string(),
            strict: false,
        }
    }
}

/// The usage line one codec's `encode` or `decode` is reported with. `hex`
/// takes no options at all, which is why an option word given to it is a
/// `wrong # args` and not a `bad option`.
fn usage(verb: &str, codec: usize) -> String {
    match (verb, CODECS[codec]) {
        ("encode", "hex") => "binary encode hex data".to_string(),
        ("encode", name) => {
            format!("binary encode {name} ?-maxlen len? ?-wrapchar char? data")
        }
        (_, name) => format!("binary decode {name} ?options? data"),
    }
}

/// Read the `-option value` words that precede the data.
///
/// Each codec has its own table, and the diagnostics differ with it: an option
/// word handed to `binary encode hex` is a `wrong # args` because that form has
/// no option slot at all, while one handed to `binary decode` is a `bad
/// option`. `-maxlen`'s range is the codec's too — uuencode cannot express a
/// line outside 5 to 85 characters.
fn options(words: &[String], encoding: bool, codec: usize) -> Result<(Options, &String), String> {
    let verb = if encoding { "encode" } else { "decode" };
    let wrong = || format!("wrong # args: should be \"{}\"", usage(verb, codec));
    let takes_pairs = encoding && CODECS[codec] != "hex";
    let shape = if takes_pairs {
        // `-option value` pairs and then the data, so an even count is a pair
        // left half-written.
        words.len().is_multiple_of(2)
    } else if encoding {
        // `binary encode hex` has no option slot at all.
        words.len() != 1
    } else {
        // `binary decode` has exactly one: `?-strict? data`.
        words.len() > 2
    };
    if shape {
        return Err(wrong());
    }
    let Some((data, opts)) = words.split_last() else {
        return Err(wrong());
    };

    let mut out = Options::default();
    let mut i = 0;
    while i < opts.len() {
        if !encoding {
            if opts[i] != "-strict" {
                return Err(format!("bad option \"{}\": must be -strict", opts[i]));
            }
            out.strict = true;
            i += 1;
            continue;
        }
        match opts[i].as_str() {
            "-maxlen" => out.maxlen = Some(maxlen(&opts[i + 1], codec)?),
            "-wrapchar" => {
                if CODECS[codec] == "uuencode"
                    && !opts[i + 1]
                        .chars()
                        .all(|c| matches!(c, '\t' | '\u{b}' | '\u{c}' | '\r' | '\n'))
                {
                    return Err("invalid wrapchar; will defeat decoding".to_string());
                }
                out.wrapchar.clone_from(&opts[i + 1]);
            }
            other => {
                return Err(format!(
                    "bad option \"{other}\": must be -maxlen or -wrapchar"
                ))
            }
        }
        i += 2;
    }
    Ok((out, data))
}

/// `-maxlen`'s value, refused outside the range the codec can express.
fn maxlen(text: &str, codec: usize) -> Result<usize, String> {
    let value = list::wide(text)?;
    let ok = if CODECS[codec] == "uuencode" {
        (5..=85).contains(&value)
    } else {
        value >= 0
    };
    if !ok {
        return Err("line length out of range".to_string());
    }
    Ok(value as usize)
}

// ── running ──────────────────────────────────────────────────────────────

/// Whether an id belongs to this module's block.
pub(crate) fn is_op(id: u16) -> bool {
    (ext::BASE..ext::BASE + crate::compiler::ext::BLOCK).contains(&id)
}

pub(crate) fn extension(vm: &mut VM, id: u16, arg: u8) -> Result<(), String> {
    match id {
        ext::FORMAT => {
            let count = popped_count(vm);
            let mut args = Vec::with_capacity(count);
            for _ in 0..count {
                args.push(to_tcl_string(&vm.pop()));
            }
            args.reverse();
            let fmt = to_tcl_string(&vm.pop());
            let bytes = format(&fmt, &args)?;
            vm.push(Value::Str(Arc::new(from_bytes(&bytes))));
            Ok(())
        }
        ext::SCAN => {
            let count = popped_count(vm);
            let mut places = Vec::with_capacity(count);
            for _ in 0..count {
                let operand = vm.pop();
                let in_frame = to_tcl_string(&vm.pop()) == "1";
                let _name = vm.pop();
                places.push(place_at(&operand, in_frame)?);
            }
            places.reverse();
            let fmt = to_tcl_string(&vm.pop());
            let data = as_bytes(&to_tcl_string(&vm.pop()))?;

            let scanned = scan(&data, &fmt, count)?;
            let mut assigned = 0i64;
            for (place, value) in places.into_iter().zip(scanned.values) {
                let Some(value) = value else { continue };
                assigned += 1;
                if let Some(cell) = var_cell(vm, place) {
                    *cell = Value::Str(Arc::new(value));
                }
            }
            vm.push(Value::Int(assigned));
            Ok(())
        }
        ext::ENCODE | ext::DECODE => {
            let count = popped_count(vm);
            let mut words = Vec::with_capacity(count);
            for _ in 0..count {
                words.push(to_tcl_string(&vm.pop()));
            }
            words.reverse();
            let encoding = id == ext::ENCODE;
            let (opts, data) = options(&words, encoding, usize::from(arg))?;
            let answer = if encoding {
                encode(usize::from(arg), &as_bytes(data)?, &opts)?
            } else {
                from_bytes(&decode(usize::from(arg), data, opts.strict)?)
            };
            vm.push(Value::Str(Arc::new(answer)));
            Ok(())
        }
        other => Err(format!("unknown extension op {other}")),
    }
}

fn popped_count(vm: &mut VM) -> usize {
    to_tcl_string(&vm.pop())
        .parse()
        .expect("the count is emitted as an integer")
}
