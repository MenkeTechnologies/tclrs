//! The `clock` ensemble.
//!
//! What is here and what is refused, stated plainly, because a wrong
//! `clock format` is worse than one that says it cannot answer:
//!
//! * `seconds`, `milliseconds`, `microseconds`, `clicks` — complete.
//! * `format` — the whole token set of tclsh 9.0.4's `FmtSTokenMap`
//!   (`generic/tclClockFmt.c`) plus the locale-format expansions
//!   `::tcl::clock::LocalizeFormat` performs (`library/clock.tcl`), in the
//!   root locale's message catalogue.
//! * `scan` — the `-format` form only. tclsh's free-form parser is a
//!   several-thousand-line grammar over relative words, month names, ISO
//!   forms and time zone abbreviations; it is refused by name rather than
//!   approximated.
//! * `add` — every unit tclsh accepts, with the calendar arithmetic for
//!   months and years and the weekday walk for `weekdays`.
//! * Time zones — `-gmt`, a fixed numeric offset, and any zone with a `TZif`
//!   file, read with the same reader tclsh's `LoadZoneinfoFile` implements in
//!   Tcl. The default zone comes from `TZ` or `/etc/localtime`, as tclsh's
//!   does.
//!
//! Refused, each with its own message:
//!
//! * Any instant before the Gregorian changeover. tclsh reckons earlier dates
//!   in the Julian calendar, and *which* earlier dates depends on the locale —
//!   `GREGORIAN_CHANGE_DATE` is 2299161 for the root locale and 2361222 for
//!   `en`, which is what this machine's tclsh resolves to. The dates this
//!   module answers for are the ones every one of those settings agrees on.
//! * `-locale` naming anything but the root locale. The month and day names,
//!   the AM/PM words and the `%x`/`%X`/`%c` expansions all come from the
//!   locale, and tclsh reads them from its `msgs/` catalogue.
//! * A POSIX `TZ` *rule* string (`EST5EDT,M3.2.0,M11.1.0`) with no matching
//!   zone file, and the `%E`/`%O` locale-modified tokens.

use std::sync::Arc;

use fusevm::{Op, Value, VM};

use crate::compiler::{CompileError, Compiler};
use crate::parser::Word;
use crate::runtime::{tcl_str, to_tcl_string, Num};

/// Extension opcode ids owned by this module. One per subcommand; the inline
/// operand is the number of stack values the op consumes.
pub mod ext {
    pub use crate::compiler::ext::CLOCK_BASE as BASE;
    /// `[]` → the current time. `arg` selects the unit: 0 seconds,
    /// 1 milliseconds, 2 microseconds, 3 `clicks` (whose switch is on the
    /// stack).
    pub const NOW: u16 = BASE;
    /// `[value …]` → the formatted time, with the option words pushed in the
    /// order the script wrote them.
    pub const FORMAT: u16 = BASE + 1;
    /// `[value …]` → the instant the input names.
    pub const SCAN: u16 = BASE + 2;
    /// `[value …]` → the instant the offsets reach.
    pub const ADD: u16 = BASE + 3;
}

/// The command names this module claims, for the REPL's completion and for the
/// reference page.
pub const COMMANDS: &[&str] = &["clock"];

/// Every subcommand, in the order the interpreter lists them when it rejects
/// one.
pub const SUBCOMMANDS: &[&str] = &[
    "add",
    "clicks",
    "format",
    "microseconds",
    "milliseconds",
    "scan",
    "seconds",
];

// ── compiling ────────────────────────────────────────────────────────────

/// Lower `clock …`. Only the subcommand is resolved here; every option is a
/// value and travels to the handler, because `clock format $t {*}$opts` and
/// `clock format $t -format $f` have to reach the same code.
pub(crate) fn compile(c: &mut Compiler, args: &[Word]) -> Result<(), CompileError> {
    let Some(first) = args.first() else {
        return c.error("wrong # args: should be \"clock subcommand ?arg ...?\"");
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
        "seconds" | "milliseconds" | "microseconds" => {
            if !rest.is_empty() {
                return c.error(format!("wrong # args: should be \"clock {sub}\""));
            }
            let unit = match sub {
                "seconds" => 0,
                "milliseconds" => 1,
                _ => 2,
            };
            c.emit(Op::Extended(ext::NOW, unit), 1);
            Ok(())
        }
        "clicks" => {
            if rest.len() > 1 {
                return c.error("wrong # args: should be \"clock clicks ?-switch?\"");
            }
            // The switch always rides on the stack, empty when absent, so the
            // handler has one shape rather than two.
            match rest.first() {
                Some(w) => c.word(w)?,
                None => c.push_str(""),
            }
            c.emit(Op::Extended(ext::NOW, 3), 0);
            Ok(())
        }
        other => {
            let id = match other {
                "format" => ext::FORMAT,
                "scan" => ext::SCAN,
                _ => ext::ADD,
            };
            let Ok(argc) = u8::try_from(rest.len()) else {
                return c.error("too many arguments for one command");
            };
            for w in rest {
                c.word(w)?;
            }
            c.emit(Op::Extended(id, argc), 1 - rest.len() as i32);
            Ok(())
        }
    }
}

/// `Tcl_GetIndexFromObj`'s rule: an exact match wins, otherwise a prefix that
/// fits exactly one entry.
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

// ── the calendar ─────────────────────────────────────────────────────────

/// The first instant this module will reckon: 1752-09-14T00:00:00Z, which is
/// `GREGORIAN_CHANGE_DATE` 2361222 — the changeover the `en` locale uses, and
/// the latest one that still precedes every date a script is likely to ask
/// about. Before it the answer depends on the locale's calendar, and this
/// module has one calendar.
const EARLIEST: i64 = -6_857_222_400;

