//! The `clock` ensemble.
//!
//! What is here and what is refused, stated plainly, because a wrong
//! `clock format` is worse than one that says it cannot answer:
//!
//! * `seconds`, `milliseconds`, `microseconds`, `clicks` — complete.
//! * `format` — the whole token set of tclsh 9.0.4's `FmtSTokenMap`, the
//!   `%E` and `%O` maps beside it (`generic/tclClockFmt.c`) and the
//!   locale-format expansions `::tcl::clock::LocalizeFormat` performs
//!   (`library/clock.tcl`).
//! * `-locale` — every catalogue the release ships, read from the vendored
//!   copies in [`crate::clock_msgs`] through [`crate::clock_locale`]. A name
//!   with no catalogue anywhere in its fallback chain is the root locale, as
//!   it is there.
//! * `scan` — the `-format` form only, with the whole of `ScnSTokenMap` and
//!   the `%E` and `%O` maps beside it. tclsh's free-form parser is a
//!   several-thousand-line grammar over relative words, month names, ISO
//!   forms and time zone abbreviations; it is refused by name rather than
//!   approximated.
//! * `add` — every unit tclsh accepts, with the calendar arithmetic for
//!   months and years and the weekday walk for `weekdays`.
//! * Time zones — `-gmt`, a fixed numeric offset, any zone with a `TZif`
//!   file, read with the same reader tclsh's `LoadZoneinfoFile` implements in
//!   Tcl (the POSIX rule at the end of a version 2 file included), a POSIX
//!   `TZ` rule string (`ParsePosixTimeZone` / `ProcessPosixTimeZone`), and the
//!   legacy abbreviations of `LegacyTimeZone`. The default zone comes from
//!   `TZ` or `/etc/localtime`, as tclsh's does.
//!
//! Refused, each with its own message:
//!
//! * Any instant before the Gregorian changeover of 1752-09-14, which tclsh
//!   reckons in the Julian calendar and this module has no calendar for. See
//!   `EARLIEST`, and note the date is not the locale's.
//! * A time zone named by abbreviation in `clock scan`.
//! * `clock scan` without `-format`.
//!
//! A `-format` scan assembles its fields as `ClockScan` and `ClockScanCommit`
//! do: every field the format does not carry is the base date's, a weekday
//! with no day of the month or of the year chooses the day within an ISO week,
//! and `-validate` (on by default) holds the fields to `ClockValidDate`'s
//! ranges.

use std::sync::Arc;

use fusevm::{Op, Value, VM};

use crate::clock_locale::Catalog;
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

/// The first instant this module will reckon: 1752-09-14T00:00:00Z, Julian day
/// 2361222. Before it tclsh reckons in the Julian calendar and this module has
/// one calendar, so it refuses rather than answering a Gregorian date for a
/// Julian one.
///
/// The changeover is not the locale's. Catalogues carry a
/// `GREGORIAN_CHANGE_DATE` and `clock.tcl` sets one for a dozen languages, but
/// Tcl 9's formatter passes the compile-time `GREGORIAN_CHANGE_DATE`
/// (`generic/tclClock.c`) to `TclConvertUTCToLocal` and never reads the
/// catalogue's — measured: `clock format -11676096000 -format %Y-%m-%d -gmt 1`
/// answers `1599-12-22` under `-locale en`, `it`, `ru`, `el` and the root
/// locale alike, and `1752-09-02` is the last Julian date every one of them
/// writes.
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

    /// Seconds since local midnight — `DateInfo.secondOfDay`, which the tokens
    /// that write a time of day are all derived from.
    fn second_of_day(&self) -> i64 {
        self.hour as i64 * 3600 + self.minute as i64 * 60 + self.second as i64
    }
}

/// `SECONDS_PER_DAY`, which the Julian-day and stardate tokens divide by.
const SECONDS_PER_DAY: i64 = 86_400;

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
    // `ReadZoneinfoFile` reads the 64-bit block, and the POSIX rule that
    // follows it, from a version "2" file only; any other version is read
    // from its 32-bit block alone.
    if bytes[4] == b'2' {
        let second = block_length(bytes, 4)?;
        let rest = bytes.get(second..)?;
        if rest.len() >= 44 && &rest[..4] == b"TZif" {
            let mut zone = read_block(rest, 8)?;
            // The rule sits between two newlines after the block, and its
            // transitions extend the file's past the last one it lists.
            let footer = rest.get(block_length(rest, 8)? + 1..).unwrap_or_default();
            let end = footer
                .iter()
                .position(|b| *b == b'\n')
                .unwrap_or(footer.len());
            let rule = String::from_utf8_lossy(&footer[..end]);
            if !rule.trim().is_empty() {
                let (_, rules) = PosixZone::parse(&rule)?.process();
                let last = zone.transitions.last().map_or(i64::MIN, |(t, _)| *t);
                zone.transitions
                    .extend(rules.into_iter().filter(|(t, _)| *t > last));
            }
            return Some(zone);
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

// ── POSIX time zone rules ────────────────────────────────────────────────

/// One `start` or `end` rule of a POSIX `TZ` string: `Jn` / `n` (a day of the
/// year) or `Mm.w.d` (a weekday of a week of a month), and the `/time` after
/// it. A field the string left out is `None`, as an unmatched group is the
/// empty string in `ParsePosixTimeZone`'s result.
#[derive(Default, Clone)]
struct PosixBound {
    julian: bool,
    day_of_year: Option<i64>,
    month: Option<i64>,
    week_of_month: Option<i64>,
    day_of_week: Option<i64>,
    hours: Option<i64>,
    minutes: Option<i64>,
    seconds: Option<i64>,
}

/// `ParsePosixTimeZone`'s fields (`library/clock.tcl`).
struct PosixZone {
    std_name: String,
    std_negative: bool,
    std_hms: (i64, Option<i64>, Option<i64>),
    dst_name: Option<String>,
    dst_negative: bool,
    dst_hms: Option<(i64, Option<i64>, Option<i64>)>,
    start: PosixBound,
    end: PosixBound,
}

/// A cursor over a POSIX `TZ` string, one method per piece of the expression
/// `ParsePosixTimeZone` matches it against. The match ignores case.
struct PosixCursor<'a> {
    text: &'a [u8],
    at: usize,
}

impl PosixCursor<'_> {
    fn peek(&self) -> Option<u8> {
        self.text.get(self.at).copied()
    }

    fn eat(&mut self, byte: u8) -> bool {
        let hit = self.peek().is_some_and(|b| b.eq_ignore_ascii_case(&byte));
        if hit {
            self.at += 1;
        }
        hit
    }

    /// `[[:alpha:]]+ | <[-+[:alnum:]]+>`, with the brackets kept.
    fn name(&mut self) -> Option<String> {
        let start = self.at;
        if self.eat(b'<') {
            while self
                .peek()
                .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'+')
            {
                self.at += 1;
            }
            if self.at == start + 1 || !self.eat(b'>') {
                self.at = start;
                return None;
            }
        } else {
            while self.peek().is_some_and(|b| b.is_ascii_alphabetic()) {
                self.at += 1;
            }
            if self.at == start {
                return None;
            }
        }
        Some(String::from_utf8_lossy(&self.text[start..self.at]).into_owned())
    }

    /// Between `min` and `max` digits.
    fn digits(&mut self, max: usize) -> Option<i64> {
        let start = self.at;
        while self.at - start < max && self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.at += 1;
        }
        (self.at > start).then(|| {
            std::str::from_utf8(&self.text[start..self.at])
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(i64::MAX)
        })
    }

    /// `h{1,2} (: m{1,2} (: s{1,2})?)?`
    fn clock(&mut self) -> Option<(i64, Option<i64>, Option<i64>)> {
        let hours = self.digits(2)?;
        let mut minutes = None;
        let mut seconds = None;
        let back = self.at;
        if self.eat(b':') {
            match self.digits(2) {
                Some(m) => {
                    minutes = Some(m);
                    let back = self.at;
                    if self.eat(b':') {
                        match self.digits(2) {
                            Some(s) => seconds = Some(s),
                            None => self.at = back,
                        }
                    }
                }
                None => self.at = back,
            }
        }
        Some((hours, minutes, seconds))
    }

    /// `(J?)(\d+) | M(\d+).(\d+).(\d+)`, then an optional `/time`.
    fn bound(&mut self) -> Option<PosixBound> {
        let mut bound = PosixBound::default();
        if self.eat(b'M') {
            bound.month = Some(self.digits(usize::MAX)?);
            if !self.eat(b'.') {
                return None;
            }
            bound.week_of_month = Some(self.digits(usize::MAX)?);
            if !self.eat(b'.') {
                return None;
            }
            bound.day_of_week = Some(self.digits(usize::MAX)?);
        } else {
            bound.julian = self.eat(b'J');
            bound.day_of_year = Some(self.digits(usize::MAX)?);
        }
        let back = self.at;
        if self.eat(b'/') {
            match self.clock() {
                Some((h, m, s)) => {
                    bound.hours = Some(h);
                    bound.minutes = m;
                    bound.seconds = s;
                }
                None => self.at = back,
            }
        }
        Some(bound)
    }
}

