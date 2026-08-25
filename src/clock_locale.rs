//! The `clock` message catalogue: which locale a name resolves to, and what
//! that locale says.
//!
//! `clock format`, `clock scan` and `clock add` all take `-locale`, and the
//! locale decides the month and day names, the AM/PM and era words, the
//! `%x`/`%X`/`%c` expansions, the digits `%O…` writes and the Gregorian
//! changeover. tclsh keeps all of that in `msgcat`, so this module is the port
//! of the three pieces of tclsh that stand between a `-locale` word and those
//! values:
//!
//! * `msgcat::mcutil::getpreferences` (`library/msgcat/msgcat.tcl:362`) — the
//!   fallback chain, `de_de` → `{de_de de {}}`;
//! * `::tcl::clock::mcMerge` (`library/clock.tcl:558`) — merging that chain
//!   from the root outwards, so a locale inherits every key it does not set;
//! * `::msgcat::mcset` — the calls the `library/msgs/*.msg` files consist of,
//!   read here out of the byte-for-byte copies in [`crate::clock_msgs`].
//!
//! The root catalogue is the `::msgcat::mcmset {}` literal at
//! `library/clock.tcl:109`, and the per-locale `GREGORIAN_CHANGE_DATE` values
//! that follow it at `library/clock.tcl:163` are set in Tcl rather than in a
//! catalogue file, so both are transcribed here beside the reader.
//!
//! What is *not* here: a script cannot add a locale. tclsh's catalogue is open
//! — `::msgcat::mcmset en_US_roman {…}` in a script defines one — and this
//! frontend has no `msgcat`, so `-locale` reaches only the catalogues shipped
//! with the release. A name with no catalogue falls back exactly as tclsh's
//! does, which is what makes an unknown name answer in the root locale rather
//! than fail.

use crate::clock_msgs::CATALOGUES;

/// One `LOCALE_ERAS` row: the instant the era begins, the name to write for
/// `%EC`, and the year its first year is numbered from for `%Ey`.
#[derive(Clone, Debug)]
pub struct Era {
    pub start: i64,
    pub name: String,
    pub year: i64,
}

/// A merged catalogue — every key `clock` reads, already resolved through the
/// fallback chain, so nothing downstream has to know a locale was involved.
#[derive(Clone, Debug)]
pub struct Catalog {
    /// The most specific locale in the chain, lower-cased — `mcMerge`'s `L`
    /// key, which it sets whether or not that locale had a catalogue of its own
    /// (`library/clock.tcl:572` and `:581`, so `mcget zz_ZZ` answers `L zz_zz`
    /// with every other key the root's). It names the map `LocalizeFormat`
    /// caches under, and nothing else reads it.
    pub name: String,
    pub months_full: Vec<String>,
    pub months_abbrev: Vec<String>,
    pub days_full: Vec<String>,
    pub days_abbrev: Vec<String>,
    pub am: String,
    pub pm: String,
    pub bce: String,
    pub ce: String,
    pub eras: Vec<Era>,
    pub numerals: Vec<String>,
    pub date_format: String,
    pub time_format: String,
    pub time_format_12: String,
    pub time_format_24: String,
    pub time_format_24_secs: String,
    pub date_time_format: String,
    pub locale_date_format: String,
    pub locale_time_format: String,
    pub locale_date_time_format: String,
    pub locale_year_format: String,
    pub gregorian_change_date: i64,
}

/// The root catalogue: `::msgcat::mcmset {}` at `library/clock.tcl:109`.
impl Default for Catalog {
    fn default() -> Self {
        Catalog {
            name: String::new(),
            months_full: words(
                "January February March April May June \
                 July August September October November December",
            ),
            months_abbrev: words("Jan Feb Mar Apr May Jun Jul Aug Sep Oct Nov Dec"),
            days_full: words("Sunday Monday Tuesday Wednesday Thursday Friday Saturday"),
            days_abbrev: words("Sun Mon Tue Wed Thu Fri Sat"),
            am: "am".to_string(),
            pm: "pm".to_string(),
            bce: "B.C.E.".to_string(),
            ce: "C.E.".to_string(),
            eras: Vec::new(),
            numerals: (0..100).map(|n| format!("{n:02}")).collect(),
            date_format: "%m/%d/%Y".to_string(),
            time_format: "%H:%M:%S".to_string(),
            time_format_12: "%I:%M:%S %P".to_string(),
            time_format_24: "%H:%M".to_string(),
            time_format_24_secs: "%H:%M:%S".to_string(),
            date_time_format: "%a %b %e %H:%M:%S %Y".to_string(),
            locale_date_format: "%m/%d/%Y".to_string(),
            locale_time_format: "%H:%M:%S".to_string(),
            locale_date_time_format: "%a %b %e %H:%M:%S %Y".to_string(),
            locale_year_format: "%EC%Ey".to_string(),
            gregorian_change_date: 2299161,
        }
    }
}