fn too_early() -> String {
    "clock: dates before the Gregorian changeover of 1752-09-14 are not supported yet".to_string()
}

/// A civil date and time, always proleptic Gregorian.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Civil {
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    /// Days since 1970-01-01, which every derived field is computed from.
    epoch_day: i64,
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

const MONTH_LENGTHS: [u32; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

fn month_length(year: i64, month: u32) -> u32 {
    if month == 2 && is_leap(year) {
        29
    } else {
        MONTH_LENGTHS[(month - 1) as usize]
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date — Hinnant's
/// `days_from_civil`, which is exact for every year an `i64` holds.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = month as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// The inverse, `civil_from_days`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Split a local time — seconds since the epoch with the zone's offset already
/// added — into its civil fields.
fn civil_of(local: i64) -> Civil {
    let days = local.div_euclid(86400);
    let secs = local.rem_euclid(86400);
    let (year, month, day) = civil_from_days(days);
    Civil {
        year,
        month,
        day,
        hour: (secs / 3600) as u32,
        minute: (secs / 60 % 60) as u32,
        second: (secs % 60) as u32,
        epoch_day: days,
    }
}

impl Civil {
    /// 1 for Monday through 7 for Sunday — `%u`'s numbering, which the other
    /// weekday tokens are derived from. 1970-01-01 was a Thursday.
    fn iso_weekday(&self) -> u32 {
        (self.epoch_day + 3).rem_euclid(7) as u32 + 1
    }

    /// Day of the year, 1-based.
    fn day_of_year(&self) -> i64 {
        self.epoch_day - days_from_civil(self.year, 1, 1) + 1
    }

    /// The ISO-8601 week-numbering year and week — `%G` and `%V`. The week
    /// holding the year's first Thursday is week 1.
    fn iso_week(&self) -> (i64, i64) {
        let thursday = self.epoch_day + 4 - self.iso_weekday() as i64;
        let (year, _, _) = civil_from_days(thursday);
        let week = (thursday - days_from_civil(year, 1, 1)) / 7 + 1;
        (year, week)
    }

    /// `%U` and `%W`: the week of the year counted from the first `start`
    /// weekday, where `start` is 0 for Sunday (`%U`) and 1 for Monday (`%W`).
    fn week_of_year(&self, start: u32) -> i64 {
        let weekday = self.iso_weekday() % 7; // 0 = Sunday
        let shifted = (weekday + 7 - start) % 7;
        (self.day_of_year() + 6 - shifted as i64) / 7
    }

    /// The Julian Day Number of the calendar day, `%J`'s value.
    fn julian_day(&self) -> i64 {
        self.epoch_day + 2440588
    }
}

// ── time zones ───────────────────────────────────────────────────────────

/// A resolved time zone: the offsets it applies and how it names itself.
struct Zone {
    /// Transitions, ascending by the UTC instant they take effect at, paired
    /// with the state in force from then on.
    transitions: Vec<(i64, State)>,
    /// The state before the first transition, and the whole zone when there
    /// are none.
    initial: State,
}

#[derive(Clone)]
struct State {
    offset: i32,
    abbreviation: String,
}

impl Zone {
    /// A zone with one fixed offset, which is what `-gmt 1` and a numeric
    /// `-timezone` produce.
    fn fixed(offset: i32, name: &str) -> Zone {
        Zone {
            transitions: Vec::new(),
            initial: State {
                offset,
                abbreviation: name.to_string(),
            },
        }
    }

    /// The state in force at a UTC instant.
    fn at(&self, utc: i64) -> &State {
        match self.transitions.partition_point(|(when, _)| *when <= utc) {
            0 => &self.initial,
            n => &self.transitions[n - 1].1,
        }
    }

    /// The state to use for a *local* time, which is what `clock scan` has:
    /// the offset is the thing being looked for, so it is guessed once from
    /// the local value and then checked. tclsh's `ConvertLocalToUTC` takes the
    /// same two steps.
    fn for_local(&self, local: i64) -> &State {
        let guess = self.at(local - self.at(local).offset as i64);
        self.at(local - guess.offset as i64)
    }
}

/// Read a `TZif` file — the format `tzfile(5)` describes and the one tclsh's
/// `LoadZoneinfoFile` parses in Tcl. Version 2 and 3 files carry a second,
/// 64-bit block after the 32-bit one; that block is the one read, since the
/// 32-bit block cannot name an instant past 2038.
fn parse_tzif(bytes: &[u8]) -> Option<Zone> {
    if bytes.len() < 44 || &bytes[..4] != b"TZif" {
        return None;
    }
    if bytes[4] >= b'2' {
        let second = block_length(bytes, 4)?;
        let rest = bytes.get(second..)?;
        if rest.len() >= 44 && &rest[..4] == b"TZif" {
            return read_block(rest, 8);
        }
    }
    read_block(bytes, 4)
}

/// How many bytes one whole block occupies, header included.
fn block_length(bytes: &[u8], width: usize) -> Option<usize> {
    let (isutc, isstd, leaps, times, types, chars) = counts_of(bytes)?;
    Some(44 + times * (width + 1) + types * 6 + chars + leaps * (width + 4) + isstd + isutc)
}

/// The six counts at the end of a `TZif` header.
fn counts_of(bytes: &[u8]) -> Option<(usize, usize, usize, usize, usize, usize)> {
    if bytes.len() < 44 {
        return None;
    }
    let at = |i: usize| -> usize {
        u32::from_be_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]) as usize
    };
    Some((at(20), at(24), at(28), at(32), at(36), at(40)))
}

/// One block's transitions and types, given the width of a transition time.
fn read_block(block: &[u8], width: usize) -> Option<Zone> {
    let (_, _, _, times, types, chars) = counts_of(block)?;
    if types == 0 {
        return None;
    }
    let body = block.get(44..)?;
    let mut at = 0usize;
    let mut when = Vec::with_capacity(times);
    for _ in 0..times {
        when.push(read_int(body, &mut at, width)?);
    }
    let mut index = Vec::with_capacity(times);
    for _ in 0..times {
        index.push(*body.get(at)? as usize);
        at += 1;
    }
    let mut infos = Vec::with_capacity(types);
    for _ in 0..types {
        let offset = read_int(body, &mut at, 4)? as i32;
        at += 1; // isdst, which nothing here reads
        let abbreviation = *body.get(at)? as usize;
        at += 1;
        infos.push((offset, abbreviation));
    }
    let names = body.get(at..at + chars)?;
    let state = |i: usize| -> State {
        let (offset, start) = infos[i];
        let start = start.min(names.len());
        let end = names[start..]
            .iter()
            .position(|b| *b == 0)
            .map_or(names.len(), |n| start + n);
        State {
            offset,
            abbreviation: String::from_utf8_lossy(&names[start..end]).into_owned(),
        }
    };
    let transitions: Vec<(i64, State)> = when
        .into_iter()
        .zip(index)
        .filter(|(_, i)| *i < infos.len())
        .map(|(w, i)| (w, state(i)))
        .collect();
    Some(Zone {
        // Before the first transition `tzfile(5)` directs the first
        // non-daylight type, and type 0 stands in when the file has none.
        initial: state(0),
        transitions,
    })
}

fn read_int(body: &[u8], at: &mut usize, width: usize) -> Option<i64> {
    let slice = body.get(*at..*at + width)?;
    *at += width;
    Some(match width {
        4 => i32::from_be_bytes(slice.try_into().ok()?) as i64,
        _ => i64::from_be_bytes(slice.try_into().ok()?),
    })
}

/// The directories tclsh's `LoadZoneinfoFile` searches, in its order.
const ZONE_DIRECTORIES: &[&str] = &[
    "/usr/share/zoneinfo",
    "/usr/share/lib/zoneinfo",
    "/usr/lib/zoneinfo",
    "/usr/local/etc/zoneinfo",
];

/// Resolve a zone name.
fn load_zone(name: &str) -> Result<Zone, String> {
    let trimmed = name.strip_prefix(':').unwrap_or(name);
    if trimmed.is_empty() {
        return Ok(Zone::fixed(0, "GMT"));
    }
    if trimmed.eq_ignore_ascii_case("utc") || trimmed.eq_ignore_ascii_case("gmt") {
        return Ok(Zone::fixed(0, trimmed));
    }
    if trimmed == "localtime" {
        return system_zone();
    }
    if let Some(offset) = fixed_offset(name) {
        return Ok(Zone::fixed(offset, name));
    }
    // A traversal would read a file outside the zone database, which is not
    // something a zone name may do.
    if trimmed.starts_with('/') || trimmed.split('/').any(|part| part == "..") {
        return Err(format!("time zone \"{name}\" not found"));
    }
    for directory in ZONE_DIRECTORIES {
        let path = std::path::Path::new(directory).join(trimmed);
        if let Ok(bytes) = std::fs::read(&path) {
            if let Some(zone) = parse_tzif(&bytes) {
                return Ok(zone);
            }
        }
    }
    Err(format!(
        "time zone \"{name}\" not found: no zone file names it, and a POSIX time zone rule is not supported yet"
    ))
}

/// `SetupTimeZone`'s fixed-offset form: `[+-]hh`, `hhmm`, `hh:mm`, `hhmmss` or
/// `hh:mm:ss` (`library/clock.tcl`).
fn fixed_offset(text: &str) -> Option<i32> {
    let chars: Vec<char> = text.chars().collect();
    let sign = match chars.first()? {
        '+' => 1,
        '-' => -1,
        _ => return None,
    };
    let two = |from: usize| -> Option<i32> {
        let a = chars.get(from)?.to_digit(10)?;
        let b = chars.get(from + 1)?.to_digit(10)?;
        Some((a * 10 + b) as i32)
    };
    let hours = two(1)?;
    let mut at = 3;
    let field = |at: &mut usize| -> Option<i32> {
        let start = if chars.get(*at) == Some(&':') {
            *at + 1
        } else {
            *at
        };
        let value = two(start)?;
        *at = start + 2;
        Some(value)
    };
    let minutes = match field(&mut at) {
        Some(m) => m,
        // Trailing text that is not a minute field means this is a name and
        // not an offset: `+foo` is not `+00`.
        None => return (at == chars.len()).then_some(sign * hours * 3600),
    };
    let seconds = field(&mut at).unwrap_or(0);
    if at != chars.len() {
        return None;
    }
    Some(sign * ((hours * 60 + minutes) * 60 + seconds))
}

/// The zone a script gets when it names none: `TZ` when it is set, and the
/// system's own zone otherwise. tclsh reads the same two.
fn system_zone() -> Result<Zone, String> {
    if let Ok(tz) = std::env::var("TZ") {
        if !tz.is_empty() {
            return load_zone(&tz);
        }
    }
    match std::fs::read("/etc/localtime") {
        Ok(bytes) => parse_tzif(&bytes)
            .ok_or_else(|| "clock: /etc/localtime is not a time zone file".to_string()),
        Err(_) => Ok(Zone::fixed(0, "GMT")),
    }
}

// ── the message catalogue ────────────────────────────────────────────────

const MONTHS_ABBREV: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const MONTHS_FULL: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const DAYS_ABBREV: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const DAYS_FULL: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

/// `::tcl::clock::LocalizeFormat`'s substitutions, in its order — later
/// entries expand into earlier ones, so the order is significant. The values
/// are the root locale's `msgcat` catalogue (`library/clock.tcl`), with the
/// nesting already resolved.
const FORMAT_ALIASES: &[(&str, &str)] = &[
    ("%%", "%%"),
    ("%D", "%m/%d/%Y"),
    ("%+", "%a %b %e %H:%M:%S %Z %Y"),
    ("%T", "%H:%M:%S"),
    ("%R", "%H:%M"),
    ("%r", "%I:%M:%S %P"),
    ("%EX", "%H:%M:%S"),
    ("%X", "%H:%M:%S"),
    ("%Ex", "%m/%d/%Y"),
    ("%x", "%m/%d/%Y"),
    ("%Ec", "%a %b %e %H:%M:%S %Y"),
    ("%c", "%a %b %e %H:%M:%S %Y"),
];

/// Expand the locale format groups the way `LocalizeFormat` does: one
/// left-to-right pass in which `%%` maps to itself, so an escaped percent
/// cannot start a group.
fn localize(format: &str) -> String {
    let mut out = String::with_capacity(format.len());
    let mut rest = format;
    'outer: while !rest.is_empty() {
        if rest.starts_with('%') {
            for (from, to) in FORMAT_ALIASES {
                if rest.starts_with(from) {
                    out.push_str(to);
                    rest = &rest[from.len()..];
                    continue 'outer;
                }
            }
        }
        let ch = rest.chars().next().expect("not empty");
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    out
}

// ── formatting ───────────────────────────────────────────────────────────

/// The default `-format`, which `clock format` uses when the script names
/// none (`library/clock.tcl`).
const DEFAULT_FORMAT: &str = "%a %b %d %H:%M:%S %Z %Y";

/// A number padded to `width` with `fill`, with the sign kept outside the
/// padding — `Clock_itoaw`'s layout.
fn pad(value: i64, width: usize, fill: char) -> String {
    let digits = value.unsigned_abs().to_string();
    let sign = if value < 0 { 1 } else { 0 };
    let mut out = String::with_capacity(width.max(digits.len() + sign));
    if sign == 1 {
        out.push('-');
    }
    for _ in digits.len() + sign..width {
        out.push(fill);
    }
    out.push_str(&digits);
    out
}

/// The zone offset as `%z` writes it.
fn offset_text(offset: i32) -> String {
    let sign = if offset < 0 { '-' } else { '+' };
    let total = offset.unsigned_abs();
    let (hours, minutes, seconds) = (total / 3600, total / 60 % 60, total % 60);
    if seconds == 0 {
        format!("{sign}{hours:02}{minutes:02}")
    } else {
        format!("{sign}{hours:02}{minutes:02}{seconds:02}")
    }
}

fn format_time(seconds: i64, format: &str, zone: &Zone) -> Result<String, String> {
    if seconds < EARLIEST {
        return Err(too_early());
    }
    let state = zone.at(seconds);
    let local = seconds
        .checked_add(state.offset as i64)
        .ok_or_else(overflow)?;
    let civil = civil_of(local);
    let expanded = localize(format);
    let mut out = String::with_capacity(expanded.len() + 16);
    let mut rest = expanded.as_str();
    while let Some(at) = rest.find('%') {
        out.push_str(&rest[..at]);
        rest = &rest[at + 1..];
        let Some(token) = rest.chars().next() else {
            // A trailing `%` is itself, as tclsh's scanner leaves it.
            out.push('%');
            return Ok(out);
        };
        rest = &rest[token.len_utf8()..];
        match one_token(token, &civil, seconds, state) {
            Some(text) => out.push_str(&text),
            // A token that is in no map is copied through unchanged, which is
            // what tclsh does for `%F` and `%i` — measured, not assumed.
            None => {
                out.push('%');
                out.push(token);
            }
        }
    }
    out.push_str(rest);
    Ok(out)
}

/// One `%`-token's text, or `None` when the token is not one tclsh's
/// `FmtSTokenMap` carries.
fn one_token(token: char, civil: &Civil, seconds: i64, state: &State) -> Option<String> {
    let weekday = civil.iso_weekday();
    let hour12 = match civil.hour % 12 {
        0 => 12,
        other => other,
    };
    Some(match token {
        '%' => "%".to_string(),
        'd' => pad(civil.day as i64, 2, '0'),
        'e' => pad(civil.day as i64, 2, ' '),
        'm' => pad(civil.month as i64, 2, '0'),
        'N' => pad(civil.month as i64, 2, ' '),
        'b' | 'h' => MONTHS_ABBREV[(civil.month - 1) as usize].to_string(),
        'B' => MONTHS_FULL[(civil.month - 1) as usize].to_string(),
        'y' => pad(civil.year.rem_euclid(100), 2, '0'),
        'Y' => pad(civil.year, 4, '0'),
        'C' => pad(civil.year.div_euclid(100), 2, '0'),
        'H' => pad(civil.hour as i64, 2, '0'),
        'M' => pad(civil.minute as i64, 2, '0'),
        'S' => pad(civil.second as i64, 2, '0'),
        'I' => pad(hour12 as i64, 2, '0'),
        'k' => pad(civil.hour as i64, 2, ' '),
        'l' => pad(hour12 as i64, 2, ' '),
        'p' => if civil.hour < 12 { "AM" } else { "PM" }.to_string(),
        'P' => if civil.hour < 12 { "am" } else { "pm" }.to_string(),
        'a' => DAYS_ABBREV[(weekday % 7) as usize].to_string(),
        'A' => DAYS_FULL[(weekday % 7) as usize].to_string(),
        'u' => weekday.to_string(),
        'w' => (weekday % 7).to_string(),
        'U' => pad(civil.week_of_year(0), 2, '0'),
        'W' => pad(civil.week_of_year(1), 2, '0'),
        'V' => pad(civil.iso_week().1, 2, '0'),
        'g' => pad(civil.iso_week().0.rem_euclid(100), 2, '0'),
        'G' => pad(civil.iso_week().0, 4, '0'),
        'j' => pad(civil.day_of_year(), 3, '0'),
        'J' => pad(civil.julian_day(), 7, '0'),
        's' => seconds.to_string(),
        'n' => "\n".to_string(),
        't' => "\t".to_string(),
        'z' => offset_text(state.offset),
        'Z' => state.abbreviation.clone(),
        _ => return None,
    })
}

// ── scanning ─────────────────────────────────────────────────────────────

/// The fields a `-format` scan fills in, before they are turned into an
/// instant.
#[derive(Default)]
struct Scanned {
    year: Option<i64>,
    century: Option<i64>,
    year_in_century: Option<i64>,
    month: Option<u32>,
    day: Option<u32>,
    day_of_year: Option<i64>,
    hour: Option<u32>,
    minute: Option<u32>,
    second: Option<u32>,
    pm: Option<bool>,
    hour_is_12: bool,
    epoch: Option<i64>,
    offset: Option<i32>,
}

fn no_match() -> String {
    "input string does not match supplied format".to_string()
}

/// Read up to `max` digits.
fn take_digits(text: &[char], at: &mut usize, max: usize) -> Option<i64> {
    let start = *at;
    let mut value: i64 = 0;
    while *at < text.len() && *at - start < max && text[*at].is_ascii_digit() {
        value = value * 10 + text[*at].to_digit(10)? as i64;
        *at += 1;
    }
    (*at != start).then_some(value)
}

/// Match one of a table's entries case-insensitively, longest first so that
/// `January` is not read as `Jan` with `uary` left over.
fn take_name(text: &[char], at: &mut usize, table: &[&str]) -> Option<usize> {
    let mut best: Option<(usize, usize)> = None;
    for (i, name) in table.iter().enumerate() {
        let chars: Vec<char> = name.chars().collect();
        if text.len() - *at >= chars.len()
            && text[*at..*at + chars.len()]
                .iter()
                .zip(&chars)
                .all(|(a, b)| a.eq_ignore_ascii_case(b))
            && best.is_none_or(|(_, len)| chars.len() > len)
        {
            best = Some((i, chars.len()));
        }
    }
    let (index, len) = best?;
    *at += len;
    Some(index)
}

/// The tokens whose value may be written with leading spaces — `%e`, `%k` and
/// `%l` do, and tclsh's scanner skips space ahead of every numeric field.
const NUMERIC_TOKENS: &str = "deEmNyYCHkIlMSjsUWVGgu w";

fn scan_time(input: &str, format: &str, zone: &Zone) -> Result<i64, String> {
    let text: Vec<char> = input.chars().collect();
    let pattern: Vec<char> = localize(format).chars().collect();
    let mut got = Scanned::default();
    let mut at = 0usize;
    let mut p = 0usize;
    while p < pattern.len() {
        let ch = pattern[p];
        if ch != '%' {
            // Whitespace in the format matches any run of it, including none,
            // as tclsh's scanner does.
            if ch.is_whitespace() {
                p += 1;
                while at < text.len() && text[at].is_whitespace() {
                    at += 1;
                }
                continue;
            }
            if text.get(at) != Some(&ch) {
                return Err(no_match());
            }
            at += 1;
            p += 1;
            continue;
        }
        p += 1;
        let Some(token) = pattern.get(p).copied() else {
            return Err(no_match());
        };
        p += 1;
        if NUMERIC_TOKENS.contains(token) {
            while at < text.len() && text[at] == ' ' {
                at += 1;
            }
        }
        let digits = |at: &mut usize, max: usize| take_digits(&text, at, max).ok_or_else(no_match);
        match token {
            '%' => {
                if text.get(at) != Some(&'%') {
                    return Err(no_match());
                }
                at += 1;
            }
            'n' | 't' => {
                if !text.get(at).is_some_and(|c| c.is_whitespace()) {
                    return Err(no_match());
                }
                at += 1;
            }
            'd' | 'e' => got.day = Some(digits(&mut at, 2)? as u32),
            'm' | 'N' => got.month = Some(digits(&mut at, 2)? as u32),
            'b' | 'h' | 'B' => {
                let index = take_name(&text, &mut at, &MONTHS_FULL)
                    .or_else(|| take_name(&text, &mut at, &MONTHS_ABBREV))
                    .ok_or_else(no_match)?;
                got.month = Some(index as u32 + 1);
            }
            'a' | 'A' => {
                take_name(&text, &mut at, &DAYS_FULL)
                    .or_else(|| take_name(&text, &mut at, &DAYS_ABBREV))
                    .ok_or_else(no_match)?;
            }
            'y' => got.year_in_century = Some(digits(&mut at, 2)?),
            'Y' => got.year = Some(digits(&mut at, 4)?),
            'C' => got.century = Some(digits(&mut at, 2)?),
            'H' | 'k' => got.hour = Some(digits(&mut at, 2)? as u32),
            'I' | 'l' => {
                got.hour = Some(digits(&mut at, 2)? as u32);
                got.hour_is_12 = true;
            }
            'M' => got.minute = Some(digits(&mut at, 2)? as u32),
            'S' => got.second = Some(digits(&mut at, 2)? as u32),
            'j' => got.day_of_year = Some(digits(&mut at, 3)?),
            'p' | 'P' => {
                let index = take_name(&text, &mut at, &["AM", "PM"]).ok_or_else(no_match)?;
                got.pm = Some(index == 1);
            }
            's' => {
                let negative = text.get(at) == Some(&'-');
                if negative || text.get(at) == Some(&'+') {
                    at += 1;
                }
                let value = digits(&mut at, 19)?;
                got.epoch = Some(if negative { -value } else { value });
            }
            // Read and discarded: the day of the week is implied by the date,
            // and tclsh's scanner also lets a wrong one through.
            'u' | 'w' => {
                digits(&mut at, 1)?;
            }
            'U' | 'W' | 'V' => {
                digits(&mut at, 2)?;
            }
            'G' => {
                digits(&mut at, 4)?;
            }
            'g' => {
                digits(&mut at, 2)?;
            }
            'z' | 'Z' => got.offset = Some(scan_zone(&text, &mut at)?),
            other => {
                return Err(format!(
                    "clock scan: the format token \"%{other}\" is not supported yet"
                ))
            }
        }
    }
    while at < text.len() && text[at].is_whitespace() {
        at += 1;
    }
    if at != text.len() {
        return Err(no_match());
    }
    assemble(got, zone)
}

/// A zone in the input: a numeric offset, or one of the names that plainly
/// mean UTC. Reading an arbitrary abbreviation would need the table tclsh
/// builds from the whole zone database, and guessing one wrong moves the
/// answer by hours.
fn scan_zone(text: &[char], at: &mut usize) -> Result<i32, String> {
    if matches!(text.get(*at), Some('+') | Some('-')) {
        let start = *at;
        *at += 1;
        while text
            .get(*at)
            .is_some_and(|c| c.is_ascii_digit() || *c == ':')
        {
            *at += 1;
        }
        let candidate: String = text[start..*at].iter().collect();
        return fixed_offset(&candidate).ok_or_else(no_match);
    }
    match take_name(text, at, &["GMT", "UTC", "Z"]) {
        Some(_) => Ok(0),
        None => {
            Err("clock scan: reading a time zone by abbreviation is not supported yet".to_string())
        }
    }
}

/// Turn scanned fields into an instant.
fn assemble(got: Scanned, zone: &Zone) -> Result<i64, String> {
    if let Some(epoch) = got.epoch {
        return Ok(epoch);
    }
    // Fields the format did not carry come from the current day in the target
    // zone, which is the base tclsh uses when `-base` is absent.
    let now = current_seconds();
    let base = civil_of(now + zone.at(now).offset as i64);
    let dated = got.year.is_some() || got.year_in_century.is_some() || got.century.is_some();
    let year = match (got.year, got.century, got.year_in_century) {
        (Some(year), _, _) => year,
        (None, Some(century), Some(year)) => century * 100 + year,
        // tclsh's two-digit year rule: 00–68 are 2000s, 69–99 are 1900s.
        (None, None, Some(year)) => year + if year < 69 { 2000 } else { 1900 },
        (None, Some(century), None) => century * 100,
        (None, None, None) => base.year,
    };
    let mut hour = got.hour.unwrap_or(0);
    if got.hour_is_12 {
        hour %= 12;
        if got.pm == Some(true) {
            hour += 12;
        }
    } else if got.pm == Some(true) && hour < 12 {
        hour += 12;
    }
    let days = match got.day_of_year {
        Some(day) => days_from_civil(year, 1, 1) + day - 1,
        None => {
            let month = got.month.unwrap_or(if dated { 1 } else { base.month });
            if !(1..=12).contains(&month) {
                return Err(no_match());
            }
            let day = got.day.unwrap_or(if dated || got.month.is_some() {
                1
            } else {
                base.day
            });
            days_from_civil(year, month, day)
        }
    };
    let local = days * 86400
        + hour as i64 * 3600
        + got.minute.unwrap_or(0) as i64 * 60
        + got.second.unwrap_or(0) as i64;
    let seconds = match got.offset {
        Some(offset) => local - offset as i64,
        None => local - zone.for_local(local).offset as i64,
    };
    if seconds < EARLIEST {
        return Err(too_early());
    }
    Ok(seconds)
}

// ── clock add ────────────────────────────────────────────────────────────

/// The units `clock add` takes, in the order it lists them when it rejects
/// one.
const UNITS: &[&str] = &[
    "years", "months", "week", "weeks", "days", "weekdays", "hours", "minutes", "seconds",
];

fn add_units(seconds: i64, count: i64, unit: &str, zone: &Zone) -> Result<i64, String> {
    let scale = match unit {
        "seconds" => Some(1),
        "minutes" => Some(60),
        "hours" => Some(3600),
        _ => None,
    };
    if let Some(scale) = scale {
        return seconds
            .checked_add(count.checked_mul(scale).ok_or_else(overflow)?)
            .ok_or_else(overflow);
    }
    // Every other unit is calendar arithmetic: the *local* date moves and the
    // zone offset is applied again, which is what makes adding a day across a
    // daylight change land at the same wall-clock time.
    let local = seconds
        .checked_add(zone.at(seconds).offset as i64)
        .ok_or_else(overflow)?;
    let civil = civil_of(local);
    let days = match unit {
        "days" => civil.epoch_day.checked_add(count).ok_or_else(overflow)?,
        "week" | "weeks" => civil
            .epoch_day
            .checked_add(count.checked_mul(7).ok_or_else(overflow)?)
            .ok_or_else(overflow)?,
        "weekdays" => weekday_walk(civil.epoch_day, count),
        _ => {
            let months = if unit == "years" {
                count.checked_mul(12).ok_or_else(overflow)?
            } else {
                count
            };
            let total = (civil.year * 12 + civil.month as i64 - 1)
                .checked_add(months)
                .ok_or_else(overflow)?;
            let year = total.div_euclid(12);
            let month = total.rem_euclid(12) as u32 + 1;
            // A day past the end of the target month is clamped to it, as
            // tclsh's `AddMonths` does.
            days_from_civil(year, month, civil.day.min(month_length(year, month)))
        }
    };
    let moved = days * 86400 + local.rem_euclid(86400);
    let result = moved - zone.for_local(moved).offset as i64;
    if result < EARLIEST {
        return Err(too_early());
    }
    Ok(result)
}

/// `weekdays` counts only Monday through Friday.
fn weekday_walk(start: i64, count: i64) -> i64 {
    let step = if count < 0 { -1 } else { 1 };
    let mut day = start;
    let mut left = count.abs();
    while left > 0 {
        day += step;
        if (day + 3).rem_euclid(7) < 5 {
            left -= 1;
        }
    }
    day
}

fn overflow() -> String {
    "integer value too large to represent".to_string()
}

fn bad_unit(unit: &str) -> String {
    format!("bad unit \"{unit}\": must be {}", listing(UNITS))
}

// ── options ──────────────────────────────────────────────────────────────

/// The options `format`, `scan` and `add` share.
struct Options {
    format: Option<String>,
    gmt: Option<bool>,
    timezone: Option<String>,
    base: Option<i64>,
}

impl Options {
    /// Resolve the zone the options ask for.
    fn zone(&self) -> Result<Zone, String> {
        if self.gmt.is_some() && self.timezone.is_some() {
            return Err("cannot use -gmt and -timezone in same call".to_string());
        }
        match (&self.timezone, self.gmt) {
            (Some(name), _) => load_zone(name),
            (None, Some(true)) => Ok(Zone::fixed(0, "GMT")),
            _ => system_zone(),
        }
    }
}

/// Read the trailing `-option value` pairs. `usage` is the wording the command
/// reports when a value is missing, which differs per subcommand.
fn options(words: &[Value], allowed: &[&str], usage: &str) -> Result<Options, String> {
    let mut out = Options {
        format: None,
        gmt: None,
        timezone: None,
        base: None,
    };
    let mut i = 0;
    while i < words.len() {
        let name = to_tcl_string(&words[i]);
        let Some(option) = resolve(&name, allowed) else {
            return Err(format!(
                "bad option \"{name}\": must be {}",
                listing(allowed)
            ));
        };
        let Some(value) = words.get(i + 1) else {
            return Err(usage.to_string());
        };
        match option {
            "-format" => out.format = Some(to_tcl_string(value)),
            "-gmt" => out.gmt = Some(crate::runtime::tcl_bool(value)?),
            "-timezone" => out.timezone = Some(to_tcl_string(value)),
            "-base" => out.base = Some(seconds_of(value)?),
            // The locale decides the month and day names, the AM/PM words and
            // the `%c`/`%x`/`%X` expansions; only the root catalogue is here.
            "-locale" => {
                let locale = to_tcl_string(value);
                if !matches!(
                    locale.to_ascii_lowercase().as_str(),
                    "" | "c" | "posix" | "current" | "system"
                ) {
                    return Err(format!(
                        "clock: the locale \"{locale}\" is not supported yet; only the root locale is built in"
                    ));
                }
            }
            _ => unreachable!("the option table and this match are one list"),
        }
        i += 2;
    }
    Ok(out)
}

/// A clock value: an integer, or `now`.
fn seconds_of(v: &Value) -> Result<i64, String> {
    let text = tcl_str(v);
    if text.trim() == "now" {
        return Ok(current_seconds());
    }
    match crate::runtime::parse_number(text.trim()) {
        Ok(Num::Int(i)) => Ok(i),
        _ => Err(format!("bad seconds \"{text}\": must be now or integer")),
    }
}

fn current_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

fn current_seconds() -> i64 {
    current_micros().div_euclid(1_000_000)
}

// ── running ──────────────────────────────────────────────────────────────

const FORMAT_USAGE: &str = "wrong # args: should be \"clock format clockval|now ?-format string? ?-gmt boolean? ?-locale LOCALE? ?-timezone ZONE?\"";
const SCAN_USAGE: &str = "wrong # args: should be \"clock scan string ?-base seconds? ?-format string? ?-gmt boolean? ?-locale LOCALE? ?-timezone ZONE?\"";
const ADD_USAGE: &str = "wrong # args: should be \"clock add clockval ?number units?... ?-gmt boolean? ?-locale LOCALE? ?-timezone ZONE?\"";

pub(crate) fn extension(vm: &mut VM, id: u16, arg: u8) -> Result<(), String> {
    if id == ext::NOW {
        let switch = if arg == 3 { Some(vm.pop()) } else { None };
        let value = now(arg, switch.as_ref())?;
        vm.push(value);
        return Ok(());
    }
    let mut words = Vec::with_capacity(arg as usize);
    for _ in 0..arg {
        words.push(vm.pop());
    }
    words.reverse();
    let value = match id {
        ext::FORMAT => run_format(&words)?,
        ext::SCAN => run_scan(&words)?,
        _ => run_add(&words)?,
    };
    vm.push(value);
    Ok(())
}

fn now(unit: u8, switch: Option<&Value>) -> Result<Value, String> {
    if let Some(switch) = switch {
        // `clock clicks` with no switch answers the highest-resolution
        // counter the platform has, which here is the microsecond clock the
        // other two units are read from.
        return match to_tcl_string(switch).as_str() {
            "-milliseconds" => Ok(Value::Int(current_micros() / 1000)),
            "-microseconds" | "" => Ok(Value::Int(current_micros())),
            other => Err(format!(
                "bad option \"{other}\": must be -microseconds or -milliseconds"
            )),
        };
    }
    Ok(Value::Int(match unit {
        0 => current_seconds(),
        1 => current_micros() / 1000,
        _ => current_micros(),
    }))
}

fn run_format(words: &[Value]) -> Result<Value, String> {
    let Some(clock) = words.first() else {
        return Err(FORMAT_USAGE.to_string());
    };
    let seconds = seconds_of(clock)?;
    let opts = options(
        &words[1..],
        &["-format", "-gmt", "-locale", "-timezone"],
        FORMAT_USAGE,
    )?;
    let zone = opts.zone()?;
    let format = opts.format.as_deref().unwrap_or(DEFAULT_FORMAT);
    Ok(Value::Str(Arc::new(format_time(seconds, format, &zone)?)))
}

fn run_scan(words: &[Value]) -> Result<Value, String> {
    let Some(input) = words.first() else {
        return Err(SCAN_USAGE.to_string());
    };
    let opts = options(
        &words[1..],
        &["-base", "-format", "-gmt", "-locale", "-timezone"],
        SCAN_USAGE,
    )?;
    let zone = opts.zone()?;
    if opts.base.is_some() {
        return Err("clock scan: -base is not supported yet".to_string());
    }
    let Some(format) = opts.format.as_deref() else {
        return Err(
            "clock scan: the free-form parser is not supported yet; use -format".to_string(),
        );
    };
    Ok(Value::Int(scan_time(&to_tcl_string(input), format, &zone)?))
}

fn run_add(words: &[Value]) -> Result<Value, String> {
    let Some(clock) = words.first() else {
        return Err(ADD_USAGE.to_string());
    };
    let mut seconds = seconds_of(clock)?;
    // The offsets come first and the options after them; the first word that
    // reads as an option name ends the offset list.
    let rest = &words[1..];
    let split = rest
        .iter()
        .position(|w| {
            let text = to_tcl_string(w);
            text.starts_with('-') && text[1..].starts_with(|c: char| c.is_ascii_alphabetic())
        })
        .unwrap_or(rest.len());
    let (offsets, tail) = rest.split_at(split);
    let opts = options(tail, &["-base", "-gmt", "-locale", "-timezone"], ADD_USAGE)?;
    let zone = opts.zone()?;
    let mut i = 0;
    while i < offsets.len() {
        let count = match crate::runtime::parse_number(tcl_str(&offsets[i]).trim()) {
            Ok(Num::Int(n)) => n,
            _ => {
                return Err(format!(
                    "expected integer but got \"{}\"",
                    to_tcl_string(&offsets[i])
                ))
            }
        };
        let Some(unit) = offsets.get(i + 1) else {
            return Err(ADD_USAGE.to_string());
        };
        let unit = to_tcl_string(unit);
        let Some(resolved) = resolve(&unit, UNITS) else {
            return Err(bad_unit(&unit));
        };
        seconds = add_units(seconds, count, resolved, &zone)?;
        i += 2;
    }
    Ok(Value::Int(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The civil-date conversions are each other's inverse over the whole
    /// range this module answers for.
    #[test]
    fn the_calendar_round_trips() {
        for day in [-79366i64, -1, 0, 1, 14288, 100000, 2932896] {
            let (y, m, d) = civil_from_days(day);
            assert_eq!(days_from_civil(y, m, d), day, "day {day} -> {y}-{m}-{d}");
        }
    }

    /// A fixed epoch, so nothing here depends on when the test runs. The
    /// differential suite is what pins these against tclsh; this guards the
    /// pieces of the derivation a whole-process run would not localize.
    #[test]
    fn a_known_instant_formats() {
        let utc = Zone::fixed(0, "GMT");
        let out = format_time(1234567890, DEFAULT_FORMAT, &utc).expect("formats");
        assert_eq!(out, "Fri Feb 13 23:31:30 GMT 2009");
        let iso = format_time(1234567890, "%G-W%V-%u %j %U %W", &utc).expect("formats");
        assert_eq!(iso, "2009-W07-5 044 06 06");
    }

    /// Before the changeover the answer would depend on the locale's calendar,
    /// so there is no answer rather than a wrong one.
    #[test]
    fn early_dates_are_refused() {
        let utc = Zone::fixed(0, "GMT");
        let err = format_time(EARLIEST - 1, "%Y", &utc).expect_err("refused");
        assert!(err.contains("Gregorian changeover"), "{err}");
    }

    /// The fixed-offset zone names `SetupTimeZone` accepts, and the ones it
    /// leaves for the zone database.
    #[test]
    fn numeric_zones_parse() {
        assert_eq!(fixed_offset("+0530"), Some(19800));
        assert_eq!(fixed_offset("-05:30"), Some(-19800));
        assert_eq!(fixed_offset("+01"), Some(3600));
        assert_eq!(fixed_offset("+01:02:03"), Some(3723));
        assert_eq!(fixed_offset("CET"), None);
        assert_eq!(fixed_offset("+abc"), None);
    }
}