impl PosixZone {
    /// `ParsePosixTimeZone`: `None` where the expression does not match the
    /// whole string.
    fn parse(tz: &str) -> Option<PosixZone> {
        let mut c = PosixCursor {
            text: tz.as_bytes(),
            at: 0,
        };
        let std_name = c.name()?;
        let std_negative = c.peek() == Some(b'-');
        if matches!(c.peek(), Some(b'-' | b'+')) {
            c.at += 1;
        }
        let std_hms = c.clock()?;
        let mut zone = PosixZone {
            std_name,
            std_negative,
            std_hms,
            dst_name: None,
            dst_negative: false,
            dst_hms: None,
            start: PosixBound::default(),
            end: PosixBound::default(),
        };
        if let Some(dst_name) = c.name() {
            zone.dst_name = Some(dst_name);
            let back = c.at;
            let negative = c.peek() == Some(b'-');
            if matches!(c.peek(), Some(b'-' | b'+')) {
                c.at += 1;
            }
            match c.clock() {
                Some(hms) => {
                    zone.dst_negative = negative;
                    zone.dst_hms = Some(hms);
                }
                None => c.at = back,
            }
            if c.eat(b',') {
                zone.start = c.bound()?;
                if !c.eat(b',') {
                    return None;
                }
                zone.end = c.bound()?;
            }
        }
        (c.at == c.text.len()).then_some(zone)
    }

    /// `ProcessPosixTimeZone`: the standard state, and the transitions of
    /// every year from 1916 to 2099 when the zone keeps daylight time.
    fn process(mut self) -> (State, Vec<(i64, State)>) {
        let strip = |name: &str| -> String {
            match name.strip_prefix('<') {
                Some(inner) => inner[..inner.len().saturating_sub(1)].to_string(),
                None => name.to_string(),
            }
        };
        let signum = |negative: bool| if negative { 1 } else { -1 };
        let std_signum = signum(self.std_negative);
        let (std_hours, std_minutes, std_seconds) = self.std_hms;
        let std_offset = ((std_hours * 60 + std_minutes.unwrap_or(0)) * 60
            + std_seconds.unwrap_or(0))
            * std_signum;
        let standard = State {
            offset: std_offset as i32,
            abbreviation: strip(&self.std_name),
        };
        let Some(dst_name) = self.dst_name.as_deref() else {
            return (standard, Vec::new());
        };
        let dst_offset = match self.dst_hms {
            None => 3600 + std_offset,
            Some((h, m, s)) => {
                ((h * 60 + m.unwrap_or(0)) * 60 + s.unwrap_or(0)) * signum(self.dst_negative)
            }
        };
        let daylight = State {
            offset: dst_offset as i32,
            abbreviation: strip(dst_name),
        };
        // Without rules, the European ones for a zone up to twelve hours
        // west and the American ones otherwise.
        let european = (0..=12).contains(&(std_signum * std_hours));
        if self.start.day_of_year.is_none() && self.start.month.is_none() {
            self.start = PosixBound {
                month: Some(3),
                week_of_month: Some(if european { 5 } else { 2 }),
                day_of_week: Some(0),
                hours: Some(match (european, std_hours > 2) {
                    (true, false) => std_hours + 1,
                    _ => 2,
                }),
                minutes: Some(0),
                seconds: Some(0),
                ..PosixBound::default()
            };
        }
        if self.end.day_of_year.is_none() && self.end.month.is_none() {
            self.end = PosixBound {
                month: Some(if european { 10 } else { 11 }),
                week_of_month: Some(if european { 5 } else { 1 }),
                day_of_week: Some(0),
                hours: Some(match (european, std_hours > 2) {
                    (true, true) => 3,
                    (true, false) => std_hours + 2,
                    (false, _) => 2,
                }),
                minutes: Some(0),
                seconds: Some(0),
                ..PosixBound::default()
            };
        }
        let mut transitions = Vec::new();
        for year in 1916..2100 {
            let start = posix_dst_time(&self.start, year) - std_offset;
            let end = posix_dst_time(&self.end, year) - dst_offset;
            if start < end {
                transitions.push((start, daylight.clone()));
                transitions.push((end, standard.clone()));
            } else {
                transitions.push((end, standard.clone()));
                transitions.push((start, daylight.clone()));
            }
        }
        (standard, transitions)
    }
}

/// `DeterminePosixDSTTime`: the wall-clock instant a rule names in a year.
fn posix_dst_time(bound: &PosixBound, year: i64) -> i64 {
    let julian_day = match bound.day_of_year {
        Some(day_of_year) => {
            // `Jn` does not count February 29, so a day past February 28 of
            // a leap year moves one on. `n` is taken as it stands.
            let day_of_year = if bound.julian && is_leap(year) && day_of_year > 58 {
                day_of_year + 1
            } else {
                day_of_year
            };
            julian_day_of(year, 1, 1) + day_of_year - 1
        }
        None => {
            // `GetJulianDayFromEraYearMonthWeekDay`: count from the zeroth day
            // of the month, or back from the seventh of the next for the last
            // week (any week from 5 up).
            let month = bound.month.unwrap_or(1);
            let week = match bound.week_of_month.unwrap_or(0) {
                w if w >= 5 => -1,
                w => w,
            };
            let reference = if week >= 0 {
                julian_day_of(year, month, 0)
            } else {
                julian_day_of(year, month + 1, 7)
            };
            let weekday = bound.day_of_week.unwrap_or(0);
            let k = (weekday + 6).rem_euclid(7);
            let on_or_before = reference - (reference - k).rem_euclid(7);
            on_or_before + 7 * week
        }
    };
    let time_of_day = (bound.hours.unwrap_or(2) * 60 + bound.minutes.unwrap_or(0)) * 60
        + bound.seconds.unwrap_or(0);
    (julian_day - JDN_OF_EPOCH) * SECONDS_PER_DAY + time_of_day
}

/// `GetJulianDayFromEraYearMonthDay` in the Gregorian calendar: the month is
/// reduced modulo 12 into the year and the day is added as it stands, so
/// neither has to be in range.
fn julian_day_of(year: i64, month: i64, day: i64) -> i64 {
    let months = month - 1;
    let year = year + months.div_euclid(12);
    let month = months.rem_euclid(12) as u32 + 1;
    days_from_civil(year, month, 1) + day - 1 + JDN_OF_EPOCH
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
    // A name with no colon that reads as a POSIX `TZ` string is one, even
    // when a zone file has the same name — `SetupTimeZone` tries it first.
    if !name.starts_with(':') {
        if let Some(posix) = PosixZone::parse(name) {
            let (initial, transitions) = posix.process();
            return Ok(Zone {
                transitions,
                initial,
            });
        }
    }
    // `LoadZoneinfoFile` refuses a name that could leave the zone directory:
    // an absolute path, a drive or volume prefix, or a `..` component.
    if unsafe_zone_path(trimmed) {
        return Err(format!("time zone \":{trimmed}\" not valid"));
    }
    for directory in ZONE_DIRECTORIES {
        let path = std::path::Path::new(directory).join(trimmed);
        if let Ok(bytes) = std::fs::read(&path) {
            if let Some(zone) = parse_tzif(&bytes) {
                return Ok(zone);
            }
        }
    }
    // A bare name no file has may be one of the legacy abbreviations, each of
    // which stands for a fixed offset named by its digits.
    if !name.starts_with(':') {
        let lower = name.to_lowercase();
        if let Some((_, offset)) = LEGACY_ZONES.iter().find(|(abbrev, _)| *abbrev == lower) {
            let seconds = fixed_offset(offset).expect("the legacy table holds offsets");
            return Ok(Zone::fixed(seconds, offset));
        }
    }
    Err(format!("time zone \":{trimmed}\" not found"))
}

/// `LoadZoneinfoFile`'s guard, `^[/\\]|^[a-zA-Z]+:|(?:^|[/\\])\.\.`.
fn unsafe_zone_path(name: &str) -> bool {
    let drive = name
        .find(':')
        .is_some_and(|at| at > 0 && name[..at].bytes().all(|b| b.is_ascii_alphabetic()));
    name.starts_with(['/', '\\'])
        || drive
        || name.starts_with("..")
        || name.contains("/..")
        || name.contains("\\..")
}