/// Split a whitespace-separated literal, which is how the root catalogue's
/// list values are written in `clock.tcl`.
fn words(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_string).collect()
}

/// The `GREGORIAN_CHANGE_DATE` values `library/clock.tcl:163`–`:224` sets
/// outside any catalogue file, in the order it sets them. The locale names are
/// lower-cased here because that is the form a lookup arrives in.
const CHANGE_DATES: &[(&str, i64)] = &[
    ("it", 2299161),
    ("es", 2299161),
    ("pt", 2299161),
    ("pl", 2299161),
    ("fr", 2299227),
    ("fr_be", 2299238),
    ("nl_be", 2299238),
    ("de_at", 2299527),
    ("hu", 2301004),
    ("de_de", 2342032),
    ("nb", 2342032),
    ("nn", 2342032),
    ("no", 2342032),
    ("da", 2342032),
    ("nl", 2342165),
    ("fr_ch", 2361342),
    ("it_ch", 2361342),
    ("de_ch", 2361342),
    ("en", 2361222),
    ("sv", 2361390),
    ("ru", 2421639),
    ("ro", 2422063),
    ("el", 2423480),
];

/// The fallback chain for a locale, most specific first and the root locale
/// last — `msgcat::mcutil::getpreferences`, `library/msgcat/msgcat.tcl:362`.
pub fn preferences(locale: &str) -> Vec<String> {
    let locale = locale.to_lowercase();
    let mut result = vec![String::new()];
    if locale.is_empty() {
        // Tcl's `split {} _` is the empty list, so the loop below does not run
        // and the chain is the root locale alone: `getpreferences {}` is `{{}}`.
        return result;
    }
    let mut element = String::new();
    for part in locale.split('_') {
        if element.is_empty() {
            element = part.to_string();
        } else {
            element = format!("{element}_{part}");
        }
        if !element.ends_with('_') {
            result.insert(0, element.clone());
        }
    }
    result
}

/// `$language[_$territory][.$codeset][@modifier]` to
/// `$language[_$territory][_$modifier]` — `msgcat::mcutil::ConvertLocale`,
/// `library/msgcat/msgcat.tcl:1189`. `None` is its "empty language part"
/// error, which its callers catch and step past.
fn convert_locale(value: &str) -> Option<String> {
    let (head, modifier) = match value.split_once('@') {
        Some((head, modifier)) => (head, modifier),
        None => (value, ""),
    };
    let head = head.split('.').next().unwrap_or("");
    let (language, territory) = match head.split_once('_') {
        Some((language, territory)) => (language, territory),
        None => (head, ""),
    };
    if language.is_empty() {
        return None;
    }
    let mut out = language.to_string();
    if !territory.is_empty() {
        out.push('_');
        out.push_str(territory);
    }
    if !modifier.is_empty() {
        out.push('_');
        out.push_str(modifier);
    }
    Some(out)
}

/// The locale `system` and `current` name. On every platform this frontend
/// builds for, `::tcl::clock::GetSystemLocale` (`library/clock.tcl`) returns
/// the current `mclocale`, and that is initialised from the environment by
/// `msgcat::mcutil::getsystemlocale` (`library/msgcat/msgcat.tcl:1255`): the
/// first of `LC_ALL`, `LC_MESSAGES` and `LANG` that is set and converts, and
/// `c` when none does.
pub fn system_locale() -> String {
    for name in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(value) = std::env::var(name) {
            if value.is_empty() {
                continue;
            }
            if let Some(locale) = convert_locale(&value) {
                return locale;
            }
        }
    }
    "c".to_string()
}

/// The catalogue a `-locale` word resolves to.
///
/// `::tcl::clock::mcget` (`library/clock.tcl:513`) maps `system` and `current`
/// onto the system locale, lower-cases the rest, and merges the fallback chain
/// from the root outwards. Every name resolves: one with no catalogue anywhere
/// in its chain is the root locale, which is why `clock format … -locale zz`
/// answers in English rather than failing.
pub fn catalog(locale: &str) -> Catalog {
    let name = match locale.to_lowercase().as_str() {
        "system" | "current" => system_locale(),
        _ => locale.to_string(),
    };
    let chain = preferences(&name);
    let mut merged = Catalog::default();
    // The chain arrives most-specific-first; merging runs the other way so a
    // derived locale overwrites what it inherited.
    for step in chain.iter().rev() {
        apply(&mut merged, step);
    }
    // `L` is the locale that was asked for, not the last one that had
    // something to say — `mcMerge` sets it on the branch where the catalogue
    // is missing too.
    merged.name = chain[0].clone();
    merged
}

