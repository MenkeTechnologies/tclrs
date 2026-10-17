//! The free-form date parser behind `clock scan` with no `-format`.
//!
//! A port of `TclClockFreeScan` and the grammar behind it — `generic/tclGetDate.y`,
//! and the LALR tables and driver bison 3.8.2 generated from it into
//! `generic/tclDate.c`. The tables below are that file's, number for number, and
//! [`parse`] is its `yyparse` as a loop over them, so which strings the grammar
//! accepts, which it refuses and *where* it reports the refusal are the
//! reference's, not an approximation of its behaviour.
//!
//! What the grammar fills in is a [`Fields`]; turning those into an instant is
//! `ClockFreeScan`'s second half and `ClockScanCommit`, which live with the other
//! scanner in [`crate::cmd_clock`].
//!
//! One platform detail is carried over rather than corrected: the scanner returns
//! a byte it does not recognise as a `char`, which is signed where tclsh was
//! measured, so a byte of 0x80 or more reads as end of input and everything after
//! it is ignored.

/// `CLF_*` of `generic/tclDate.h`, the ones the grammar raises.
pub(crate) mod flag {
    pub const TIME: u32 = 1 << 4;
    pub const ZONE: u32 = 1 << 5;
    pub const DAYOFMONTH: u32 = 1 << 7;
    pub const MONTH: u32 = 1 << 9;
    pub const YEAR: u32 = 1 << 10;
    pub const DAYOFWEEK: u32 = 1 << 11;
    pub const HAVEDATE: u32 = DAYOFMONTH | MONTH | YEAR;
    pub const RELCONV: u32 = 1 << 17;
    pub const ORDINALMONTH: u32 = 1 << 18;
    pub const TREK: u32 = 1 << 19;
}

/// `MERIDIAN`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Meridian {
    Am,
    Pm,
    H24,
}

/// `DSTMODE`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Dst {
    On,
    Off,
    Maybe,
}

/// Which relative-time accumulator the last unit named — `yyRelPointer`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RelTarget {
    Seconds,
    Day,
    Month,
}

/// The parts of `DateInfo` the grammar reads and writes. A field the string does
/// not mention keeps the value it was given, which is the base date's.
#[derive(Clone, Debug)]
pub(crate) struct Fields {
    pub flags: u32,
    /// `errFlags`: a part of the date found twice.
    err_flags: u32,
    pub year: i64,
    pub month: i64,
    pub day: i64,
    /// Monday is 1 and Sunday is 7.
    pub day_of_week: i64,
    pub hour: i64,
    pub minutes: i64,
    pub seconds: i64,
    pub meridian: Meridian,
    /// Minutes west of Greenwich.
    pub timezone: i64,
    pub dst_mode: Dst,
    pub rel_month: i64,
    pub rel_day: i64,
    pub rel_seconds: i64,
    pub month_ordinal_incr: i64,
    pub month_ordinal: i64,
    pub day_ordinal: i64,
    rel_target: RelTarget,
    digit_count: i64,
}

impl Fields {
    pub(crate) fn new(year: i64, month: i64, day: i64, day_of_week: i64) -> Fields {
        Fields {
            flags: 0,
            err_flags: 0,
            year,
            month,
            day,
            day_of_week,
            hour: 0,
            minutes: 0,
            seconds: 0,
            meridian: Meridian::H24,
            timezone: 0,
            dst_mode: Dst::Maybe,
            rel_month: 0,
            rel_day: 0,
            rel_seconds: 0,
            month_ordinal_incr: 0,
            month_ordinal: 0,
            day_ordinal: 0,
            rel_target: RelTarget::Seconds,
            digit_count: 0,
        }
    }
}

// ── the tables bison generated ───────────────────────────────────────────

const YYFINAL: i16 = 2;
const YYLAST: i16 = 105;
const YYNTOKENS: i16 = 33;
const YYMAXUTOK: i32 = 280;
const YYPACT_NINF: i16 = -35;

const YYTRANSLATE: [i16; 281] = [
    0, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
    2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 32, 27, 28, 31, 29, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 26, 2, 2, 2,
    2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 30, 2, 2, 2, 2, 2, 2, 2, 2,
    2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
    2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
    2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
    2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
    2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
    2, 2, 2, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
    25,
];

const YYPACT: [i16; 107] = [
    -35, 53, -35, -24, -35, 2, 36, -35, -3, 6, -14, -14, -35, -2, 10, 28, -35, 31, -35, -35, -35,
    25, -35, -35, -35, -35, -35, -35, -35, -13, -35, -35, -35, 49, 17, -35, 22, -35, 35, -35, -24,
    -35, -35, -35, 57, -35, -35, 67, 77, 71, 78, -35, 36, 36, -35, -35, -35, -35, -35, -35, 54,
    -35, -35, 22, -35, -35, -35, -35, 58, -35, 4, 63, 22, -35, -35, 79, 80, -35, 68, 61, 69, 66,
    -35, -35, -35, -35, 70, -35, -35, -35, -35, -35, 94, 22, -35, -35, -35, 86, 87, 88, 89, -35,
    -35, -35, -35, -35, -35,
];