/// `LegacyTimeZone` (`library/clock.tcl`): the abbreviations a bare
/// `-timezone` falls back on when neither a POSIX rule nor a zone file reads
/// it.
const LEGACY_ZONES: &[(&str, &str)] = &[
    ("gmt", "+0000"),
    ("ut", "+0000"),
    ("utc", "+0000"),
    ("bst", "+0100"),
    ("wet", "+0000"),
    ("wat", "-0100"),
    ("at", "-0200"),
    ("nft", "-0330"),
    ("nst", "-0330"),
    ("ndt", "-0230"),
    ("ast", "-0400"),
    ("adt", "-0300"),
    ("est", "-0500"),
    ("edt", "-0400"),
    ("cst", "-0600"),
    ("cdt", "-0500"),
    ("mst", "-0700"),
    ("mdt", "-0600"),
    ("pst", "-0800"),
    ("pdt", "-0700"),
    ("yst", "-0900"),
    ("ydt", "-0800"),
    ("akst", "-0900"),
    ("akdt", "-0800"),
    ("hst", "-1000"),
    ("hdt", "-0900"),
    ("cat", "-1000"),
    ("ahst", "-1000"),
    ("nt", "-1100"),
    ("idlw", "-1200"),
    ("cet", "+0100"),
    ("cest", "+0200"),
    ("met", "+0100"),
    ("mewt", "+0100"),
    ("mest", "+0200"),
    ("swt", "+0100"),
    ("sst", "+0200"),
    ("fwt", "+0100"),
    ("fst", "+0200"),
    ("eet", "+0200"),
    ("eest", "+0300"),
    ("bt", "+0300"),
    ("it", "+0330"),
    ("zp4", "+0400"),
    ("zp5", "+0500"),
    ("ist", "+0530"),
    ("zp6", "+0600"),
    ("wast", "+0700"),
    ("wadt", "+0800"),
    ("jt", "+0730"),
    ("cct", "+0800"),
    ("jst", "+0900"),
    ("kst", "+0900"),
    ("cast", "+0930"),
    ("jdt", "+1000"),
    ("kdt", "+1000"),
    ("cadt", "+1030"),
    ("east", "+1000"),
    ("eadt", "+1030"),
    ("gst", "+1000"),
    ("nzt", "+1200"),
    ("nzst", "+1200"),
    ("nzdt", "+1300"),
    ("idle", "+1200"),
    ("a", "+0100"),
    ("b", "+0200"),
    ("c", "+0300"),
    ("d", "+0400"),
    ("e", "+0500"),
    ("f", "+0600"),
    ("g", "+0700"),
    ("h", "+0800"),
    ("i", "+0900"),
    ("k", "+1000"),
    ("l", "+1100"),
    ("m", "+1200"),
    ("n", "-0100"),
    ("o", "-0200"),
    ("p", "-0300"),
    ("q", "-0400"),
    ("r", "-0500"),
    ("s", "-0600"),
    ("t", "-0700"),
    ("u", "-0800"),
    ("v", "-0900"),
    ("w", "-1000"),
    ("x", "-1100"),
    ("y", "-1200"),
    ("z", "+0000"),
];

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

/// `::tcl::clock::LocalizeFormat`'s substitution list for one catalogue
/// (`library/clock.tcl:852`), in its order. Each entry is expanded through the
/// entries already in the list before it joins them, which is what lets `%c`
/// be written in terms of `%X` and `%X` in terms of `%T`.
fn format_map(cat: &Catalog) -> Vec<(String, String)> {
    let mut map = vec![
        ("%%".to_string(), "%%".to_string()),
        ("%D".to_string(), "%m/%d/%Y".to_string()),
        ("%+".to_string(), "%a %b %e %H:%M:%S %Z %Y".to_string()),
    ];
    for (key, value) in [
        ("%EY", &cat.locale_year_format),
        ("%T", &cat.time_format_24_secs),
        ("%R", &cat.time_format_24),
        ("%r", &cat.time_format_12),
        ("%X", &cat.time_format),
        ("%EX", &cat.locale_time_format),
        ("%x", &cat.date_format),
        ("%Ex", &cat.locale_date_format),
        ("%c", &cat.date_time_format),
        ("%Ec", &cat.locale_date_time_format),
    ] {
        let expanded = string_map(&map, value);
        map.push((key.to_string(), expanded));
    }
    map
}

/// Tcl's `string map`: one left-to-right pass over the subject in which the
/// first pair that matches at a position wins and its replacement is not
/// rescanned. `%%` maps to itself, so an escaped percent cannot start a group.
fn string_map(map: &[(String, String)], text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    'outer: while !rest.is_empty() {
        for (from, to) in map {
            if rest.starts_with(from.as_str()) {
                out.push_str(to);
                rest = &rest[from.len()..];
                continue 'outer;
            }
        }
        let ch = rest.chars().next().expect("not empty");
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    out
}

/// A `string map` pair list: what to look for and what to write instead.
type Substitutions = Arc<Vec<(String, String)>>;

thread_local! {
    /// `::tcl::clock::LocFmtMap`: the substitution list per locale, since
    /// building it reads ten catalogue entries and expands each one.
    static LOC_FMT_MAP: std::cell::RefCell<std::collections::HashMap<String, Substitutions>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Expand the locale format groups the way `LocalizeFormat` does.
fn localize(format: &str, cat: &Catalog) -> String {
    let map = LOC_FMT_MAP.with(|cache| {
        if let Some(hit) = cache.borrow().get(&cat.name) {
            return hit.clone();
        }
        let built = Arc::new(format_map(cat));
        cache.borrow_mut().insert(cat.name.clone(), built.clone());
        built
    });
    string_map(&map, format)
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

fn format_time(seconds: i64, format: &str, zone: &Zone, cat: &Catalog) -> Result<String, String> {
    if seconds < EARLIEST {
        return Err(too_early());
    }
    let state = zone.at(seconds);
    let local = seconds
        .checked_add(state.offset as i64)
        .ok_or_else(overflow)?;
    let civil = civil_of(local);
    let expanded = localize(format, cat);
    let mut out = String::with_capacity(expanded.len() + 16);
    let mut rest = expanded.as_str();
    while let Some(at) = rest.find('%') {
        out.push_str(&rest[..at]);
        rest = &rest[at + 1..];
        // `%E` and `%O` select a token map of their own, and a character that
        // is in neither this map nor the plain one leaves the whole group —
        // percent, modifier and all — in the output.
        let modifier = rest.chars().next().filter(|c| matches!(c, 'E' | 'O'));
        let after = match modifier {
            Some(m) => &rest[m.len_utf8()..],
            None => rest,
        };
        let Some(token) = after.chars().next() else {
            // A trailing `%` is itself, as tclsh's scanner leaves it.
            out.push('%');
            if let Some(m) = modifier {
                out.push(m);
            }
            return Ok(out);
        };
        match one_token(modifier, token, &civil, seconds, local, state, cat)? {
            Some(text) => {
                out.push_str(&text);
                rest = &after[token.len_utf8()..];
            }
            // A token that is in no map is copied through unchanged, which is
            // what tclsh does for `%F` and `%i` — measured, not assumed.
            // `rest` still points at the modifier, or at the token when there
            // was none, so the next pass copies the rest of the group.
            None => out.push('%'),
        }
    }
    out.push_str(rest);
    Ok(out)
}

/// One `%`-token's text, or `Ok(None)` when the token is in no map — in which
/// case the caller copies the group through. `modifier` is the `E` or `O` that
/// picked `FmtETokenMap` or `FmtOTokenMap` over `FmtSTokenMap`. The error is
/// [`indexed`]'s.
fn one_token(
    modifier: Option<char>,
    token: char,
    civil: &Civil,
    seconds: i64,
    local: i64,
    state: &State,
    cat: &Catalog,
) -> Result<Option<String>, String> {
    let weekday = civil.iso_weekday();
    let hour12 = match civil.hour % 12 {
        0 => 12,
        other => other,
    };
    Ok(match modifier {
        // `FmtETokenMap`, whose index is `EJjys` with `C` aliased onto `y`.
        Some('E') => Some(match token {
            'E' => if civil.year <= 0 { &cat.bce } else { &cat.ce }.clone(),
            'J' => julian_fraction(civil.julian_day(), civil.second_of_day(), 0),
            'j' => julian_fraction(
                civil.julian_day(),
                civil.second_of_day(),
                SECONDS_PER_DAY / 2,
            ),
            'y' | 'C' => era_year(token, civil, local, cat),
            's' => local.to_string(),
            _ => return Ok(None),
        }),
        // `FmtOTokenMap`, whose index is `dmyHIMSuw` with `ekl` aliased onto
        // `dHI`. Every entry writes its value as a locale numeral.
        Some('O') => {
            let value = match token {
                'd' | 'e' => civil.day as i64,
                'm' => civil.month as i64,
                'y' => civil.year.rem_euclid(100),
                'H' | 'k' => civil.hour as i64,
                'I' | 'l' => hour12 as i64,
                'M' => civil.minute as i64,
                'S' => civil.second as i64,
                'u' => weekday as i64,
                'w' => (weekday % 7) as i64,
                _ => return Ok(None),
            };
            Some(indexed(&cat.numerals, value)?)
        }
        // `FmtSTokenMap`.
        _ => Some(match token {
            '%' => "%".to_string(),
            'd' => pad(civil.day as i64, 2, '0'),
            'e' => pad(civil.day as i64, 2, ' '),
            'm' => pad(civil.month as i64, 2, '0'),
            'N' => pad(civil.month as i64, 2, ' '),
            'b' | 'h' => indexed(&cat.months_abbrev, civil.month as i64 - 1)?,
            'B' => indexed(&cat.months_full, civil.month as i64 - 1)?,
            'y' => pad(civil.year.rem_euclid(100), 2, '0'),
            'Y' => pad(civil.year, 4, '0'),
            'C' => pad(civil.year.div_euclid(100), 2, '0'),
            'H' => pad(civil.hour as i64, 2, '0'),
            'M' => pad(civil.minute as i64, 2, '0'),
            'S' => pad(civil.second as i64, 2, '0'),
            'I' => pad(hour12 as i64, 2, '0'),
            'k' => pad(civil.hour as i64, 2, ' '),
            'l' => pad(hour12 as i64, 2, ' '),
            // `%p` is the catalogue's word upper-cased and `%P` is the word as
            // it stands — `ClockFmtToken_AMPM_Proc` reads the same two entries
            // for both and only `p` calls `Tcl_UtfToUpper`.
            'p' => meridiem(civil, cat).to_uppercase(),
            'P' => meridiem(civil, cat).to_string(),
            'a' => indexed(&cat.days_abbrev, (weekday % 7) as i64)?,
            'A' => indexed(&cat.days_full, (weekday % 7) as i64)?,
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
            'Q' => stardate(civil),
            _ => return Ok(None),
        }),
    })
}

/// One entry of a catalogue list. Past its end tclsh's `Tcl_ListObjIndex`
/// fails and `ClockFormat` reports that with no message at all — measured:
/// `clock format 946684800 -format %a -gmt 1 -locale mt` raises an empty error
/// under `errorCode NONE`, because `mt.msg` ships six weekday abbreviations
/// and that instant is a Saturday.
fn indexed(list: &[String], at: i64) -> Result<String, String> {
    usize::try_from(at)
        .ok()
        .and_then(|i| list.get(i))
        .cloned()
        .ok_or_else(String::new)
}

/// The catalogue's `AM` or `PM` word for this time of day.
fn meridiem<'c>(civil: &Civil, cat: &'c Catalog) -> &'c str {
    if civil.hour < 12 {
        &cat.am
    } else {
        &cat.pm
    }
}