/// Merge one locale's own settings into `into`.
fn apply(into: &mut Catalog, locale: &str) {
    // `clock.tcl` sets these after loading the catalogue files, so they are
    // applied first and a file of the same locale can still override them.
    if let Some((_, date)) = CHANGE_DATES.iter().find(|(name, _)| *name == locale) {
        into.gregorian_change_date = *date;
    }
    if let Ok(at) = CATALOGUES.binary_search_by(|(name, _)| (*name).cmp(locale)) {
        read_catalogue(into, CATALOGUES[at].1);
    }
}

/// Read one `.msg` file: the `::msgcat::mcset <locale> <KEY> <value>` calls
/// `tools/loadICU.tcl` writes, whose values are either a quoted word or a
/// `[list "a" "b" …]` continued over several lines.
///
/// Anything else in the file is passed over rather than guessed at. The
/// generator (`scripts/gen_clock_msgs.py`) refuses to vendor a file whose
/// lines do not all have one of those shapes, so a release that changed the
/// shape fails there — loudly, at the point the data enters the tree — instead
/// of here.
fn read_catalogue(into: &mut Catalog, text: &str) {
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let Some(rest) = line.trim_start().strip_prefix("::msgcat::mcset ") else {
            continue;
        };
        let mut parts = rest.trim_start().splitn(3, ' ');
        let (Some(_locale), Some(key), Some(value)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let value = value.trim();
        let value = if let Some(head) = value.strip_prefix("[list") {
            // A list continued with `\` until a line ends in `]`.
            let mut collected = head.to_string();
            if !collected.trim_end().ends_with(']') {
                for more in lines.by_ref() {
                    collected.push(' ');
                    collected.push_str(more.trim());
                    if more.trim_end().ends_with(']') {
                        break;
                    }
                }
            }
            collected
        } else {
            value.to_string()
        };
        set(into, key, &value);
    }
}

/// One catalogue entry. `raw` is the value as the file wrote it: a `"…"` word,
/// a run of `"…"` words from a `[list …]`, or a bare number.
fn set(into: &mut Catalog, key: &str, raw: &str) {
    match key {
        "MONTHS_FULL" => into.months_full = quoted_words(raw),
        "MONTHS_ABBREV" => into.months_abbrev = quoted_words(raw),
        "DAYS_OF_WEEK_FULL" => into.days_full = quoted_words(raw),
        "DAYS_OF_WEEK_ABBREV" => into.days_abbrev = quoted_words(raw),
        "LOCALE_NUMERALS" => into.numerals = quoted_words(raw),
        "AM" => into.am = unquote(raw),
        "PM" => into.pm = unquote(raw),
        "BCE" => into.bce = unquote(raw),
        "CE" => into.ce = unquote(raw),
        "DATE_FORMAT" => into.date_format = unquote(raw),
        "TIME_FORMAT" => into.time_format = unquote(raw),
        "TIME_FORMAT_12" => into.time_format_12 = unquote(raw),
        "TIME_FORMAT_24" => into.time_format_24 = unquote(raw),
        "TIME_FORMAT_24_SECS" => into.time_format_24_secs = unquote(raw),
        "DATE_TIME_FORMAT" => into.date_time_format = unquote(raw),
        "LOCALE_DATE_FORMAT" => into.locale_date_format = unquote(raw),
        "LOCALE_TIME_FORMAT" => into.locale_time_format = unquote(raw),
        "LOCALE_DATE_TIME_FORMAT" => into.locale_date_time_format = unquote(raw),
        "LOCALE_YEAR_FORMAT" => into.locale_year_format = unquote(raw),
        "LOCALE_ERAS" => into.eras = eras(&unquote(raw)),
        "GREGORIAN_CHANGE_DATE" => {
            if let Some(day) = crate::list::parse_int(raw.trim()) {
                into.gregorian_change_date = day;
            }
        }
        // A key `clock` never reads. The catalogues carry none today; one that
        // appeared would be a value this port does not use, not a value it
        // would get wrong.
        _ => {}
    }
}

/// Strip one layer of double quotes, which is how every scalar in a generated
/// catalogue is written. The generator rejects a file whose values carry a
/// backslash escape, so there is nothing else to undo.
fn unquote(raw: &str) -> String {
    let trimmed = raw.trim();
    trimmed
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(trimmed)
        .to_string()
}

/// The `"a"\ "b"\ … "z"]` body of a `[list …]`, as its elements.
fn quoted_words(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = raw;
    while let Some(at) = rest.find('"') {
        rest = &rest[at + 1..];
        let Some(end) = rest.find('"') else { break };
        out.push(rest[..end].to_string());
        rest = &rest[end + 1..];
    }
    out
}