const YYDEFACT: [i16; 107] = [
    2, 0, 1, 27, 20, 0, 0, 69, 74, 19, 0, 0, 41, 47, 48, 0, 70, 0, 64, 65, 3, 75, 4, 5, 10, 8, 49,
    6, 7, 36, 11, 12, 9, 57, 0, 63, 0, 13, 25, 28, 38, 71, 73, 72, 0, 29, 16, 40, 0, 0, 0, 18, 0,
    0, 54, 53, 32, 43, 68, 66, 71, 67, 61, 0, 76, 17, 46, 45, 0, 56, 23, 0, 0, 60, 26, 0, 0, 42,
    15, 0, 0, 34, 21, 22, 44, 62, 0, 50, 51, 52, 31, 24, 71, 0, 59, 39, 55, 0, 0, 0, 0, 30, 58, 14,
    37, 33, 35,
];

const YYPGOTO: [i16; 26] = [
    -35, -35, -35, 37, -35, -35, -35, -35, 64, -35, -35, -35, -35, -35, -35, -35, -35, -35, -35,
    -35, 76, -34, -35, -6, -35, -35,
];

const YYDEFGOTO: [i16; 26] = [
    0, 1, 20, 21, 22, 23, 24, 91, 39, 25, 26, 27, 28, 68, 29, 89, 30, 31, 32, 33, 34, 62, 35, 36,
    37, 65,
];

const YYTABLE: [i16; 106] = [
    44, 45, 73, 38, 46, 47, -71, -73, 90, -71, -73, 63, 66, -71, 18, 40, -71, 67, 19, -72, -71,
    -73, -72, 48, 51, 49, 50, -71, 72, 85, 70, 58, 64, -72, 59, 56, 42, 43, 94, 57, 58, 54, 71, 59,
    60, 61, 82, 83, 55, 41, 42, 43, 69, 2, 61, 42, 43, 3, 4, 102, 74, 5, 84, 6, 7, 93, 8, 9, 10,
    11, 12, 86, 13, 14, 15, 16, 92, 17, 87, 79, 77, 18, 42, 43, 80, 19, 52, 53, 76, 98, 78, 81, 95,
    96, 97, 100, 48, 99, 101, 103, 104, 105, 106, 0, 75, 88,
];

const YYCHECK: [i16; 106] = [
    6, 4, 36, 27, 7, 8, 9, 9, 4, 12, 12, 17, 25, 9, 28, 13, 12, 30, 32, 9, 23, 23, 12, 26, 18, 28,
    29, 23, 34, 63, 13, 9, 7, 23, 12, 4, 19, 20, 72, 8, 9, 13, 25, 12, 13, 23, 52, 53, 20, 13, 19,
    20, 3, 0, 23, 19, 20, 4, 5, 93, 25, 8, 8, 10, 11, 71, 13, 14, 15, 16, 17, 13, 19, 20, 21, 22,
    13, 24, 20, 8, 13, 28, 19, 20, 13, 32, 10, 11, 31, 28, 13, 13, 13, 13, 26, 29, 26, 28, 4, 13,
    13, 13, 13, -1, 40, 68,
];

const YYR1: [i16; 77] = [
    0, 33, 34, 34, 35, 35, 35, 35, 35, 35, 35, 35, 35, 35, 36, 36, 37, 37, 38, 38, 38, 38, 38, 40,
    39, 41, 41, 42, 42, 42, 42, 42, 42, 43, 44, 44, 44, 44, 44, 44, 44, 44, 44, 45, 45, 46, 46, 47,
    47, 47, 48, 48, 49, 49, 49, 50, 51, 51, 52, 52, 52, 52, 52, 52, 53, 53, 54, 54, 54, 55, 55, 56,
    56, 56, 57, 58, 58,
];

const YYR2: [i16; 77] = [
    0, 2, 0, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 5, 3, 2, 2, 2, 1, 1, 3, 3, 0, 3, 1, 2, 1, 2, 2, 4, 3,
    2, 5, 3, 5, 1, 5, 2, 4, 2, 1, 3, 2, 3, 1, 1, 1, 1, 1, 1, 1, 3, 2, 2, 4, 2, 1, 4, 3, 2, 2, 3, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 1,
];

// ── token kinds ──────────────────────────────────────────────────────────
//
// The numbers bison gave the named tokens, in the order `tclGetDate.y`
// declares them; a literal character is its own code.

const T_AGO: i32 = 258;
const T_DAY: i32 = 259;
const T_DAYZONE: i32 = 260;
const T_ID: i32 = 261;
const T_MERIDIAN: i32 = 262;
const T_MONTH: i32 = 263;
const T_RMONTH_UNIT: i32 = 264;
const T_STARDATE: i32 = 265;
const T_SEC_UNIT: i32 = 266;
const T_RSEC_UNIT: i32 = 267;
const T_UNUMBER: i32 = 268;
const T_ZONE: i32 = 269;
const T_ZONE_WO4: i32 = 270;
const T_ZONE_WO2: i32 = 271;
const T_EPOCH: i32 = 272;
const T_DST: i32 = 273;
const T_ISOBAS8: i32 = 274;
const T_ISOBAS6: i32 = 275;
const T_ISOBASL: i32 = 276;
const T_DAY_UNIT: i32 = 277;
const T_RDAY_UNIT: i32 = 278;
const T_NEXT: i32 = 279;
const SP: i32 = 280;