/// `%EC` and `%Ey` — `ClockFmtToken_LocaleERAYear_Proc`. With no era covering
/// the instant the two are the century and the year within it; with one they
/// are the era's name and the year counted from the era's own epoch, that year
/// written as a locale numeral while it fits in two digits.
fn era_year(token: char, civil: &Civil, local: i64, cat: &Catalog) -> String {
    let Some(era) = cat.era_at(local) else {
        return if token == 'C' {
            pad(civil.year.div_euclid(100), 2, '0')
        } else {
            pad(civil.year.rem_euclid(100), 2, '0')
        };
    };
    if token == 'C' {
        return era.name.clone();
    }
    let year = civil.year - era.year;
    match usize::try_from(year).ok().and_then(|y| cat.numerals.get(y)) {
        Some(numeral) => numeral.clone(),
        None => pad(year, 2, '0'),
    }
}

/// `%EJ` and `%Ej` — `ClockFmtToken_JDN_Proc`. The Julian day with the time of
/// day as a fraction, `offset` being the moment the day is reckoned from:
/// midnight for the calendar day number and noon for the astronomical one.
fn julian_fraction(julian_day: i64, second_of_day: i64, offset: i64) -> String {
    let mut day = julian_day;
    let mut fraction = second_of_day - offset;
    if fraction < 0 {
        day -= 1;
        fraction += SECONDS_PER_DAY;
    }
    let mut sign = "";
    if fraction != 0 && day < 0 {
        // Stepping the integer part towards zero would lose the sign of a
        // day that rounds to `-0`, so the sign is written out instead.
        day += 1;
        if day == 0 {
            sign = "-";
        }
        fraction = SECONDS_PER_DAY - fraction;
    }
    if fraction == 0 || fraction == SECONDS_PER_DAY / 2 {
        let half = if fraction == 0 { '0' } else { '5' };
        return format!("{sign}{day}.{half}");
    }
    // Eight digits, rounded, with the trailing zeroes cut.
    let scaled = (fraction as f64 * 100_000_000.0 / SECONDS_PER_DAY as f64 + 0.5) as i64;
    let digits = pad(scaled, 8, '0');
    format!("{sign}{day}.{}", digits.trim_end_matches('0'))
}

/// `%Q` — `ClockFmtToken_StarDate_Proc`, whose epoch is 1946.
fn stardate(civil: &Civil) -> String {
    let day = civil.day_of_year() - 1;
    let year_length = if is_leap(civil.year) { 366 } else { 365 };
    let fraction_of_year = 1000 * day / year_length;
    let tenth = civil.second_of_day() / (SECONDS_PER_DAY / 10);
    format!(
        "Stardate {}{}.{}",
        pad(civil.year - 1946, 2, '0'),
        pad(fraction_of_year, 3, '0'),
        pad(if tenth < 0 { 10 + tenth } else { tenth }, 1, '0')
    )
}

// ── scanning ─────────────────────────────────────────────────────────────

/// The flags a scan token raises — `CLF_*` of `generic/tclDate.h`, under the
/// same names. Which of them the input carried decides how the fields are
/// assembled into an instant, exactly as in `ClockScan` and `ClockScanCommit`.
mod flag {
    pub const POSIXSEC: u32 = 1 << 1;
    pub const LOCALSEC: u32 = 1 << 2;
    pub const JULIANDAY: u32 = 1 << 3;
    pub const TIME: u32 = 1 << 4;
    pub const CENTURY: u32 = 1 << 6;
    pub const DAYOFMONTH: u32 = 1 << 7;
    pub const DAYOFYEAR: u32 = 1 << 8;
    pub const MONTH: u32 = 1 << 9;
    pub const YEAR: u32 = 1 << 10;
    pub const DAYOFWEEK: u32 = 1 << 11;
    pub const ISO8601YEAR: u32 = 1 << 12;
    pub const ISO8601WEEK: u32 = 1 << 13;
    pub const ISO8601CENTURY: u32 = 1 << 14;
    pub const DATE: u32 =
        JULIANDAY | DAYOFMONTH | DAYOFYEAR | MONTH | YEAR | ISO8601YEAR | DAYOFWEEK | ISO8601WEEK;
}

/// `ClockDefaultCenturySwitch` and `ClockDefaultYearCentury`: a two-digit
/// year from 38 up is in the 1900s, one below it in the 2000s.
const YEAR_OF_CENTURY_SWITCH: i64 = 38;
const CURRENT_YEAR_CENTURY: i64 = 2000;

/// `GREGORIAN_CHANGE_DATE`, as a Julian Day Number.
const GREGORIAN_CHANGE_JDN: i64 = 2_361_222;

/// The Julian Day Number of 1970-01-01.
const JDN_OF_EPOCH: i64 = 2_440_588;

/// `MERIDIAN`: whether `%p` said AM or PM, or nothing did.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Meridian {
    Am,
    Pm,
    H24,
}

/// The fields a `-format` scan fills in — `DateInfo` and its `TclDateFields`.
/// Every date field starts out as the base date's, which is what a field the
/// format does not carry is taken from (`ClockParseFmtScnArgs`); which ones the
/// input did carry is in `flags`.
struct Scanned {
    flags: u32,
    year: i64,
    /// `dateCentury`, which only `%C` sets.
    century: i64,
    month: i64,
    day: i64,
    day_of_year: i64,
    iso_year: i64,
    iso_week: i64,
    /// 1 for Monday through 7 for Sunday.
    weekday: i64,
    julian_day: i64,
    /// `%EE`: the year is counted before the common era.
    bce: bool,
    hour: i64,
    minute: i64,
    second: i64,
    meridian: Meridian,
    second_of_day: i64,
    /// Local seconds since the epoch: the base's, or `%Es` and `%Q`'s.
    local_seconds: i64,
    /// `%s`, and a Julian day written with a fraction: the instant itself.
    seconds: i64,
    /// A zone the input named, which replaces the command's own.
    offset: Option<i32>,
}

