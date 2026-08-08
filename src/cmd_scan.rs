//! `scan` — the inverse of `format`.
//!
//! Ported from `tclScan.c` at tclsh 9.0.4: `validate` is `ValidateFormat`,
//! `run` is the body of `Tcl_ScanObjCmd`, and `CharSet` is the same character
//! set `BuildCharSet` builds. The number parsing each conversion does is the
//! subset of `TclParseNumber` (`tclStrToD.c`) that `scan`'s flags reach, and it
//! keeps that function's shape: a state machine that remembers the last point
//! at which what it had read was already a number, and rewinds to it when the
//! next character is not part of one. That rule is why `scan 1e %f` is `1.0`
//! rather than a failure.
//!
//! The variables are written by the op itself rather than by stores the
//! compiler emits after it, because which of them are written is decided by how
//! far the scan got: `scan "1 2" "%d %d %d" a b c` sets `a` and `b`, answers 2,
//! and leaves `c` unset. There is no arrangement of stores that does that
//! without the op saying which ones ran.

use std::sync::Arc;

use fusevm::{Op, Value, VM};
use num_bigint::BigInt;

use crate::assoc::{target_of, Target};
use crate::compiler::{CompileError, Compiler};
use crate::list;
use crate::parser::Word;
use crate::runtime::{format_double, place_at, to_tcl_string, var_cell};

/// The ids this command owns, inside [`crate::cmd_string`]'s block: `scan` is
/// `format`'s inverse and shares its range rather than opening a new one.
pub mod ext {
    pub use crate::compiler::ext::STRING_BASE as BASE;
    /// `[string, format, (name, in_frame, place) …, count]` → the number of
    /// conversions assigned, or the converted values as a list when `count` is
    /// zero.
    pub const SCAN: u16 = BASE + 40;
}

// ── compiling ────────────────────────────────────────────────────────────