/// `LOCALE_ERAS` as its rows. Each is a three-element Tcl list of the era's
/// first instant, its name and the year its numbering starts from; a row that
/// is not that shape is dropped rather than half-read.
fn eras(raw: &str) -> Vec<Era> {
    let Ok(rows) = crate::list::split(raw) else {
        return Vec::new();
    };
    let mut out: Vec<Era> = rows
        .iter()
        .filter_map(|row| {
            let fields = crate::list::split(row).ok()?;
            let [start, name, year] = fields.as_slice() else {
                return None;
            };
            Some(Era {
                start: crate::list::parse_int(start.trim())?,
                name: name.clone(),
                year: crate::list::parse_int(year.trim())?,
            })
        })
        .collect();
    // `TclClockLookupLastTransition` binary-searches this list, which assumes
    // it is sorted; the shipped catalogues are, and sorting costs nothing.
    out.sort_by_key(|era| era.start);
    out
}

impl Catalog {
    /// The era covering `local_seconds`, or `None` when the catalogue has no
    /// eras or the instant is before the first — `TclClockLookupLastTransition`
    /// as `ClockFmtToken_LocaleERAYear_Proc` calls it
    /// (`generic/tclClockFmt.c:3005`).
    pub fn era_at(&self, local_seconds: i64) -> Option<&Era> {
        let at = self.eras.partition_point(|era| era.start <= local_seconds);
        (at > 0).then(|| &self.eras[at - 1])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chain_runs_from_the_name_to_the_root() {
        assert_eq!(preferences("de_DE"), ["de_de", "de", ""]);
        assert_eq!(preferences("en_US_roman"), ["en_us_roman", "en_us", "en", ""]);
        assert_eq!(preferences(""), [""]);
    }

    #[test]
    fn a_locale_inherits_what_it_does_not_set() {
        // `es_bo.msg` sets three formats and nothing else, so everything else
        // is `es`'s. Measured: `dict get [::tcl::clock::mcget es_BO] …` answers
        // `%d-%m-%Y`, `ene` and `a.C.`, the last two of which are `es`'s.
        let bolivia = catalog("es_BO");
        assert_eq!(bolivia.date_format, "%d-%m-%Y");
        assert_eq!(bolivia.months_abbrev[0], "ene");
        assert_eq!(bolivia.bce, "a.C.");
        assert_eq!(bolivia.months_abbrev, catalog("es").months_abbrev);
        assert_eq!(bolivia.bce, catalog("es").bce);
        // and `es` itself writes its own date format, which `es_BO` overrode.
        assert_eq!(catalog("es").date_format, "%e de %B de %Y");
    }

    #[test]
    fn an_unknown_locale_is_the_root_locale() {
        // Measured: `mcget zz_ZZ` answers `B.C.E.`, `Jan` and `%m/%d/%Y` — the
        // root's — under `L zz_zz`, its own name.
        let unknown = catalog("zz_ZZ");
        assert_eq!(unknown.months_full, Catalog::default().months_full);
        assert_eq!(unknown.bce, "B.C.E.");
        assert_eq!(unknown.date_format, "%m/%d/%Y");
        assert_eq!(unknown.name, "zz_zz");
        assert_eq!(catalog("es_BO").name, "es_bo");
        assert_eq!(catalog("").name, "");
    }

    #[test]
    fn the_change_date_comes_from_the_language_when_the_country_sets_none() {
        // `de_DE` sets one, `de_AT` sets its own, `de` alone falls to the root.
        assert_eq!(catalog("de_DE").gregorian_change_date, 2342032);
        assert_eq!(catalog("de_AT").gregorian_change_date, 2299527);
        assert_eq!(catalog("de").gregorian_change_date, 2299161);
        assert_eq!(catalog("en_GB").gregorian_change_date, 2361222);
    }

    #[test]
    fn eras_are_read_and_searched() {
        let japan = catalog("ja");
        assert_eq!(japan.eras.len(), 6);
        // 2009-02-13 is in the Heisei era, which numbers its years from 1988.
        let era = japan.era_at(1234567890).expect("an era covers 2009");
        assert_eq!(era.name, "平成");
        assert_eq!(era.year, 1988);
    }

    #[test]
    fn a_codeset_and_a_modifier_are_dropped() {
        assert_eq!(convert_locale("de_DE.UTF-8"), Some("de_DE".to_string()));
        assert_eq!(convert_locale("sr_RS@latin"), Some("sr_RS_latin".to_string()));
        assert_eq!(convert_locale(".UTF-8"), None);
    }
}