impl Scanned {
    /// The fields of the base instant in the zone, as `ClockGetDateFields`
    /// fills them in before the input is read.
    fn from_base(base_at: i64, zone: &Zone) -> Scanned {
        let local = base_at + zone.at(base_at).offset as i64;
        let civil = civil_of(local);
        let (iso_year, iso_week) = civil.iso_week();
        Scanned {
            flags: 0,
            year: civil.year,
            century: 0,
            month: civil.month as i64,
            day: civil.day as i64,
            day_of_year: civil.day_of_year(),
            iso_year,
            iso_week,
            weekday: civil.iso_weekday() as i64,
            julian_day: civil.julian_day(),
            bce: false,
            // `ClockScanObjCmd` resets the time of day before scanning.
            hour: 0,
            minute: 0,
            second: 0,
            meridian: Meridian::H24,
            second_of_day: 0,
            local_seconds: local,
            seconds: base_at,
            offset: None,
        }
    }

    /// The astronomical year — `1 - year` before the common era.
    fn absolute_year(&self) -> i64 {
        if self.bce {
            1 - self.year
        } else {
            self.year
        }
    }
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

/// Lower-case one character, one for one — `Tcl_UtfToLower`, which the index
/// tree's keys and the input are both put through. A folding that expanded a
/// character into two would move the input position off the character it
/// matched, so only the first is taken.
fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Match the leading run of the input that names exactly one of the tables'
/// entries, where the tables share an index space — `TclStrIdxTreeSearch` over
/// the radix trie `ClockMCGetMultiListIdxTree` builds from the abbreviated and
/// the full list together (`generic/tclStrIdxTree.c:91`). A node a split
/// created carries a value only when its whole subtree agrees on one, so the
/// search reads as far as the input keeps matching some entry and answers only
/// if everything still matching means the same thing.
///
/// A name may therefore be abbreviated as far as it stays unique. Measured
/// against tclsh: `clock scan "13 f 2009" -format {%d %b %Y} -locale fr` is
/// February, because `févr.` is the only French month beginning with `f`;
/// `j` is refused because `janv.`, `juin` and `juil.` all do, and `ju` because
/// two still do. `Marc` is March, one character short of the full name.
fn take_prefix(text: &[char], at: &mut usize, tables: &[&[String]]) -> Option<usize> {
    let matches = |entry: &str, len: usize| {
        entry.chars().count() >= len
            && entry
                .chars()
                .take(len)
                .enumerate()
                .all(|(i, e)| text.get(*at + i).is_some_and(|&c| lower(c) == lower(e)))
    };
    // How far the input goes on matching some entry, which is where the walk
    // through the tree stops.
    let mut len = 0;
    for entry in tables.iter().flat_map(|t| t.iter()) {
        let reached = entry
            .chars()
            .enumerate()
            .take_while(|(i, e)| text.get(*at + i).is_some_and(|&c| lower(c) == lower(*e)))
            .count();
        len = len.max(reached);
    }
    if len == 0 {
        return None;
    }
    // Everything still matching there has to mean one thing.
    let mut value = None;
    for table in tables {
        for (index, entry) in table.iter().enumerate() {
            if !matches(entry, len) {
                continue;
            }
            if value.is_some_and(|found| found != index) {
                return None;
            }
            value = Some(index);
        }
    }
    *at += len;
    value
}

/// Match one of a table's entries case-insensitively, longest first so that
/// `January` is not read as `Jan` with `uary` left over.
fn take_name<S: AsRef<str>>(text: &[char], at: &mut usize, table: &[S]) -> Option<usize> {
    let mut best: Option<(usize, usize)> = None;
    for (i, name) in table.iter().enumerate() {
        let chars: Vec<char> = name.as_ref().chars().collect();
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

fn scan_time(
    input: &str,
    format: &str,
    zone: &Zone,
    cat: &Catalog,
    base_at: i64,
    validate: bool,
) -> Result<i64, String> {
    let text: Vec<char> = input.chars().collect();
    let pattern: Vec<char> = localize(format, cat).chars().collect();
    let mut got = Scanned::from_base(base_at, zone);
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
        // `%E` and `%O` select a scan map of their own, exactly as they select
        // a format map — `ScnETokenMap` and `ScnOTokenMap`.
        let modifier = pattern.get(p).copied().filter(|c| matches!(c, 'E' | 'O'));
        if modifier.is_some() {
            p += 1;
        }
        let Some(token) = pattern.get(p).copied() else {
            return Err(no_match());
        };
        p += 1;
        if let Some(m) = modifier {
            if scan_modified(m, token, &text, &mut at, &mut got, cat)? {
                continue;
            }
            return Err(format!(
                "clock scan: the format token \"%{m}{token}\" is not supported yet"
            ));
        }
        if NUMERIC_TOKENS.contains(token) {
            while at < text.len() && text[at] == ' ' {
                at += 1;
            }
        }
        let digits = |at: &mut usize, max: usize| take_digits(&text, at, max).ok_or_else(no_match);
        // `ScnSTokenMap`'s `minSize`: `%Y` and `%G` need all four digits and
        // `%g` both of its two, so `clock scan 70 -format %Y` does not match.
        let exactly = |at: &mut usize, size: usize| {
            let start = *at;
            let value = take_digits(&text, at, size).ok_or_else(no_match)?;
            if *at - start < size {
                return Err(no_match());
            }
            Ok(value)
        };
        let raised = match token {
            '%' => {
                if text.get(at) != Some(&'%') {
                    return Err(no_match());
                }
                at += 1;
                0
            }
            'n' | 't' => {
                if !text.get(at).is_some_and(|c| c.is_whitespace()) {
                    return Err(no_match());
                }
                at += 1;
                0
            }
            'd' | 'e' => {
                got.day = digits(&mut at, 2)?;
                flag::DAYOFMONTH
            }
            'm' | 'N' => {
                got.month = digits(&mut at, 2)?;
                flag::MONTH
            }
            'b' | 'h' | 'B' => {
                let index = take_prefix(&text, &mut at, &[&cat.months_full, &cat.months_abbrev])
                    .ok_or_else(no_match)?;
                got.month = index as i64 + 1;
                flag::MONTH
            }
            'a' | 'A' => {
                // The list is Sunday-first and `dayOfWeek` is Monday-first, so
                // `ClockScnToken_DayOfWeek_Proc` decrements the 1-based index
                // it gets and reads a resulting 0 as 7.
                let index = take_prefix(&text, &mut at, &[&cat.days_full, &cat.days_abbrev])
                    .ok_or_else(no_match)?;
                got.weekday = if index == 0 { 7 } else { index as i64 };
                flag::DAYOFWEEK
            }
            'y' => {
                got.year = digits(&mut at, 2)?;
                flag::YEAR
            }
            'Y' => {
                got.year = exactly(&mut at, 4)?;
                flag::YEAR | flag::CENTURY
            }
            'C' => {
                got.century = digits(&mut at, 2)?;
                flag::CENTURY | flag::ISO8601CENTURY
            }
            // `%I` and `%l` are aliases of `%H`: the hour is read on the
            // 24-hour clock unless a `%p` says otherwise.
            'H' | 'k' | 'I' | 'l' => {
                got.hour = digits(&mut at, 2)?;
                flag::TIME
            }
            'M' => {
                got.minute = digits(&mut at, 2)?;
                flag::TIME
            }
            'S' => {
                got.second = digits(&mut at, 2)?;
                flag::TIME
            }
            'j' => {
                got.day_of_year = digits(&mut at, 3)?;
                flag::DAYOFYEAR
            }
            'p' | 'P' => {
                let index = take_prefix(&text, &mut at, &[&[cat.am.clone(), cat.pm.clone()]])
                    .ok_or_else(no_match)?;
                got.meridian = if index == 1 {
                    Meridian::Pm
                } else {
                    Meridian::Am
                };
                0
            }
            's' => {
                got.seconds = signed(&text, &mut at)?;
                flag::POSIXSEC
            }
            'u' | 'w' => {
                let day = digits(&mut at, 1)?;
                if day > 7 {
                    return Err("day of week is greater than 7".to_string());
                }
                // `%w` numbers Sunday 0 and `%u` numbers it 7; both reach
                // `dayOfWeek` through the same `if (val == 0) val = 7`.
                got.weekday = if day == 0 { 7 } else { day };
                flag::DAYOFWEEK
            }
            // Parse-only: `%U` and `%W` capture nothing.
            'U' | 'W' => {
                digits(&mut at, 2)?;
                0
            }
            'V' => {
                got.iso_week = digits(&mut at, 2)?;
                flag::ISO8601WEEK
            }
            'G' => {
                got.iso_year = exactly(&mut at, 4)?;
                flag::ISO8601YEAR | flag::ISO8601CENTURY
            }
            'g' => {
                got.iso_year = exactly(&mut at, 2)?;
                flag::ISO8601YEAR
            }
            'z' | 'Z' => {
                got.offset = Some(scan_zone(&text, &mut at)?);
                0
            }
            // A whole Julian Day Number, which names a local day.
            'J' => {
                got.julian_day = signed(&text, &mut at)?;
                flag::JULIANDAY
            }
            'Q' => {
                scan_stardate(&text, &mut at, &mut got)?;
                flag::LOCALSEC
            }
            other => {
                return Err(format!(
                    "clock scan: the format token \"%{other}\" is not supported yet"
                ))
            }
        };
        got.flags |= raised;
    }
    while at < text.len() && text[at].is_whitespace() {
        at += 1;
    }
    if at != text.len() {
        return Err(no_match());
    }
    assemble(got, zone, validate)
}

/// A signed run of digits — `%s`, `%J` and the integer part of a Julian day.
/// The sign is read first because `Clock_str2wideInt` is handed one.
fn signed(text: &[char], at: &mut usize) -> Result<i64, String> {
    let negative = text.get(*at) == Some(&'-');
    if negative || text.get(*at) == Some(&'+') {
        *at += 1;
    }
    let value = take_digits(text, at, 19).ok_or_else(no_match)?;
    Ok(if negative { -value } else { value })
}

/// A token under an `%E` or `%O` modifier — `ScnETokenMap` and
/// `ScnOTokenMap`. `false` means the token is in neither map, which the caller
/// turns into its refusal.
///
/// Every `%O` entry reads a locale numeral rather than digits, which is why
/// `clock scan 5 -format %Od` fails where `clock scan 05 -format %Od` answers:
/// the root catalogue's numerals are `00` through `99` and nothing matches a
/// bare `5` (measured against tclsh).
fn scan_modified(
    modifier: char,
    token: char,
    text: &[char],
    at: &mut usize,
    got: &mut Scanned,
    cat: &Catalog,
) -> Result<bool, String> {
    if modifier == 'O' {
        // `ScnOTokenMapIndex` is `dmyHMSu`, with `ekIlw` aliased onto `dHHHu`.
        let numeral = |at: &mut usize| {
            take_prefix(text, at, &[&cat.numerals])
                .map(|n| n as i64)
                .ok_or_else(no_match)
        };
        got.flags |= match token {
            'd' | 'e' => {
                got.day = numeral(at)?;
                flag::DAYOFMONTH
            }
            'm' => {
                got.month = numeral(at)?;
                flag::MONTH
            }
            'y' => {
                got.year = numeral(at)?;
                flag::YEAR
            }
            'H' | 'k' | 'I' | 'l' => {
                got.hour = numeral(at)?;
                flag::TIME
            }
            'M' => {
                got.minute = numeral(at)?;
                flag::TIME
            }
            'S' => {
                got.second = numeral(at)?;
                flag::TIME
            }
            // `ClockScnToken_DayOfWeek_Proc` with a locale list: the numeral's
            // index is the weekday, and 0 means Sunday as everywhere else.
            'u' | 'w' => {
                let day = numeral(at)?;
                if day > 7 {
                    return Err("day of week is greater than 7".to_string());
                }
                got.weekday = if day == 0 { 7 } else { day };
                flag::DAYOFWEEK
            }
            _ => return Ok(false),
        };
        return Ok(true);
    }
    // `ScnETokenMapIndex` is `EJjys`.
    match token {
        'E' => got.bce = !scan_era(text, at, cat).ok_or_else(no_match)?,
        // `ClockScnToken_JDN_Proc`: the calendar day number and the
        // astronomical one, which starts at noon. A whole calendar day names
        // a local day; anything else is the instant itself, `CLF_POSIXSEC`,
        // and no zone applies.
        'J' | 'j' => {
            let offset = if token == 'j' { SECONDS_PER_DAY / 2 } else { 0 };
            let mut day = signed(text, at)?;
            got.flags |= flag::JULIANDAY;
            let fraction = scan_day_fraction(text, at);
            if fraction.is_none() && token == 'J' {
                got.julian_day = day;
                return Ok(true);
            }
            let mut seconds = offset + fraction.unwrap_or(0);
            if seconds >= SECONDS_PER_DAY {
                seconds %= SECONDS_PER_DAY;
                day += 1;
            }
            got.julian_day = day;
            got.second_of_day = seconds;
            got.seconds = (day - JDN_OF_EPOCH) * SECONDS_PER_DAY + seconds;
            got.flags |= flag::POSIXSEC;
        }
        // Parse-only: `ScnETokenMap`'s `%Ey` entry captures nothing, so a
        // matched numeral moves the input on and changes no field.
        'y' => {
            take_prefix(text, at, &[&cat.numerals]).ok_or_else(no_match)?;
        }
        's' => {
            got.local_seconds = signed(text, at)?;
            got.flags |= flag::LOCALSEC;
        }
        _ => return Ok(false),
    }
    Ok(true)
}

/// The `.ddd` of a Julian day, as a count of seconds into the day. `None` when
/// no fraction follows, which is a whole Julian day number.
fn scan_day_fraction(text: &[char], at: &mut usize) -> Option<i64> {
    if text.get(*at) != Some(&'.') {
        return None;
    }
    let start = *at + 1;
    let mut end = start;
    let mut divisor: i64 = 1;
    while text.get(end).is_some_and(|c| c.is_ascii_digit()) {
        divisor = divisor.saturating_mul(10);
        end += 1;
    }
    let mut value: i64 = 0;
    for c in &text[start..end] {
        value = value * 10 + c.to_digit(10).expect("a digit") as i64;
    }
    *at = end;
    Some(SECONDS_PER_DAY * value / divisor)
}

/// `%EE` — `ClockScnToken_LocaleERA_Proc`, which searches the catalogue's two
/// era words together with four fixed spellings: `b.c.e.`, `c.e.`, `b.c.` and
/// `a.d.`. `true` is the common era.
///
/// The answer is the era rather than the entry, because several entries mean
/// the same one: `b.c.` is a prefix of `b.c.e.` and both are before the common
/// era, so the input is not ambiguous even though the entry is.
fn scan_era(text: &[char], at: &mut usize, cat: &Catalog) -> Option<bool> {
    let table = [
        (cat.bce.as_str(), false),
        (cat.ce.as_str(), true),
        ("b.c.e.", false),
        ("c.e.", true),
        ("b.c.", false),
        ("a.d.", true),
    ];
    let mut len = 0;
    for (word, _) in table {
        let reached = word
            .chars()
            .enumerate()
            .take_while(|(i, w)| text.get(*at + i).is_some_and(|c| lower(*c) == lower(*w)))
            .count();
        len = len.max(reached);
    }
    if len == 0 {
        return None;
    }
    let mut era = None;
    for (word, common) in table {
        let matches = word.chars().count() >= len
            && word
                .chars()
                .take(len)
                .enumerate()
                .all(|(i, w)| text.get(*at + i).is_some_and(|c| lower(*c) == lower(w)));
        if matches {
            if era.is_some_and(|found| found != common) {
                return None;
            }
            era = Some(common);
        }
    }
    *at += len;
    era
}

/// `%Q` — `ClockScnToken_StarDate_Proc`. `Stardate NNNNN.d`: at least four
/// digits, of which the last three are the thousandths of the year elapsed and
/// the rest the year since 1946, then a fraction of the day.
fn scan_stardate(text: &[char], at: &mut usize, got: &mut Scanned) -> Result<(), String> {
    let prefix: Vec<char> = "stardate ".chars().collect();
    if text.len() < *at + prefix.len()
        || !text[*at..*at + prefix.len()]
            .iter()
            .zip(&prefix)
            .all(|(c, p)| lower(*c) == *p)
    {
        return Err(no_match());
    }
    let mut cursor = *at + prefix.len();
    while text.get(cursor).is_some_and(|c| c.is_whitespace()) {
        cursor += 1;
    }
    if text.get(cursor) == Some(&'+') {
        cursor += 1;
    }
    let start = cursor;
    while text.get(cursor).is_some_and(|c| c.is_ascii_digit()) {
        cursor += 1;
    }
    // The last three digits are the fraction of the year, so there has to be
    // at least one digit of year in front of them.
    if cursor - start < 4 {
        return Err(no_match());
    }
    let number = |slice: &[char]| -> i64 {
        slice
            .iter()
            .fold(0, |n, c| n * 10 + c.to_digit(10).expect("a digit") as i64)
    };
    let year = number(&text[start..cursor - 3]) + 1946;
    let elapsed = number(&text[cursor - 3..cursor]);
    if text.get(cursor) != Some(&'.') {
        return Err(no_match());
    }
    *at = cursor;
    let fraction = scan_day_fraction(text, at).ok_or_else(no_match)?;
    // The thousandths are of the whole year, rounded to a day.
    let length = if is_leap(year) { 366 } else { 365 };
    let scaled = elapsed * length;
    let day_of_year = scaled / 1000 + 1 + i64::from(scaled % 1000 >= 500);
    let day = days_from_civil(year, 1, 1) + day_of_year - 1;
    got.year = year;
    got.bce = false;
    got.day_of_year = day_of_year;
    got.julian_day = day + JDN_OF_EPOCH;
    got.local_seconds = day * SECONDS_PER_DAY + fraction;
    Ok(())
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

/// Turn scanned fields into an instant: the tail of `ClockScan` that settles
/// which fields take precedence, then `ClockScanCommit`, with
/// `ClockValidDate`'s two stages where tclsh runs them when `-validate` is on.
fn assemble(mut got: Scanned, zone: &Zone, validate: bool) -> Result<i64, String> {
    use flag::*;
    let mut flags = got.flags;
    let mut assemble_julian = false;
    let mut assemble_seconds = false;
    // `%s` takes precedence over every other token.
    if flags & POSIXSEC == 0 {
        if flags & DATE != 0 && flags & JULIANDAY == 0 {
            assemble_seconds = true;
            assemble_julian = true;
            // A day of the month or of the year wins over a bare weekday, and
            // with neither the weekday chooses the day within the ISO week.
            match flags & (MONTH | DAYOFYEAR | DAYOFMONTH) {
                f if f == DAYOFYEAR | DAYOFMONTH => {
                    flags &= !DAYOFMONTH;
                    if flags & ISO8601YEAR == 0 {
                        flags &= !ISO8601WEEK;
                    }
                }
                f if f == DAYOFYEAR
                    || f == MONTH | DAYOFYEAR | DAYOFMONTH
                    || f == MONTH | DAYOFMONTH
                    || f == DAYOFMONTH =>
                {
                    if flags & ISO8601YEAR == 0 {
                        flags &= !ISO8601WEEK;
                    }
                }
                0 if flags & DAYOFWEEK != 0 => flags |= ISO8601WEEK,
                _ => {}
            }
            // A year with a month and day, or with a day of the year, wins
            // over the ISO week unless the ISO year is the one written out.
            if flags & ISO8601WEEK != 0
                && (flags & (YEAR | DAYOFYEAR) == YEAR | DAYOFYEAR
                    || flags & (YEAR | DAYOFMONTH | MONTH) == YEAR | DAYOFMONTH | MONTH)
            {
                // A century beside a two-digit ISO year puts the ISO week
                // down; otherwise only an ISO year written out keeps it.
                let century_only = flags & ISO8601CENTURY == 0 && flags & CENTURY != 0;
                if century_only || flags & ISO8601YEAR == 0 {
                    flags &= !ISO8601WEEK;
                }
            }
            if flags & YEAR != 0 {
                got.year = widen_year(got.year, flags & CENTURY != 0, got.century);
            }
            if flags & (ISO8601WEEK | ISO8601YEAR) != 0 {
                if flags & (ISO8601YEAR | YEAR) == YEAR {
                    got.iso_year = got.year;
                } else {
                    got.iso_year =
                        widen_year(got.iso_year, flags & ISO8601CENTURY != 0, got.century);
                }
                if flags & (ISO8601YEAR | YEAR) == ISO8601YEAR {
                    got.year = got.iso_year;
                }
            }
        }
        // With no time in the input the day starts at midnight.
        if flags & (TIME | LOCALSEC) == 0 {
            assemble_seconds = true;
            got.local_seconds = 0;
        }
        if flags & TIME != 0 {
            assemble_seconds = true;
            got.second_of_day = to_seconds(got.hour, got.minute, got.second, got.meridian);
        } else if flags & LOCALSEC == 0 {
            assemble_seconds = true;
            got.second_of_day = got.local_seconds % SECONDS_PER_DAY;
        }
    }
    got.flags = flags;

    // ── ClockScanCommit ──
    let mut stage_one_done = false;
    if validate && (assemble_seconds || flags & LOCALSEC != 0) {
        validate_fields(&mut got, &mut assemble_julian)?;
        stage_one_done = true;
    }
    if assemble_julian {
        assemble_julian_day(&mut got)?;
    }
    if flags & JULIANDAY != 0 {
        let jdn = got.julian_day as f64
            + (got.second_of_day - SECONDS_PER_DAY / 2) as f64 / SECONDS_PER_DAY as f64;
        if jdn > MAX_JDN {
            return Err("requested date too large to represent".to_string());
        }
    }
    // 24:00, and a time past the end of the day, run into the next one.
    if got.second_of_day >= SECONDS_PER_DAY {
        got.julian_day += got.second_of_day / SECONDS_PER_DAY;
        got.second_of_day %= SECONDS_PER_DAY;
    }
    if assemble_seconds {
        got.local_seconds = (got.julian_day - JDN_OF_EPOCH) * SECONDS_PER_DAY + got.second_of_day;
    }
    if assemble_seconds || flags & LOCALSEC != 0 {
        let local = got.local_seconds;
        got.seconds = match got.offset {
            Some(offset) => local - offset as i64,
            None => local - zone.for_local(local).offset as i64,
        };
    }

    // ── the rest of ClockValidDate ──
    if validate {
        if !stage_one_done {
            validate_fields(&mut got, &mut assemble_julian)?;
        }
        if flags & DAYOFWEEK != 0 {
            let weekday = (got.julian_day - JDN_OF_EPOCH + 3).rem_euclid(7) + 1;
            if weekday != got.weekday {
                return Err(invalid_input("invalid day of week"));
            }
        }
    }
    Ok(got.seconds)
}

/// `maxJDN`, the largest Julian day `clock scan` accepts.
const MAX_JDN: f64 = 5_373_484.499_999_994;

/// A year written with fewer than three digits, placed in a century: the
/// one `%C` gave when it is written, or else the one the century switch picks.
fn widen_year(year: i64, has_century: bool, century: i64) -> i64 {
    if year >= 100 {
        return year;
    }
    if has_century {
        return year + century * 100;
    }
    let year = if year >= YEAR_OF_CENTURY_SWITCH {
        year - 100
    } else {
        year
    };
    year + CURRENT_YEAR_CENTURY
}

/// `TclToSeconds`: a time of day on the 24-hour clock, or on the 12-hour one
/// when `%p` named a half of the day.
fn to_seconds(hour: i64, minute: i64, second: i64, meridian: Meridian) -> i64 {
    let hour = match meridian {
        Meridian::H24 => hour,
        Meridian::Am => hour / 24 * 24 + hour % 12,
        Meridian::Pm => hour / 24 * 24 + hour % 12 + 12,
    };
    (hour * 60 + minute) * 60 + second
}

fn invalid_input(what: &str) -> String {
    format!("unable to convert input string: {what}")
}

/// `ClockAssembleJulianDay`: the day from the ISO week, from the month and
/// day, or from the day of the year, whichever the input settled on.
fn assemble_julian_day(got: &mut Scanned) -> Result<(), String> {
    use flag::*;
    let flags = got.flags;
    got.julian_day = if flags & ISO8601WEEK != 0 {
        // January 4 is always in week 1; its Monday starts the week count.
        let iso_year = if got.bce {
            1 - got.iso_year
        } else {
            got.iso_year
        };
        let fourth = days_from_civil(iso_year, 1, 4) + JDN_OF_EPOCH;
        let first_monday = fourth - (fourth % 7);
        first_monday + 7 * (got.iso_week - 1) + got.weekday - 1
    } else if flags & DAYOFYEAR == 0 || flags & (DAYOFMONTH | MONTH) == DAYOFMONTH | MONTH {
        julian_day_of(got.absolute_year(), got.month, got.day)
    } else {
        days_from_civil(got.absolute_year(), 1, 1) + got.day_of_year - 1 + JDN_OF_EPOCH
    };
    // Before the changeover tclsh reckons in the Julian calendar, which this
    // module does not have.
    if got.julian_day < GREGORIAN_CHANGE_JDN {
        return Err(too_early());
    }
    Ok(())
}

/// The first stage of `ClockValidDate`: every field the input carried is held
/// to its range, in the order tclsh checks them.
fn validate_fields(got: &mut Scanned, assemble_julian: &mut bool) -> Result<(), String> {
    use flag::*;
    let flags = got.flags;
    if flags & (YEAR | ISO8601YEAR) != 0 {
        if flags & YEAR == 0 {
            got.year = got.iso_year;
        }
        if flags & (ISO8601YEAR | YEAR) == ISO8601YEAR | YEAR && got.year != got.iso_year {
            return Err(invalid_input("ambiguous year"));
        }
    }
    if flags & MONTH != 0 && !(1..=12).contains(&got.month) {
        return Err(invalid_input("invalid month"));
    }
    if *assemble_julian {
        assemble_julian_day(got)?;
        *assemble_julian = false;
    }
    let leap = is_leap(got.absolute_year());
    if flags & (DAYOFMONTH | DAYOFWEEK) != 0 {
        if !(1..=31).contains(&got.day) {
            return Err(invalid_input("invalid day"));
        }
        if flags & MONTH != 0
            && got.day > month_length(got.absolute_year(), got.month as u32) as i64
        {
            return Err(invalid_input("invalid day"));
        }
    }
    if flags & DAYOFYEAR != 0 {
        let length = if leap { 366 } else { 365 };
        if got.day_of_year < 1 || got.day_of_year > length {
            return Err(invalid_input("invalid day of year"));
        }
    }
    if flags & (DAYOFYEAR | DAYOFMONTH | MONTH) == DAYOFYEAR | DAYOFMONTH | MONTH {
        let by_day_of_year =
            days_from_civil(got.absolute_year(), 1, 1) + got.day_of_year - 1 + JDN_OF_EPOCH;
        if by_day_of_year != got.julian_day {
            return Err(invalid_input("ambiguous day"));
        }
    }
    if flags & TIME != 0 {
        let limit = if got.meridian == Meridian::H24 {
            23
        } else {
            12
        };
        if got.hour < 0 || got.hour > limit {
            // 24:00:00 is the midnight that ends the day, and the weekday the
            // input named is the next day's.
            if got.meridian == Meridian::H24 && got.hour == 24 {
                if got.minute != 0 || got.second != 0 {
                    return Err(invalid_input("invalid time"));
                }
                if flags & DAYOFWEEK != 0 {
                    got.weekday = got.weekday % 7 + 1;
                }
            } else {
                return Err(invalid_input("invalid time (hour)"));
            }
        }
        if !(0..=59).contains(&got.minute) {
            return Err(invalid_input("invalid time (minutes)"));
        }
        if !(0..=59).contains(&got.second) || got.second_of_day <= -1 {
            return Err(invalid_input("invalid time"));
        }
    }
    Ok(())
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
    base: Option<Value>,
    locale: Option<String>,
    /// `-validate`, which only `clock scan` takes: whether the scanned fields
    /// are held to their ranges. On unless the script turns it off.
    validate: bool,
}

impl Options {
    /// The message catalogue `-locale` asks for. With no `-locale` the answer
    /// is the current locale, which `::tcl::clock::EnterLocale` reads from
    /// `mclocale` and msgcat initialises from the environment — so an
    /// unadorned `clock format` answers in the caller's language, as tclsh's
    /// does.
    fn catalog(&self) -> Result<Arc<Catalog>, String> {
        crate::clock_locale::enter(self.locale.as_deref().unwrap_or("current"))
    }

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

/// Which subcommand is reading its options — `ClockOperation`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Operation {
    Format,
    Scan,
    Add,
}

/// Every option name `ClockParseFmtScnArgs` looks a word up in, whichever
/// subcommand is asking; one the subcommand does not take is refused after
/// it has been recognised.
const OPTION_NAMES: &[&str] = &[
    "-base",
    "-format",
    "-gmt",
    "-locale",
    "-timezone",
    "-validate",
];

/// `ClockParseFmtScnArgs`: read the `-option value` pairs that follow the
/// subcommand's first argument. The caller has already checked that they
/// come in pairs. `clock add`'s offsets are integers standing where an option
/// name would, and are stepped over here.
fn options(words: &[Value], operation: Operation) -> Result<Options, String> {
    let must_be = match operation {
        Operation::Format => "-format, -gmt, -locale, or -timezone",
        Operation::Scan => "-base, -format, -gmt, -locale, -timezone or -validate",
        Operation::Add => "-gmt, -locale, or -timezone",
    };
    let mut out = Options {
        format: None,
        gmt: None,
        timezone: None,
        base: None,
        locale: None,
        validate: true,
    };
    // `format` and `add` read their clock value as the base, so a `-base`
    // there is refused as an option they do not take.
    let mut seen: Vec<&str> = Vec::new();
    if operation != Operation::Scan {
        seen.push("-base");
    }
    for pair in words.chunks(2) {
        let name = to_tcl_string(&pair[0]);
        if operation == Operation::Add
            && matches!(crate::runtime::parse_number(name.trim()), Ok(Num::Int(_)))
        {
            continue;
        }
        let bad = || format!("bad option \"{name}\": must be {must_be}");
        let Some(option) = resolve(&name, OPTION_NAMES) else {
            return Err(bad());
        };
        if seen.contains(&option) {
            if operation != Operation::Scan && option == "-base" {
                return Err(bad());
            }
            return Err(format!("bad option \"{name}\": doubly present"));
        }
        let value = &pair[1];
        match option {
            "-format" if operation == Operation::Add => return Err(bad()),
            "-format" => out.format = Some(to_tcl_string(value)),
            "-gmt" => out.gmt = Some(crate::runtime::tcl_bool(value)?),
            // The locale decides the month and day names, the AM/PM and era
            // words, the `%c`/`%x`/`%X` expansions, the digits `%O…` writes
            // and the Gregorian changeover. Every name resolves: one with no
            // catalogue anywhere in its fallback chain is the root locale.
            "-locale" => out.locale = Some(to_tcl_string(value)),
            "-timezone" => out.timezone = Some(to_tcl_string(value)),
            // Read once the zone is set up, which is where tclsh reads it.
            "-base" => out.base = Some(value.clone()),
            "-validate" if operation != Operation::Scan => return Err(bad()),
            "-validate" => out.validate = crate::runtime::tcl_bool(value)?,
            _ => unreachable!("the option table and this match are one list"),
        }
        seen.push(option);
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
const SCAN_USAGE: &str = "wrong # args: should be \"clock scan string ?-base seconds? ?-format string? ?-gmt boolean? ?-locale LOCALE? ?-timezone ZONE? ?-validate boolean?\"";
const ADD_USAGE: &str = "wrong # args: should be \"clock add clockval|now ?number units?...?-gmt boolean? ?-locale LOCALE? ?-timezone ZONE?\"";

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
    // The clock value and then options in pairs, or the usage.
    if words.len().is_multiple_of(2) {
        return Err(FORMAT_USAGE.to_string());
    }
    let opts = options(&words[1..], Operation::Format)?;
    let zone = opts.zone()?;
    let seconds = seconds_of(&words[0])?;
    let format = opts.format.as_deref().unwrap_or(DEFAULT_FORMAT);
    let cat = opts.catalog()?;
    Ok(Value::Str(Arc::new(format_time(
        seconds, format, &zone, &cat,
    )?)))
}

fn run_scan(words: &[Value]) -> Result<Value, String> {
    if words.len().is_multiple_of(2) {
        return Err(SCAN_USAGE.to_string());
    }
    let opts = options(&words[1..], Operation::Scan)?;
    let zone = opts.zone()?;
    // `-base` is the instant the fields the format did not carry are taken
    // from, which is the current one when the script names none.
    let base_at = match &opts.base {
        Some(base) => seconds_of(base)?,
        None => current_seconds(),
    };
    let Some(format) = opts.format.as_deref() else {
        return Err(
            "clock scan: the free-form parser is not supported yet; use -format".to_string(),
        );
    };
    let cat = opts.catalog()?;
    Ok(Value::Int(scan_time(
        &to_tcl_string(&words[0]),
        format,
        &zone,
        &cat,
        base_at,
        opts.validate,
    )?))
}

fn run_add(words: &[Value]) -> Result<Value, String> {
    if words.len().is_multiple_of(2) {
        return Err(ADD_USAGE.to_string());
    }
    // Offsets and options share the pairs: an integer where an option name
    // would stand starts an offset, and every other word is an option.
    let pairs = &words[1..];
    let opts = options(pairs, Operation::Add)?;
    let zone = opts.zone()?;
    let mut seconds = seconds_of(&words[0])?;
    for pair in pairs.chunks(2) {
        let Ok(Num::Int(count)) = crate::runtime::parse_number(tcl_str(&pair[0]).trim()) else {
            continue;
        };
        let unit = to_tcl_string(&pair[1]);
        let Some(resolved) = resolve(&unit, UNITS) else {
            return Err(bad_unit(&unit));
        };
        seconds = add_units(seconds, count, resolved, &zone)?;
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
        let out =
            format_time(1234567890, DEFAULT_FORMAT, &utc, &Catalog::default()).expect("formats");
        assert_eq!(out, "Fri Feb 13 23:31:30 GMT 2009");
        let iso = format_time(1234567890, "%G-W%V-%u %j %U %W", &utc, &Catalog::default())
            .expect("formats");
        assert_eq!(iso, "2009-W07-5 044 06 06");
    }

    /// Before the changeover the answer would depend on the locale's calendar,
    /// so there is no answer rather than a wrong one.
    #[test]
    fn early_dates_are_refused() {
        let utc = Zone::fixed(0, "GMT");
        let err = format_time(EARLIEST - 1, "%Y", &utc, &Catalog::default()).expect_err("refused");
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