/// `scan string format ?varName ...?`.
pub(crate) fn compile(c: &mut Compiler, args: &[Word]) -> Result<(), CompileError> {
    if args.len() < 2 {
        return c.error("wrong # args: should be \"scan string format ?varName ...?\"");
    }
    let vars = &args[2..];
    // Every name is resolved before anything is emitted, so a bad one is a
    // compile error rather than a half-run command — the same rule `lassign`
    // follows.
    let mut names = Vec::with_capacity(vars.len());
    for w in vars {
        match target_of(w) {
            Some(Target::Scalar(name)) => names.push(name),
            _ => return c.error("\"scan\" into an array element is not supported yet"),
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

// ── running ──────────────────────────────────────────────────────────────

pub(crate) fn extension(vm: &mut VM) -> Result<(), String> {
    let count = to_tcl_string(&vm.pop())
        .parse::<usize>()
        .expect("the variable count is emitted as an integer");
    let mut places = Vec::with_capacity(count);
    for _ in 0..count {
        let operand = vm.pop();
        let in_frame = to_tcl_string(&vm.pop()) == "1";
        let _name = vm.pop();
        places.push(place_at(&operand, in_frame)?);
    }
    places.reverse();
    let format = to_tcl_string(&vm.pop());
    let subject = to_tcl_string(&vm.pop());

    let total = validate(&format, count)?;
    let scanned = run(&subject, &format, total)?;

    if count == 0 {
        // The inline form answers a list, with an empty element wherever a
        // conversion never ran. Nothing matched at all is the empty list.
        let values: Vec<String> = if scanned.underflow && scanned.conversions == 0 {
            Vec::new()
        } else {
            scanned
                .values
                .into_iter()
                .map(Option::unwrap_or_default)
                .collect()
        };
        vm.push(Value::Str(Arc::new(list::join(&values))));
        return Ok(());
    }

    let mut assigned = 0i64;
    for (place, value) in places.into_iter().zip(scanned.values) {
        let Some(value) = value else { continue };
        assigned += 1;
        if let Some(cell) = var_cell(vm, place) {
            *cell = Value::Str(Arc::new(value));
        }
    }
    // Running out of input before the first conversion is -1, which is how a
    // caller tells "nothing there" from "nothing matched".
    vm.push(Value::Int(
        if scanned.underflow && scanned.conversions == 0 {
            -1
        } else {
            assigned
        },
    ));
    Ok(())
}

/// What one scan produced: a slot per conversion specifier, unfilled where the
/// scan stopped before reaching it.
struct Scanned {
    values: Vec<Option<String>>,
    conversions: usize,
    underflow: bool,
}

/// How wide the value a conversion produces is, from the size modifier.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Size {
    /// No modifier: the value is an `int`, saturating at its ends.
    Int,
    /// `l`, `j`, `q`, `z`, `t`: a wide integer, saturating at its ends.
    Wide,
    /// `L`, `ll`: arbitrary precision, which saturates nowhere.
    Big,
}

/// Which spellings of an integer a conversion accepts, following the
/// `TCL_PARSE_*_ONLY` flags `scan` passes to `TclParseNumber`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Radix {
    /// `%d` and `%u`: decimal digits only, and a leading zero is just a digit.
    Decimal,
    /// `%i`: a leading `0x` is hexadecimal and any other leading zero is octal.
    Prefixed,
    /// `%o`: octal digits, with no `0o` prefix — a leading zero is a digit, and
    /// the `o` after it ends the number.
    Octal,
    /// `%x`, `%X`: hexadecimal digits, with an optional `0x` prefix.
    Hex,
    /// `%b`: binary digits, with an optional `0b` prefix.
    Binary,
}

/// One conversion specifier, as the format string spells it.
struct Spec {
    suppress: bool,
    /// Which result slot it fills, from `%n$` or from its position.
    index: usize,
    /// `0` when none was written, which every conversion reads as unbounded.
    width: usize,
    size: Size,
    kind: char,
    /// The character set of a `%[...]` conversion.
    set: Option<CharSet>,
}

/// `ValidateFormat`: check the format string and answer how many result slots
/// it needs. With variables given, every one of them must be assigned exactly
/// once, which is what makes a mismatch an error before anything is scanned.
fn validate(format: &str, num_vars: usize) -> Result<usize, String> {
    let f: Vec<char> = format.chars().collect();
    let mut i = 0;
    let mut obj_index = 0usize;
    let mut got_xpg = false;
    let mut got_sequential = false;
    let mut xpg_size = 0usize;
    let mut assigned: Vec<usize> = vec![0; num_vars];

    while i < f.len() {
        if f[i] != '%' {
            i += 1;
            continue;
        }
        i += 1;
        if i < f.len() && f[i] == '%' {
            i += 1;
            continue;
        }
        let mut suppress = false;
        let mut has_width = false;
        if i < f.len() && f[i] == '*' {
            suppress = true;
            i += 1;
        } else if let Some((value, end)) = digits_at(&f, i) {
            // `%n$` picks a slot; a run of digits not followed by `$` is a
            // width, and is left for the width step below.
            if f.get(end) == Some(&'$') {
                i = end + 1;
                got_xpg = true;
                if got_sequential {
                    return Err("cannot mix \"%\" and \"%n$\" conversion specifiers".to_string());
                }
                if value == 0 || value >= i64::from(i32::MAX) as u64 {
                    return Err(bad_index(got_xpg));
                }
                obj_index = value as usize - 1;
                if num_vars > 0 && obj_index >= num_vars {
                    return Err(bad_index(got_xpg));
                }
                if num_vars == 0 {
                    xpg_size = xpg_size.max(value as usize);
                }
            } else {
                got_sequential = true;
                if got_xpg {
                    return Err("cannot mix \"%\" and \"%n$\" conversion specifiers".to_string());
                }
            }
        } else {
            got_sequential = true;
            if got_xpg {
                return Err("cannot mix \"%\" and \"%n$\" conversion specifiers".to_string());
            }
        }

        if let Some((width, end)) = digits_at(&f, i) {
            if width >= i64::MAX as u64 {
                return Err(format!(
                    "specified field width {width} exceeds limit {}.",
                    i64::MAX - 1
                ));
            }
            has_width = true;
            i = end;
        }

        let size = read_size(&f, &mut i);
        if !suppress && num_vars > 0 && obj_index >= num_vars {
            return Err(bad_index(got_xpg));
        }

        // A format that ends in a bare `%` reads its terminator as the
        // conversion character, and the diagnostic carries that NUL — the
        // reference implementation prints one byte of 0 between the quotes.
        let ch = f.get(i).copied().unwrap_or('\0');
        i += 1;
        match ch {
            'c' if has_width => {
                return Err("field width may not be specified in %c conversion".to_string())
            }
            'c' | 'n' | 's' if size != Size::Int => {
                return Err(format!(
                    "field size modifier may not be specified in %{ch} conversion"
                ))
            }
            'c' | 'n' | 's' | 'd' | 'e' | 'E' | 'f' | 'g' | 'G' | 'i' | 'o' | 'x' | 'X' | 'b'
            | 'u' => {}
            '[' => {
                if size != Size::Int {
                    return Err(format!(
                        "field size modifier may not be specified in %{ch} conversion"
                    ));
                }
                i = skip_char_set(&f, i).ok_or("unmatched [ in format string")?;
            }
            other => return Err(format!("bad scan conversion character \"{other}\"")),
        }

        if !suppress {
            if obj_index >= assigned.len() {
                assigned.resize(obj_index + 1, 0);
            }
            assigned[obj_index] += 1;
            obj_index += 1;
        }
    }

    let total = if num_vars == 0 {
        if xpg_size > 0 {
            xpg_size
        } else {
            obj_index
        }
    } else {
        num_vars
    };
    for slot in assigned.iter().take(total) {
        if *slot > 1 {
            return Err(
                "variable is assigned by multiple \"%n$\" conversion specifiers".to_string(),
            );
        }
        if xpg_size == 0 && *slot == 0 {
            return Err("variable is not assigned by any conversion specifiers".to_string());
        }
    }
    Ok(total)
}

fn bad_index(got_xpg: bool) -> String {
    if got_xpg {
        "\"%n$\" argument index out of range".to_string()
    } else {
        "different numbers of variable names and field specifiers".to_string()
    }
}

/// The run of decimal digits at `at`, with where it ends. `None` when there is
/// none, which is how both the `%n$` check and the width check ask.
fn digits_at(f: &[char], at: usize) -> Option<(u64, usize)> {
    let mut end = at;
    while f.get(end).is_some_and(char::is_ascii_digit) {
        end += 1;
    }
    if end == at {
        return None;
    }
    // Saturating, because the only thing either caller does with a value this
    // large is refuse it.
    let value = f[at..end]
        .iter()
        .collect::<String>()
        .parse::<u64>()
        .unwrap_or(u64::MAX);
    Some((value, end))
}

/// The size modifier at `at`, advancing past it.
fn read_size(f: &[char], at: &mut usize) -> Size {
    match f.get(*at) {
        // `z` and `t` are pointer-width, which is wide on every target this
        // frontend builds for.
        Some('z' | 't' | 'j' | 'q') => {
            *at += 1;
            Size::Wide
        }
        Some('L') => {
            *at += 1;
            Size::Big
        }
        Some('l') => {
            *at += 1;
            if f.get(*at) == Some(&'l') {
                *at += 1;
                Size::Big
            } else {
                Size::Wide
            }
        }
        Some('h') => {
            *at += 1;
            Size::Int
        }
        _ => Size::Int,
    }
}

/// Where the `]` closing a `%[...]` set is, one past it. The first `]` may be a
/// member of the set rather than its end, and so may the one after a leading
/// `^`.
fn skip_char_set(f: &[char], at: usize) -> Option<usize> {
    let mut i = at;
    if f.get(i) == Some(&'^') {
        i += 1;
    }
    if f.get(i) == Some(&']') {
        i += 1;
    }
    while f.get(i) != Some(&']') {
        f.get(i)?;
        i += 1;
    }
    Some(i + 1)
}

/// The character set of a `%[...]` conversion: loose characters and ranges,
/// with `^` meaning everything else.
struct CharSet {
    exclude: bool,
    chars: Vec<char>,
    ranges: Vec<(char, char)>,
}

impl CharSet {
    /// `BuildCharSet`: read the set at `at`, leaving `at` one past its `]`.
    fn build(f: &[char], at: &mut usize) -> CharSet {
        let mut set = CharSet {
            exclude: false,
            chars: Vec::new(),
            ranges: Vec::new(),
        };
        if f.get(*at) == Some(&'^') {
            set.exclude = true;
            *at += 1;
        }
        // Whether the set holds any range at all is decided before it is read,
        // because a `-` in a set that has none is a literal `-`.
        let has_range = {
            let mut i = *at;
            if f.get(i) == Some(&']') {
                i += 1;
            }
            let mut found = false;
            while let Some(&c) = f.get(i) {
                if c == ']' {
                    break;
                }
                if c == '-' {
                    found = true;
                }
                i += 1;
            }
            found
        };

        let mut start = *f.get(*at).unwrap_or(&']');
        if start == ']' || start == '-' {
            set.chars.push(start);
            *at += 1;
        }
        while let Some(&ch) = f.get(*at) {
            if ch == ']' {
                break;
            }
            if f.get(*at + 1) == Some(&'-') {
                // The first character of a range: held back until the `-` and
                // the character after it are read.
                start = ch;
            } else if ch == '-' {
                if f.get(*at + 1) == Some(&']') || !has_range {
                    set.chars.push(start);
                    set.chars.push(ch);
                } else {
                    *at += 1;
                    let end = *f.get(*at).unwrap_or(&']');
                    set.ranges.push(if start < end {
                        (start, end)
                    } else {
                        (end, start)
                    });
                }
            } else {
                set.chars.push(ch);
            }
            *at += 1;
        }
        *at += 1;
        set
    }

    fn holds(&self, c: char) -> bool {
        let found = self.chars.contains(&c)
            || self
                .ranges
                .iter()
                .any(|&(start, end)| start <= c && c <= end);
        found != self.exclude
    }
}

/// The body of `Tcl_ScanObjCmd`: walk the format and the subject together,
/// filling a slot per conversion until one of them runs out or a conversion
/// fails to match.
fn run(subject: &str, format: &str, total: usize) -> Result<Scanned, String> {
    let s: Vec<char> = subject.chars().collect();
    let f: Vec<char> = format.chars().collect();
    let mut values: Vec<Option<String>> = (0..total).map(|_| None).collect();
    let mut conversions = 0usize;
    let mut underflow = false;
    let mut at = 0usize; // where we are in the subject
    let mut i = 0usize; // where we are in the format
    let mut next_slot = 0usize;

    'format: while i < f.len() {
        let ch = f[i];
        i += 1;

        // Whitespace in the format skips whitespace in the subject, and running
        // out of subject there ends the scan without counting as underflow.
        if ch.is_whitespace() {
            while s.get(at).is_some_and(|c| c.is_whitespace()) {
                at += 1;
            }
            continue;
        }

        let mut literal = ch;
        if ch == '%' {
            let Some(&next) = f.get(i) else {
                break;
            };
            i += 1;
            if next != '%' {
                let spec = read_spec(&f, &mut i, &mut next_slot);
                match convert(&s, &mut at, &spec, &mut values) {
                    Outcome::Converted => {
                        conversions += 1;
                        continue;
                    }
                    Outcome::Stopped => break 'format,
                    Outcome::Underflow => {
                        underflow = true;
                        break 'format;
                    }
                }
            }
            literal = next;
        }

        // A literal character in the format must be the next one in the
        // subject; running out of subject there is underflow.
        let Some(&got) = s.get(at) else {
            underflow = true;
            break;
        };
        at += 1;
        if literal != got {
            break;
        }
    }

    Ok(Scanned {
        values,
        conversions,
        underflow,
    })
}