const EPOCH: i64 = 1970;

// ── the lexical tables ───────────────────────────────────────────────────

/// `TABLE`: a word, the token it is, and the value it carries.
type Word = (&'static str, i32, i64);

const MONTH_DAY_TABLE: &[Word] = &[
    ("january", T_MONTH, 1),
    ("february", T_MONTH, 2),
    ("march", T_MONTH, 3),
    ("april", T_MONTH, 4),
    ("may", T_MONTH, 5),
    ("june", T_MONTH, 6),
    ("july", T_MONTH, 7),
    ("august", T_MONTH, 8),
    ("september", T_MONTH, 9),
    ("sept", T_MONTH, 9),
    ("october", T_MONTH, 10),
    ("november", T_MONTH, 11),
    ("december", T_MONTH, 12),
    ("sunday", T_DAY, 7),
    ("monday", T_DAY, 1),
    ("tuesday", T_DAY, 2),
    ("tues", T_DAY, 2),
    ("wednesday", T_DAY, 3),
    ("wednes", T_DAY, 3),
    ("thursday", T_DAY, 4),
    ("thur", T_DAY, 4),
    ("thurs", T_DAY, 4),
    ("friday", T_DAY, 5),
    ("saturday", T_DAY, 6),
];

const UNITS_TABLE: &[Word] = &[
    ("year", T_RMONTH_UNIT, 12),
    ("month", T_RMONTH_UNIT, 1),
    ("fortnight", T_RDAY_UNIT, 14),
    ("week", T_RDAY_UNIT, 7),
    ("day", T_RDAY_UNIT, 1),
    ("hour", T_RSEC_UNIT, 60 * 60),
    ("minute", T_RSEC_UNIT, 60),
    ("min", T_RSEC_UNIT, 60),
    ("second", T_RSEC_UNIT, 1),
    ("sec", T_RSEC_UNIT, 1),
];

const OTHER_TABLE: &[Word] = &[
    ("tomorrow", T_DAY_UNIT, 1),
    ("yesterday", T_DAY_UNIT, -1),
    ("today", T_DAY_UNIT, 0),
    ("now", T_SEC_UNIT, 0),
    ("last", T_UNUMBER, -1),
    ("this", T_SEC_UNIT, 0),
    ("next", T_NEXT, 1),
    ("ago", T_AGO, 1),
    ("epoch", T_EPOCH, 0),
    ("stardate", T_STARDATE, 0),
];

/// `HOUR(x)` is `60 * (int) x`; the half-hour zones are written `HOUR(7/2)` in
/// the source, which is integer division, so Newfoundland is three hours and the
/// others lose their halves the same way. The values here are what that computes.
const TIMEZONE_TABLE: &[Word] = &[
    ("gmt", T_ZONE, 0),
    ("ut", T_ZONE, 0),
    ("utc", T_ZONE, 0),
    ("uct", T_ZONE, 0),
    ("wet", T_ZONE, 0),
    ("bst", T_DAYZONE, 0),
    ("wat", T_ZONE, 60),
    ("at", T_ZONE, 120),
    ("nft", T_ZONE, 180),
    ("nst", T_ZONE, 180),
    ("ndt", T_DAYZONE, 180),
    ("ast", T_ZONE, 240),
    ("adt", T_DAYZONE, 240),
    ("est", T_ZONE, 300),
    ("edt", T_DAYZONE, 300),
    ("cst", T_ZONE, 360),
    ("cdt", T_DAYZONE, 360),
    ("mst", T_ZONE, 420),
    ("mdt", T_DAYZONE, 420),
    ("pst", T_ZONE, 480),
    ("pdt", T_DAYZONE, 480),
    ("yst", T_ZONE, 540),
    ("ydt", T_DAYZONE, 540),
    ("akst", T_ZONE, 540),
    ("akdt", T_DAYZONE, 540),
    ("hst", T_ZONE, 600),
    ("hdt", T_DAYZONE, 600),
    ("cat", T_ZONE, 600),
    ("ahst", T_ZONE, 600),
    ("nt", T_ZONE, 660),
    ("idlw", T_ZONE, 720),
    ("cet", T_ZONE, -60),
    ("cest", T_DAYZONE, -60),
    ("met", T_ZONE, -60),
    ("mewt", T_ZONE, -60),
    ("mest", T_DAYZONE, -60),
    ("swt", T_ZONE, -60),
    ("sst", T_DAYZONE, -60),
    ("fwt", T_ZONE, -60),
    ("fst", T_DAYZONE, -60),
    ("eet", T_ZONE, -120),
    ("bt", T_ZONE, -180),
    ("it", T_ZONE, -180),
    ("zp4", T_ZONE, -240),
    ("zp5", T_ZONE, -300),
    ("ist", T_ZONE, -300),
    ("zp6", T_ZONE, -360),
    ("wast", T_ZONE, -420),
    ("wadt", T_DAYZONE, -420),
    ("jt", T_ZONE, -420),
    ("cct", T_ZONE, -480),
    ("jst", T_ZONE, -540),
    ("jdt", T_DAYZONE, -540),
    ("kst", T_ZONE, -540),
    ("kdt", T_DAYZONE, -540),
    ("cast", T_ZONE, -540),
    ("cadt", T_DAYZONE, -540),
    ("east", T_ZONE, -600),
    ("eadt", T_DAYZONE, -600),
    ("gst", T_ZONE, -600),
    ("nzt", T_ZONE, -720),
    ("nzst", T_ZONE, -720),
    ("nzdt", T_DAYZONE, -720),
    ("idle", T_ZONE, -720),
    ("dst", T_DST, 0),
];

/// The one-letter military zones, `a` through `z` without `j`.
const MILITARY_TABLE: &[Word] = &[
    ("a", T_ZONE, -60),
    ("b", T_ZONE, -120),
    ("c", T_ZONE, -180),
    ("d", T_ZONE, -240),
    ("e", T_ZONE, -300),
    ("f", T_ZONE, -360),
    ("g", T_ZONE, -420),
    ("h", T_ZONE, -480),
    ("i", T_ZONE, -540),
    ("k", T_ZONE, -600),
    ("l", T_ZONE, -660),
    ("m", T_ZONE, -720),
    ("n", T_ZONE, 60),
    ("o", T_ZONE, 120),
    ("p", T_ZONE, 180),
    ("q", T_ZONE, 240),
    ("r", T_ZONE, 300),
    ("s", T_ZONE, 360),
    ("t", T_ZONE, 420),
    ("u", T_ZONE, 480),
    ("v", T_ZONE, 540),
    ("w", T_ZONE, 600),
    ("x", T_ZONE, 660),
    ("y", T_ZONE, 720),
    ("z", T_ZONE, 0),
];

fn lookup_in(table: &[Word], word: &str) -> Option<(i32, i64)> {
    table
        .iter()
        .find(|(name, _, _)| *name == word)
        .map(|&(_, kind, value)| (kind, value))
}

/// `LookupWord`: the token a run of letters and periods is, and its value.
/// A meridian's value is 0 for am and 1 for pm.
fn lookup_word(word: &str) -> (i32, i64) {
    let mut buff = word.to_ascii_lowercase();
    if buff == "am" || buff == "a.m." {
        return (T_MERIDIAN, 0);
    }
    if buff == "pm" || buff == "p.m." {
        return (T_MERIDIAN, 1);
    }
    // A three-letter word, or one with a period after three, abbreviates any
    // month or day whose name begins with it.
    let abbrev = if buff.len() == 3 {
        true
    } else if buff.len() == 4 && buff.as_bytes()[3] == b'.' {
        buff.truncate(3);
        true
    } else {
        false
    };
    for &(name, kind, value) in MONTH_DAY_TABLE {
        let hit = if abbrev {
            name.as_bytes().starts_with(buff.as_bytes())
        } else {
            name == buff
        };
        if hit {
            return (kind, value);
        }
    }
    if let Some(found) = lookup_in(TIMEZONE_TABLE, &buff) {
        return found;
    }
    if let Some(found) = lookup_in(UNITS_TABLE, &buff) {
        return found;
    }
    // Strip a plural and try the units again. The scanner cuts the `s` off the
    // word itself, not off a copy, so the tables below it see the singular too
    // — which is why `this`, a word of `OtherTable`, is read as `thi`.
    if buff.len() > 1 && buff.ends_with('s') {
        buff.pop();
        if let Some(found) = lookup_in(UNITS_TABLE, &buff) {
            return found;
        }
    }
    if let Some(found) = lookup_in(OTHER_TABLE, &buff) {
        return found;
    }
    if buff.len() == 1 && buff.as_bytes()[0].is_ascii_alphabetic() {
        if let Some(found) = lookup_in(MILITARY_TABLE, &buff) {
            return found;
        }
    }
    // Drop the periods and try the zones again.
    let without: String = buff.chars().filter(|&c| c != '.').collect();
    if without.len() != buff.len() {
        if let Some(found) = lookup_in(TIMEZONE_TABLE, &without) {
            return found;
        }
    }
    (T_ID, 0)
}

// ── the scanner ──────────────────────────────────────────────────────────

/// `YYLTYPE`: the columns a token spans.
#[derive(Clone, Copy)]
struct Location {
    first_column: i64,
    last_column: i64,
}

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// `TclDatelex` and the input it walks.
struct Lexer {
    input: Vec<u8>,
    pos: usize,
}

impl Lexer {
    /// `*yyInput`, with the terminating NUL past the end.
    fn at(&self, offset: usize) -> u8 {
        self.input.get(self.pos + offset).copied().unwrap_or(0)
    }

    fn bypass_spaces(&mut self) {
        while is_space(self.at(0)) {
            self.pos += 1;
        }
    }

    /// The next token's code, its value in `value`, and its columns in `loc`.
    fn next(&mut self, value: &mut i64, loc: &mut Location, digit_count: &mut i64) -> i32 {
        loc.first_column = self.pos as i64;
        loop {
            if is_space(self.at(0)) {
                self.bypass_spaces();
                // Space at the end of the text, or before a word, is ignored.
                let c = self.at(0);
                if c != 0 && !c.is_ascii_alphabetic() {
                    return SP;
                }
            }
            let token_start = self.pos;
            let c = self.at(0);

            if c.is_ascii_digit() {
                let mut end = self.pos + 1;
                while self.input.get(end).is_some_and(|b| b.is_ascii_digit()) {
                    end += 1;
                }
                *digit_count = (end - self.pos) as i64;
                // Twelve or fourteen digits are YYYYMMDDhhmm[ss]: the first
                // eight are the date, and the rest is read as the next token.
                if *digit_count == 14 || *digit_count == 12 {
                    let eight = self.pos + 8;
                    match digits(&self.input[self.pos..eight]) {
                        Some(n) => *value = n,
                        None => return T_ID,
                    }
                    *digit_count = 8;
                    self.pos = eight;
                    loc.last_column = self.pos as i64 - 1;
                    return T_ISOBASL;
                }
                match digits(&self.input[self.pos..end]) {
                    Some(n) => *value = n,
                    None => return T_ID,
                }
                self.pos = end;
                loc.last_column = self.pos as i64 - 1;
                if *digit_count == 8 {
                    return T_ISOBAS8;
                }
                if *digit_count == 6 {
                    return T_ISOBAS6;
                }
                // Spaces after the digits are optional and ignored.
                self.bypass_spaces();
                return T_UNUMBER;
            }

            if c & 0x80 == 0 && c.is_ascii_alphabetic() {
                // At most nineteen characters of the word are kept.
                let mut word = String::new();
                loop {
                    let ch = self.at(0);
                    if !(ch.is_ascii_alphabetic() || ch == b'.') {
                        break;
                    }
                    self.pos += 1;
                    if word.len() < 19 {
                        word.push(char::from(ch));
                    }
                }
                loc.last_column = self.pos as i64 - 1;
                let (kind, number) = lookup_word(&word);
                *value = number;
                // Look ahead: a zone word followed by digits is not a zone
                // (the `T` of `...T...Z` is a literal), and one followed by a
                // signed number is a zone with an offset.
                if kind == T_ZONE || kind == T_DAYZONE {
                    let next = self.at(0);
                    if next.is_ascii_digit() {
                        self.pos = token_start;
                        let literal = i32::from(self.at(0));
                        self.pos += 1;
                        return literal;
                    }
                    if (next == b'+' || next == b'-') && self.at(1).is_ascii_digit() {
                        if !self.at(2).is_ascii_digit() || !self.at(3).is_ascii_digit() {
                            return T_ZONE_WO2;
                        }
                        if self.at(4).is_ascii_digit() && !self.at(5).is_ascii_digit() {
                            return T_ZONE_WO4;
                        }
                    }
                }
                self.bypass_spaces();
                return kind;
            }

            if c != b'(' {
                loc.last_column = self.pos as i64;
                let code = self.at(0);
                self.pos += 1;
                // A `char` is signed where the reference was measured.
                return i32::from(code as i8);
            }

            // A parenthesised comment, nested comments included.
            let mut depth = 0;
            loop {
                let ch = self.at(0);
                self.pos += 1;
                if ch == 0 {
                    loc.last_column = self.pos as i64 - 1;
                    return 0;
                } else if ch == b'(' {
                    depth += 1;
                } else if ch == b')' {
                    depth -= 1;
                }
                if depth <= 0 {
                    break;
                }
            }
        }
    }
}

/// `TclAtoWIe` over a run of digits: the number, or `None` when it overflows.
fn digits(run: &[u8]) -> Option<i64> {
    let mut n: i64 = 0;
    for &d in run {
        n = n.checked_mul(10)?.checked_add(i64::from(d - b'0'))?;
    }
    Some(n)
}

// ── the parser ───────────────────────────────────────────────────────────

/// `(int)` of a value the grammar stores in one of `DateInfo`'s `int` fields.
fn int(n: i64) -> i64 {
    i64::from(n as i32)
}

fn is_leap_year(y: i64) -> bool {
    let y = int(y);
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

/// What a reduction decided: carry on, or stop the parse as `YYABORT` and
/// `YYERROR` do.
enum Outcome {
    Continue,
    Abort,
    /// `YYERROR`: unwind as a syntax error without a message.
    Error,
}

/// `yyIncrFlags`: raise flags, refusing a second raising of any of them.
fn incr_flags(info: &mut Fields, f: u32) -> Outcome {
    info.err_flags |= info.flags & f;
    if info.err_flags != 0 {
        return Outcome::Abort;
    }
    info.flags |= f;
    Outcome::Continue
}

/// Run the action of rule `rule`. `v(k)` is `$k`, and `top` is the value `$$`
/// starts as — `$1`, as bison's default action has it.
fn reduce(rule: usize, info: &mut Fields, v: &dyn Fn(usize) -> i64, out: &mut i64) -> Outcome {
    use flag::*;
    let m = |n: i64| match n {
        0 => Meridian::Am,
        1 => Meridian::Pm,
        _ => Meridian::H24,
    };
    match rule {
        4 => return incr_flags(info, TIME),
        5 => return incr_flags(info, ZONE),
        6 => return incr_flags(info, HAVEDATE),
        7 => {
            if let Outcome::Abort = incr_flags(info, ORDINALMONTH) {
                return Outcome::Abort;
            }
            info.flags |= RELCONV;
        }
        8 => {
            if let Outcome::Abort = incr_flags(info, DAYOFWEEK) {
                return Outcome::Abort;
            }
            info.flags |= RELCONV;
        }
        9 => info.flags |= RELCONV,
        10 => return incr_flags(info, ZONE),
        11 => return incr_flags(info, TIME | HAVEDATE),
        12 => {
            if let Outcome::Abort = incr_flags(info, TIME | HAVEDATE) {
                return Outcome::Abort;
            }
            info.flags |= TREK;
        }
        // iextime: tUNUMBER ':' tUNUMBER ':' tUNUMBER
        14 => {
            info.hour = int(v(1));
            info.minutes = int(v(3));
            info.seconds = v(5);
        }
        // iextime: tUNUMBER ':' tUNUMBER
        15 => {
            info.hour = int(v(1));
            info.minutes = int(v(3));
            info.seconds = 0;
        }
        // time: tUNUMBER tMERIDIAN
        16 => {
            info.hour = int(v(1));
            info.minutes = 0;
            info.seconds = 0;
            info.meridian = m(v(2));
        }
        // time: iextime o_merid
        17 => info.meridian = m(v(2)),
        // zone: tZONE tDST
        18 => {
            info.timezone = int(v(1));
            info.dst_mode = Dst::On;
        }
        19 => {
            info.timezone = int(v(1));
            info.dst_mode = Dst::Off;
        }
        20 => {
            info.timezone = int(v(1));
            info.dst_mode = Dst::On;
        }
        // zone: tZONEwO4 sign INTNUM
        21 => {
            info.timezone = int(v(1) - v(2) * (v(3) % 100 + (v(3) / 100) * 60));
            info.dst_mode = Dst::Off;
        }
        // zone: tZONEwO2 sign INTNUM
        22 => {
            info.timezone = int(v(1) - v(2) * (v(3) * 60));
            info.dst_mode = Dst::Off;
        }
        // The `%?{ ... }` predicate of nmzone.
        23 => {
            if !(info.digit_count == 4 || info.digit_count <= 2) {
                return Outcome::Error;
            }
        }
        // nmzone: sign tUNUMBER $@1
        24 => {
            if info.digit_count == 4 {
                info.timezone = int(-v(1) * (v(2) % 100 + (v(2) / 100) * 60));
            } else {
                info.timezone = int(-v(1) * (v(2) * 60));
            }
            info.dst_mode = Dst::Off;
        }
        // day: tDAY | tDAY comma
        27 => {
            info.day_ordinal = 1;
            info.day_of_week = int(v(1));
        }
        28 => {
            info.day_ordinal = 1;
            info.day_of_week = int(v(1));
        }
        // day: tUNUMBER tDAY
        29 => {
            info.day_ordinal = int(v(1));
            info.day_of_week = int(v(2));
        }
        // day: sign SP tUNUMBER tDAY
        30 => {
            info.day_ordinal = int(v(1) * v(3));
            info.day_of_week = int(v(4));
        }
        // day: sign tUNUMBER tDAY
        31 => {
            info.day_ordinal = int(v(1) * v(2));
            info.day_of_week = int(v(3));
        }
        // day: tNEXT tDAY
        32 => {
            info.day_ordinal = 2;
            info.day_of_week = int(v(2));
        }
        // iexdate: tUNUMBER '-' tUNUMBER '-' tUNUMBER
        33 => {
            info.month = int(v(3));
            info.day = int(v(5));
            info.year = int(v(1));
        }
        // date: tUNUMBER '/' tUNUMBER
        34 => {
            info.month = int(v(1));
            info.day = int(v(3));
        }
        35 => {
            info.month = int(v(1));
            info.day = int(v(3));
            info.year = int(v(5));
        }
        // date: tUNUMBER '-' tMONTH '-' tUNUMBER
        37 => {
            info.day = int(v(1));
            info.month = int(v(3));
            info.year = int(v(5));
        }
        // date: tMONTH tUNUMBER
        38 => {
            info.month = int(v(1));
            info.day = int(v(2));
        }
        // date: tMONTH tUNUMBER comma tUNUMBER
        39 => {
            info.month = int(v(1));
            info.day = int(v(2));
            info.year = int(v(4));
        }
        // date: tUNUMBER tMONTH
        40 => {
            info.month = int(v(2));
            info.day = int(v(1));
        }
        // date: tEPOCH
        41 => {
            info.month = 1;
            info.day = 1;
            info.year = EPOCH;
        }
        // date: tUNUMBER tMONTH tUNUMBER
        42 => {
            info.month = int(v(2));
            info.day = int(v(1));
            info.year = int(v(3));
        }
        // ordMonth: tNEXT tMONTH
        43 => {
            info.month_ordinal_incr = 1;
            info.month_ordinal = int(v(2));
        }
        // ordMonth: tNEXT tUNUMBER tMONTH
        44 => {
            info.month_ordinal_incr = int(v(2));
            info.month_ordinal = int(v(3));
        }
        // isodate: tISOBAS8 | tISOBAS6
        47 | 48 => {
            info.year = int(v(1) / 10000);
            info.month = int((v(1) % 10000) / 100);
            info.day = int(v(1) % 100);
        }
        // isotime: tISOBAS6
        50 => {
            info.hour = int(v(1) / 10000);
            info.minutes = int((v(1) % 10000) / 100);
            info.seconds = v(1) % 100;
        }
        // iso: tISOBASL tISOBAS6
        53 => {
            info.year = int(v(1) / 10000);
            info.month = int((v(1) % 10000) / 100);
            info.day = int(v(1) % 100);
            info.hour = int(v(2) / 10000);
            info.minutes = int((v(2) % 10000) / 100);
            info.seconds = v(2) % 100;
        }
        // iso: tISOBASL tUNUMBER
        54 => {
            if info.digit_count != 4 {
                return Outcome::Abort;
            }
            info.year = int(v(1) / 10000);
            info.month = int((v(1) % 10000) / 100);
            info.day = int(v(1) % 100);
            info.hour = int(v(2) / 100);
            info.minutes = int(v(2) % 100);
            info.seconds = 0;
        }
        // trek: tSTARDATE INTNUM '.' tUNUMBER
        55 => {
            // The computed year is offset by -377 so that it stays in the range
            // a 32-bit clock value reaches.
            info.year = int(v(2) / 1000 + 2323 - 377);
            info.day = 1;
            info.month = 1;
            info.rel_day = info
                .rel_day
                .wrapping_add(((v(2) % 1000) * (365 + i64::from(is_leap_year(info.year)))) / 1000);
            info.rel_seconds = info.rel_seconds.wrapping_add(v(4).wrapping_mul(144 * 60));
            info.flags |= RELCONV;
        }
        // relspec: relunits tAGO
        56 => {
            info.rel_seconds = info.rel_seconds.wrapping_mul(-1);
            info.rel_month = info.rel_month.wrapping_mul(-1);
            info.rel_day = info.rel_day.wrapping_mul(-1);
        }
        // relunits: sign SP INTNUM runit
        58 => relative(info, v(1).wrapping_mul(v(3)).wrapping_mul(v(4))),
        // relunits: sign INTNUM runit
        59 => relative(info, v(1).wrapping_mul(v(2)).wrapping_mul(v(3))),
        // relunits: INTNUM runit
        60 => relative(info, v(1).wrapping_mul(v(2))),
        // relunits: tNEXT runit
        61 => relative(info, v(2)),
        // relunits: tNEXT INTNUM runit
        62 => relative(info, v(2).wrapping_mul(v(3))),
        // relunits: unit
        63 => relative(info, v(1)),
        // sign
        64 => *out = -1,
        65 => *out = 1,
        // runit: tRSEC_UNIT
        66 => {
            *out = v(1);
            info.rel_target = RelTarget::Seconds;
        }
        67 => {
            *out = v(1);
            info.rel_target = RelTarget::Day;
            info.flags |= RELCONV;
        }
        68 => {
            *out = v(1);
            info.rel_target = RelTarget::Month;
            info.flags |= RELCONV;
        }
        // unit
        69 => {
            *out = v(1);
            info.rel_target = RelTarget::Seconds;
        }
        70 => {
            *out = v(1);
            info.rel_target = RelTarget::Day;
            info.flags |= RELCONV;
        }
        // INTNUM
        71..=73 => *out = v(1),
        // numitem: tUNUMBER
        74 => {
            if info.flags & (TIME | HAVEDATE | TREK) == (TIME | HAVEDATE) {
                info.year = int(v(1));
            } else {
                if let Outcome::Abort = incr_flags(info, TIME) {
                    return Outcome::Abort;
                }
                if info.digit_count <= 2 {
                    info.hour = int(v(1));
                    info.minutes = 0;
                } else {
                    info.hour = int(v(1) / 100);
                    info.minutes = int(v(1) % 100);
                }
                info.seconds = 0;
                info.meridian = Meridian::H24;
            }
        }
        // o_merid: %empty
        75 => *out = 2,
        // o_merid: tMERIDIAN
        76 => *out = v(1),
        _ => {}
    }
    Outcome::Continue
}

/// `*yyRelPointer += n`.
fn relative(info: &mut Fields, n: i64) {
    let slot = match info.rel_target {
        RelTarget::Seconds => &mut info.rel_seconds,
        RelTarget::Day => &mut info.rel_day,
        RelTarget::Month => &mut info.rel_month,
    };
    *slot = slot.wrapping_add(n);
}

/// How a parse ended.
enum Parsed {
    Accepted,
    Aborted,
}

/// `yyparse`, over the tables above. A syntax error is reported once, with the
/// columns of the token the parser had read, and ends the parse: the grammar has
/// no `error` rule to recover to.
fn yyparse(info: &mut Fields, lexer: &mut Lexer, messages: &mut String) -> Parsed {
    let mut states: Vec<i16> = vec![0];
    let mut values: Vec<i64> = vec![0];
    let mut state: i16 = 0;
    let mut lookahead: Option<i32> = None;
    let mut lval: i64 = 0;
    let mut loc = Location {
        first_column: 1,
        last_column: 1,
    };
    loop {
        if state == YYFINAL {
            return Parsed::Accepted;
        }
        // Decide, from the state alone when it allows, whether to read a token.
        let mut rule: Option<usize> = None;
        let pact = YYPACT[state as usize];
        if pact != YYPACT_NINF {
            if lookahead.is_none() {
                let token = lexer.next(&mut lval, &mut loc, &mut info.digit_count);
                lookahead = Some(token);
            }
            let code = lookahead.expect("read above");
            let symbol: i16 = if code <= 0 {
                0
            } else if code <= YYMAXUTOK {
                YYTRANSLATE[code as usize]
            } else {
                2
            };
            let at = pact + symbol;
            if (0..=YYLAST).contains(&at) && YYCHECK[at as usize] == symbol {
                let action = YYTABLE[at as usize];
                if action > 0 {
                    // Shift.
                    state = action;
                    states.push(state);
                    values.push(lval);
                    lookahead = None;
                    continue;
                }
                rule = Some((-action) as usize);
            }
        }
        let rule = match rule {
            Some(rule) => rule,
            None => match YYDEFACT[state as usize] {
                0 => {
                    // A syntax error: reported once, at the lookahead's columns.
                    let separator = if messages.is_empty() { "" } else { "\n" };
                    messages.push_str(&format!(
                        "{separator}syntax error (characters {}-{})",
                        loc.first_column, loc.last_column
                    ));
                    return Parsed::Aborted;
                }
                n => n as usize,
            },
        };

        let length = YYR2[rule] as usize;
        let base = values.len() - length;
        let mut result = if length > 0 { values[base] } else { 0 };
        let outcome = {
            let v = |k: usize| values[base + k - 1];
            reduce(rule, info, &v, &mut result)
        };
        match outcome {
            Outcome::Continue => {}
            Outcome::Abort | Outcome::Error => return Parsed::Aborted,
        }
        states.truncate(states.len() - length);
        values.truncate(values.len() - length);
        let top = *states.last().expect("the stack holds the start state");
        let lhs = (YYR1[rule] - YYNTOKENS) as usize;
        let index = YYPGOTO[lhs] + top;
        state = if (0..=YYLAST).contains(&index) && YYCHECK[index as usize] == top {
            YYTABLE[index as usize]
        } else {
            YYDEFGOTO[lhs]
        };
        states.push(state);
        values.push(result);
    }
}

/// `TclClockFreeScan`: parse `input` into `info`, or say why it will not.
///
/// The error is the text `clock scan` puts after `unable to convert date-time
/// string "…": `.
pub(crate) fn parse(input: &str, info: &mut Fields) -> Result<(), String> {
    // Tcl's strings carry a NUL as the two bytes C0 80, so that the scanner's
    // C-string walk never meets one in the middle.
    let mut bytes = Vec::with_capacity(input.len());
    for &b in input.as_bytes() {
        if b == 0 {
            bytes.extend_from_slice(&[0xC0, 0x80]);
        } else {
            bytes.push(b);
        }
    }
    let mut lexer = Lexer {
        input: bytes,
        pos: 0,
    };
    // Spaces at the start are ignored.
    lexer.bypass_spaces();
    let mut messages = String::new();
    match yyparse(info, &mut lexer, &mut messages) {
        Parsed::Accepted => Ok(()),
        Parsed::Aborted => {
            use flag::*;
            let more = if info.err_flags & HAVEDATE != 0 {
                Some("more than one date in string")
            } else if info.err_flags & TIME != 0 {
                Some("more than one time of day in string")
            } else if info.err_flags & ZONE != 0 {
                Some("more than one time zone in string")
            } else if info.err_flags & DAYOFWEEK != 0 {
                Some("more than one weekday in string")
            } else if info.err_flags & ORDINALMONTH != 0 {
                Some("more than one ordinal month in string")
            } else {
                None
            };
            Err(more.map_or(messages, str::to_string))
        }
    }
}