/// Read one conversion specifier, `%` and the character after it already
/// consumed — `i` points at that character.
fn read_spec(f: &[char], i: &mut usize, next_slot: &mut usize) -> Spec {
    let mut spec = Spec {
        suppress: false,
        index: *next_slot,
        width: 0,
        size: Size::Int,
        kind: '\0',
        set: None,
    };
    *i -= 1;
    if f.get(*i) == Some(&'*') {
        spec.suppress = true;
        *i += 1;
    } else if let Some((value, end)) = digits_at(f, *i) {
        if f.get(end) == Some(&'$') {
            *i = end + 1;
            spec.index = value as usize - 1;
        }
    }
    if let Some((width, end)) = digits_at(f, *i) {
        spec.width = width as usize;
        *i = end;
    }
    spec.size = read_size(f, i);
    spec.kind = *f.get(*i).unwrap_or(&'\0');
    *i += 1;
    if spec.kind == '[' {
        spec.set = Some(CharSet::build(f, i));
    }
    if !spec.suppress {
        *next_slot = spec.index + 1;
    }
    spec
}

/// What one conversion did.
enum Outcome {
    Converted,
    /// It did not match, and the scan ends where it is.
    Stopped,
    /// The subject ran out before it could match.
    Underflow,
}

fn convert(s: &[char], at: &mut usize, spec: &Spec, values: &mut [Option<String>]) -> Outcome {
    let mut store = |text: String| {
        if !spec.suppress {
            if let Some(slot) = values.get_mut(spec.index) {
                *slot = Some(text);
            }
        }
    };

    // `%n` reports how far the scan has come and reads nothing, so it is
    // answered before the end-of-subject test below.
    if spec.kind == 'n' {
        store(at.to_string());
        return Outcome::Converted;
    }
    if *at >= s.len() {
        return Outcome::Underflow;
    }
    // Every conversion but `%c` and `%[` skips leading whitespace first.
    if !matches!(spec.kind, 'c' | '[') {
        while s.get(*at).is_some_and(|c| c.is_whitespace()) {
            *at += 1;
        }
        if *at >= s.len() {
            return Outcome::Underflow;
        }
    }

    let width = if spec.width == 0 {
        usize::MAX
    } else {
        spec.width
    };

    match spec.kind {
        's' => {
            let start = *at;
            while *at < s.len() && !s[*at].is_whitespace() && *at - start < width {
                *at += 1;
            }
            store(s[start..*at].iter().collect());
            Outcome::Converted
        }
        '[' => {
            let set = spec.set.as_ref().expect("a set conversion carries one");
            let start = *at;
            while *at < s.len() && set.holds(s[*at]) && *at - start < width {
                *at += 1;
            }
            if *at == start {
                return Outcome::Stopped;
            }
            store(s[start..*at].iter().collect());
            Outcome::Converted
        }
        'c' => {
            let c = s[*at];
            *at += 1;
            store((c as u32).to_string());
            Outcome::Converted
        }
        'f' | 'e' | 'E' | 'g' | 'G' => match parse_double(s, *at, width) {
            // A NaN parses but does not convert: the reference implementation
            // reads the value back out through `Tcl_GetDoubleFromObj`, which
            // refuses one, and stops the scan where it is without counting the
            // conversion or advancing.
            Ok((value, _)) if value.is_nan() => Outcome::Stopped,
            Ok((value, end)) => {
                *at = end;
                store(format_double(value));
                Outcome::Converted
            }
            Err(reached) => underflow_or_stop(s, reached, *at, spec.width),
        },
        _ => {
            let radix = match spec.kind {
                'i' => Radix::Prefixed,
                'o' => Radix::Octal,
                'x' | 'X' => Radix::Hex,
                'b' => Radix::Binary,
                _ => Radix::Decimal,
            };
            match parse_integer(s, *at, width, radix) {
                Ok((value, end)) => {
                    *at = end;
                    store(narrow(value, spec.size, spec.kind == 'u'));
                    Outcome::Converted
                }
                Err(reached) => underflow_or_stop(s, reached, *at, spec.width),
            }
        }
    }
}

/// A number that did not parse is underflow when the parser ran off the end of
/// the subject inside it — with a width, when it reached the width instead —
/// and an ordinary stop otherwise. That is the difference between `scan + %d`,
/// which is -1, and `scan +x %d`, which is 0: both fail, but only the first
/// failed for want of more input.
fn underflow_or_stop(s: &[char], reached: usize, at: usize, width: usize) -> Outcome {
    let ran_out = if width == 0 {
        reached >= s.len()
    } else {
        reached == at + width
    };
    if ran_out {
        Outcome::Underflow
    } else {
        Outcome::Stopped
    }
}

/// Fit a scanned integer into the width its size modifier asked for, saturating
/// as the reference implementation's `TclGetIntFromObj` failure path does.
fn narrow(value: BigInt, size: Size, unsigned: bool) -> String {
    if size == Size::Big {
        return value.to_string();
    }
    let fitted = match size {
        // `TclGetIntFromObj` takes anything from `INT_MIN` to `UINT_MAX` and
        // casts it, so `scan 4294967295 %d` is -1 rather than a saturation;
        // only what is outside *that* range saturates, by its sign.
        Size::Int => match i64::try_from(&value) {
            Ok(w) if (i64::from(i32::MIN)..=i64::from(u32::MAX)).contains(&w) => {
                i64::from(w as i32)
            }
            _ => {
                if value.sign() == num_bigint::Sign::Minus {
                    i64::from(i32::MIN)
                } else {
                    i64::from(i32::MAX)
                }
            }
        },
        // A wide saturates outright: `scan 18446744073709551615 %ld` is
        // `WIDE_MAX`, not -1.
        _ => i64::try_from(&value).unwrap_or({
            if value.sign() == num_bigint::Sign::Minus {
                i64::MIN
            } else {
                i64::MAX
            }
        }),
    };
    if unsigned && fitted < 0 {
        (fitted as u64).to_string()
    } else {
        fitted.to_string()
    }
}

/// The integer states of `TclParseNumber`, for the flags `scan` passes it. What
/// is returned is the value and where it ended; the error is how far the parser
/// had got, which is what says whether the failure was for want of more input.
fn parse_integer(
    s: &[char],
    at: usize,
    width: usize,
    radix: Radix,
) -> Result<(BigInt, usize), usize> {
    let limit = s.len().min(at.saturating_add(width));
    let peek = |k: usize| if k < limit { s.get(k).copied() } else { None };

    let mut i = at;
    let mut negative = false;
    match peek(i) {
        Some('+') => i += 1,
        Some('-') => {
            negative = true;
            i += 1;
        }
        _ => {}
    }

    // The base the digits are in, and where they start. A leading zero is the
    // interesting case: it is already a complete number, so a prefix that turns
    // out not to be one leaves that zero behind rather than failing — which is
    // what the reference parser's accept point holds while it looks ahead.
    let zero_led = peek(i) == Some('0');
    let (base, start) = match radix {
        Radix::Decimal => (10, i),
        Radix::Octal => (8, i),
        Radix::Hex if zero_led && matches!(peek(i + 1), Some('x' | 'X')) => (16, i + 2),
        Radix::Hex => (16, i),
        Radix::Binary if zero_led && matches!(peek(i + 1), Some('b' | 'B')) => (2, i + 2),
        Radix::Binary => (2, i),
        // `%i` reads `0x` as hexadecimal and any other leading zero as the
        // start of an octal number; there is no `0o` prefix on this path.
        Radix::Prefixed if zero_led && matches!(peek(i + 1), Some('x' | 'X')) => (16, i + 2),
        Radix::Prefixed if zero_led => (8, i),
        Radix::Prefixed => (10, i),
    };

    let mut end = start;
    while end < limit && s[end].is_digit(base) {
        end += 1;
    }
    if end == start {
        // Nothing after the prefix. The zero that introduced it stands alone
        // when there was one, and there is no number at all when there was not
        // — with the sign already read, which is what makes `scan + %d` a
        // failure for want of input rather than an ordinary mismatch.
        return if start > i {
            Ok((BigInt::from(0), i + 1))
        } else {
            Err(i)
        };
    }

    let digits: String = s[start..end].iter().collect();
    let magnitude = BigInt::parse_bytes(digits.as_bytes(), base).ok_or(i)?;
    Ok((if negative { -magnitude } else { magnitude }, end))
}

/// The floating-point states of `TclParseNumber`, for `%f`, `%e` and `%g`.
/// Decimal only, no whitespace, and the longest prefix that is a number wins:
/// `1e` reads as `1`, because the exponent was never completed.
fn parse_double(s: &[char], at: usize, width: usize) -> Result<(f64, usize), usize> {
    let limit = s.len().min(at.saturating_add(width));
    let mut i = at;
    if i < limit && matches!(s.get(i), Some('+' | '-')) {
        i += 1;
    }
    // How far the parser got, for the failure case: the sign, and the radix
    // point after it when there is one — the state the reference parser is in
    // after reading `+.` is one it can still complete a number from.
    let reached = if s.get(i) == Some(&'.') && i < limit {
        i + 1
    } else {
        i
    };

    // `Inf` and `NaN`, which the reference parser reaches from the same state
    // the digits are reached from.
    if let Some(end) = word_at(s, i, limit, "infinity").or_else(|| word_at(s, i, limit, "inf")) {
        let text: String = s[at..end].iter().collect();
        return Ok((
            if text.starts_with('-') {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            },
            end,
        ));
    }
    if let Some(end) = word_at(s, i, limit, "nan") {
        return Ok((f64::NAN, end));
    }

    let mut accepted = None;
    let mut digits = 0;
    while i < limit && s[i].is_ascii_digit() {
        i += 1;
        digits += 1;
    }
    if digits > 0 {
        accepted = Some(i);
    }
    if i < limit && s[i] == '.' {
        let mut j = i + 1;
        let mut fraction = 0;
        while j < limit && s[j].is_ascii_digit() {
            j += 1;
            fraction += 1;
        }
        if digits > 0 || fraction > 0 {
            i = j;
            accepted = Some(i);
        }
    }
    let Some(whole) = accepted else {
        return Err(reached);
    };
    if i < limit && matches!(s[i], 'e' | 'E') {
        let mut j = i + 1;
        if j < limit && matches!(s[j], '+' | '-') {
            j += 1;
        }
        let mut exponent = 0;
        while j < limit && s[j].is_ascii_digit() {
            j += 1;
            exponent += 1;
        }
        if exponent > 0 {
            accepted = Some(j);
        }
    }
    let end = accepted.unwrap_or(whole);
    let text: String = s[at..end].iter().collect();
    // The text is a decimal number by construction, so anything Rust refuses
    // here is a magnitude past `f64`, which reads as an infinity exactly as the
    // reference parser's overflow does.
    let mut value = text.parse::<f64>().unwrap_or(if text.starts_with('-') {
        f64::NEG_INFINITY
    } else {
        f64::INFINITY
    });
    // Text with no radix point and no exponent is an *integer* to the reference
    // parser, and reading an integer back as a double loses the sign of a zero:
    // `scan -0 %f` is `0.0` there, where `scan -0.0 %f` is `-0.0`.
    if value == 0.0 && !text.contains(['.', 'e', 'E']) {
        value = 0.0;
    }
    Ok((value, end))
}

/// Whether `word` (given in lower case) starts at `i`, case-insensitively, and
/// where it ends.
fn word_at(s: &[char], i: usize, limit: usize, word: &str) -> Option<usize> {
    let mut at = i;
    for want in word.chars() {
        if at >= limit || s[at].to_ascii_lowercase() != want {
            return None;
        }
        at += 1;
    }
    Some(at)
}
