//! Differential execution of the `clock` ensemble against tclsh.
//!
//! Every instant here is a *fixed* epoch and every zone is named, so nothing
//! in this file depends on when or where it runs. `clock seconds` and its
//! siblings appear only through properties that hold for any answer — that the
//! three units agree with each other, and that formatting the current second
//! and scanning the result gives it back — because a program that printed the
//! current time would compare two different instants and fail at random.
//!
//! The named zones are the ones a stock `tzdata` install carries. If a zone
//! file is missing both interpreters refuse, and the refusals differ, so the
//! zone programs are skipped when the zone database is not present rather than
//! failing for a reason that is not about this crate.

use std::path::PathBuf;
use std::process::Command;

/// Programs that need nothing but a working `libc` and the `TZif` reader.
const PROGRAMS: &[&str] = &[
    // The default format, and a fixed instant either side of the epoch.
    "puts [clock format 1234567890 -gmt 1]",
    "puts [clock format 0 -gmt 1]",
    "puts [clock format -1 -gmt 1]",
    "puts [clock format 2147483647 -gmt 1]",
    "puts [clock format 4102444800 -gmt 1]",
    "puts [clock format 951782400 -gmt 1]",
    // Every token the format scanner knows, plus the ones it copies through.
    "puts [clock format 1234567890 -gmt 1 -format {%Y|%m|%d|%H|%M|%S|%j|%w|%u}]",
    "puts [clock format 1234567890 -gmt 1 -format {%Z|%z|%e|%b|%B|%a|%A|%p|%P}]",
    "puts [clock format 1234567890 -gmt 1 -format {%s|%V|%G|%g|%C|%y|%I|%k|%l|%N|%J}]",
    "puts [clock format 1234567890 -gmt 1 -format {%U|%W}]",
    "puts [clock format 1234567890 -gmt 1 -format {%D|%T|%R|%r|%x|%X|%c|%+}]",
    "puts [clock format 1234567890 -gmt 1 -format {%%|%n|%t|end}]",
    "puts [clock format 1234567890 -gmt 1 -format {%F %i %O}]",
    "puts [clock format 1234567890 -gmt 1 -format {}]",
    // The week numbering, over a year boundary where ISO and the two
    // `%U`/`%W` counts disagree with each other.
    "foreach t {1609459199 1609459200 1609545600 1104537600 1104624000} {puts [clock format $t -gmt 1 -format {%Y %j %V %G %U %W %u %w}]}",
    "foreach t {946684800 978307200 1009843200 1041379200 1072915200} {puts [clock format $t -gmt 1 -format {%Y-%m-%d %V %G}]}",
    // Leap years and month lengths.
    "foreach t {951782400 1078012800 4107456000 1583020800} {puts [clock format $t -gmt 1 -format {%Y-%m-%d %j}]}",
    // Midnight and noon, which is where the twelve-hour tokens turn over.
    "foreach t {0 43200 43199 86399 86400} {puts [clock format $t -gmt 1 -format {%H %I %k %l %p %P}]}",
    // A fixed numeric offset, which needs no zone database at all.
    "puts [clock format 1234567890 -format {%Y-%m-%dT%H:%M:%S %Z %z} -timezone +0530]",
    "puts [clock format 1234567890 -format {%Y-%m-%dT%H:%M:%S %Z %z} -timezone {-05:30}]",
    "puts [clock format 1234567890 -format {%Y-%m-%dT%H:%M:%S %Z %z} -timezone +01]",
    "puts [clock format 1234567890 -format {%Y-%m-%dT%H:%M:%S %Z %z} -timezone :UTC]",
    "puts [clock format 1234567890 -format {%Z %z} -timezone :GMT]",
    // `-locale` in the spellings that mean the root catalogue.
    "puts [clock format 1234567890 -gmt 1 -locale C]",
    "puts [clock format 1234567890 -gmt 1 -locale current]",
    // Scanning, which has to be the inverse of formatting.
    "puts [clock scan {2009-02-13 23:31:30} -format {%Y-%m-%d %H:%M:%S} -gmt 1]",
    "puts [clock scan {02/13/2009} -format {%D} -gmt 1]",
    "puts [clock scan {Fri Feb 13 2009} -format {%a %b %d %Y} -gmt 1]",
    "puts [clock scan {13 February 2009} -format {%d %B %Y} -gmt 1]",
    "puts [clock scan {2009-044} -format {%Y-%j} -gmt 1]",
    "puts [clock scan {11:31:30 pm 2009-02-13} -format {%I:%M:%S %P %Y-%m-%d} -gmt 1]",
    "puts [clock scan 1234567890 -format %s -gmt 1]",
    "puts [clock scan {09-02-13} -format {%y-%m-%d} -gmt 1]",
    "puts [clock scan {99-02-13} -format {%y-%m-%d} -gmt 1]",
    "puts [clock scan {20090213} -format {%Y%m%d} -gmt 1]",
    "puts [clock scan {2009-02-13 23:31:30 +0000} -format {%Y-%m-%d %H:%M:%S %z} -gmt 1]",
    "puts [clock scan {2009-02-13 23:31:30 -0500} -format {%Y-%m-%d %H:%M:%S %z} -gmt 1]",
    "puts [clock scan {Feb 13, 2009} -format {%b %d, %Y} -gmt 1]",
    "puts [clock scan {2009-02-13T23:31:30} -format {%Y-%m-%dT%H:%M:%S} -gmt 1]",
    "foreach t {0 1 1234567890 2147483647 951782400} {puts [clock scan [clock format $t -gmt 1 -format {%Y-%m-%d %H:%M:%S}] -format {%Y-%m-%d %H:%M:%S} -gmt 1]}",
    // Arithmetic, including the month clamp and the weekday walk.
    "foreach {n u} {1 day 1 month 1 week 2 weeks 90 seconds 1 hours 100 minutes -1 year 13 months 31 days 0 days} {puts [clock add 1234567890 $n $u -gmt 1]}",
    "foreach {n u} {1 weekdays 5 weekdays -5 weekdays 10 weekdays} {puts [clock add 1234567890 $n $u -gmt 1]}",
    "puts [clock add 1234567890 -3 months 2 years -gmt 1]",
    // 2006-01-31 plus a month is 2006-02-28: the day is clamped to the target
    // month rather than spilling into March.
    "puts [clock format [clock add 1138665600 1 month -gmt 1] -gmt 1 -format {%Y-%m-%d}]",
    "puts [clock format [clock add 1141084800 -1 month -gmt 1] -gmt 1 -format {%Y-%m-%d}]",
    "puts [clock add 1234567890 -gmt 1]",
    // Refusals, caught so the message is what is compared.
    "puts [catch {clock format abc} m]\nputs $m",
    "puts [catch {clock format} m]\nputs $m",
    "puts [catch {clock format 1 -bogus 2} m]\nputs $m",
    "puts [catch {clock format 1 -format} m]\nputs $m",
    "puts [catch {clock seconds 1} m]\nputs $m",
    "puts [catch {clock milliseconds 1} m]\nputs $m",
    "puts [catch {clock bogus} m]\nputs $m",
    "puts [catch {clock} m]\nputs $m",
    "puts [catch {clock add 1234567890 1 bogus -gmt 1} m]\nputs $m",
    "puts [catch {clock scan xyz -format %Y -gmt 1} m]\nputs $m",
    "puts [catch {clock scan {2009-02-13} -format {%Y-%m-%d} -gmt 1 -timezone :UTC} m]\nputs $m",
    "puts [catch {clock scan {2009-02-13 extra} -format {%Y-%m-%d} -gmt 1} m]\nputs $m",
    // The three units are read from one clock, so they agree with each other
    // however long the program takes to run.
    "set s [clock seconds]\nset ms [clock milliseconds]\nputs [expr {$ms/1000 - $s <= 1}]",
    "set us [clock microseconds]\nset ms [clock milliseconds]\nputs [expr {abs($ms - $us/1000) <= 1000}]",
    "puts [expr {[clock clicks -microseconds] > 0}]",
    "puts [expr {[clock clicks -milliseconds] > 0}]",
    "puts [string is entier [clock clicks]]",
    // Formatting the current second and scanning it back is the identity,
    // whatever second that is.
    "set now [clock seconds]\nputs [expr {[clock scan [clock format $now -gmt 1 -format {%Y-%m-%d %H:%M:%S}] -format {%Y-%m-%d %H:%M:%S} -gmt 1] == $now}]",
];

/// `-locale`: the message catalogue, the `%E`/`%O` token maps and the
/// locale-dependent format groups.
///
/// One program per locale-sensitive behaviour rather than per locale, since
/// the whole shipped catalogue set is swept in a single program below. These
/// run in one tclrs process while each reaches tclsh as its own, so a
/// behaviour that fires only on a locale's first use appears exactly once
/// here — see the `he` program.
const LOCALE_PROGRAMS: &[&str] = &[
    // The names, words and format groups a locale supplies.
    "foreach l {de fr es ja zh ru it pt nl sv pl tr ko} {puts [clock format 1234567890 -gmt 1 -locale $l -format {%a|%A|%b|%B|%p|%P}]}",
    "foreach l {de fr es ja zh ru en_GB en_US it} {puts [clock format 1234567890 -gmt 1 -locale $l -format {%x|%X|%c|%r|%D|%T|%R|%+}]}",
    "foreach l {de fr ja zh ru} {puts [clock format 1234567890 -gmt 1 -locale $l -format {%Ex|%EX|%Ec|%EY}]}",
    // A locale inherits from its language, and a name with no catalogue
    // anywhere in its chain is the root locale rather than an error.
    "foreach l {es_BO es_AR de_AT de_CH fr_CA pt_BR zz_ZZ qqq xx_YY_zz {}} {puts [clock format 1234567890 -gmt 1 -locale $l -format {%B|%x|%A}]}",
    // `system` and `current` name the same catalogue, and a codeset or a
    // modifier on the name is dropped.
    "foreach l {system current C POSIX de_DE.UTF-8 sr_RS@latin} {puts [clock format 1234567890 -gmt 1 -locale $l -format {%B %x}]}",
    // The era tokens, in a locale that has eras and in one that has none.
    "foreach t {-1000000000 0 600220800 1234567890 1556668800 1600000000} {puts [clock format $t -gmt 1 -locale ja -format {%EE|%EC|%Ey|%EY}]}",
    "foreach t {-1000000000 0 1234567890} {puts [clock format $t -gmt 1 -locale en_US -format {%EE|%EC|%Ey|%EY}]}",
    // The locale numerals `%O…` writes, in the one locale that supplies its
    // own and in one that inherits the root's digits.
    "foreach t {0 1234567890 946684800} {puts [clock format $t -gmt 1 -locale zh -format {%Od|%Oe|%Om|%Oy|%OH|%Ok|%OI|%Ol|%OM|%OS|%Ou|%Ow}]}",
    "foreach t {0 1234567890} {puts [clock format $t -gmt 1 -locale de -format {%Od|%Om|%Oy|%OH|%OM|%OS|%Ou|%Ow}]}",
    // The Julian day tokens and the stardate, which no locale changes but
    // which only `%E` reaches.
    "foreach t {0 1 43199 43200 43201 86399 1234567890 -6857222400 -6857136000} {puts [clock format $t -gmt 1 -format {%EJ|%Ej|%Es|%Q}]}",
    // A group whose token is in neither map keeps its percent, its modifier
    // and all.
    "puts [clock format 1234567890 -gmt 1 -format {%EQ|%Oq|%F|%i|%E|%O|%}]",
    // The changeover is the same for every locale: Tcl 9 formats from the
    // compile-time date and never reads the catalogue's, so the locales that
    // set a later one still answer Gregorian above it.
    "foreach l {en it ru el {}} {puts [clock format -6857222400 -gmt 1 -locale $l -format {%Y-%m-%d}]}",
    // `mt.msg` ships six weekday abbreviations, so `%a` on a Saturday indexes
    // past the end of the list and tclsh reports that with no message at all.
    "puts [catch {clock format 946684800 -gmt 1 -locale mt -format %a} m]\nputs [list $m]\nputs [clock format 0 -gmt 1 -locale mt -format %a]",
    // `he.msg` is not valid Tcl — its era words carry an unescaped quote — so
    // sourcing it stops there. The first command to ask for the locale raises
    // the parser's message and the next one answers, with every key below the
    // bad line missing. This is the only `he` program in the file: the
    // behaviour fires once per process and tclrs runs them all in one.
    "puts [catch {clock format 0 -gmt 1 -locale he -format %c} m]\nputs $m\nputs [clock format 1234567890 -gmt 1 -locale he -format {%x|%B|%EE}]",
    // Scanning reads the locale's names and its AM/PM words too.
    "puts [clock scan {13 Februar 2009} -format {%d %B %Y} -gmt 1 -locale de]",
    "puts [clock scan {13 fév 2009} -format {%d %b %Y} -gmt 1 -locale fr]",
    "puts [clock scan {2009-02-13 11:31:30 nachm.} -format {%Y-%m-%d %I:%M:%S %P} -gmt 1 -locale de]",
    "puts [clock scan {13.02.2009} -format %x -gmt 1 -locale de]",
    "puts [catch {clock scan {13 February 2009} -format {%d %B %Y} -gmt 1 -locale de} m]\nputs $m",
    // A name may be abbreviated as far as it stays unique, and no further.
    "foreach s {f fé fév févr {févr.} février FÉV ja jan janv juin juil mar mars mai} {puts [clock scan \"13 $s 2009\" -format {%d %b %Y} -gmt 1 -locale fr]}",
    "foreach s {j ju jui m ma} {puts [catch {clock scan \"13 $s 2009\" -format {%d %b %Y} -gmt 1 -locale fr} m]\nputs $m}",
    "foreach s {Ja Jan January F Fe Feb Febr February Marc Sep Sept September} {puts [clock scan \"13 $s 2009\" -format {%d %b %Y} -gmt 1]}",
    "foreach s {J M} {puts [catch {clock scan \"13 $s 2009\" -format {%d %b %Y} -gmt 1} m]\nputs $m}",
    "foreach s {F Fr Fri Frid Friday} {puts [clock scan \"$s 13 Feb 2009\" -format {%a %d %b %Y} -gmt 1]}",
    "foreach s {a am AM p pm PM} {puts [clock scan \"2009-02-13 11:31:30 $s\" -format {%Y-%m-%d %I:%M:%S %P} -gmt 1]}",
    "foreach s {n nachm vorm} {puts [clock scan \"2009-02-13 11:31:30 $s\" -format {%Y-%m-%d %I:%M:%S %P} -gmt 1 -locale de]}",
    "foreach s {a am p pm} {puts [catch {clock scan \"2009-02-13 11:31:30 $s\" -format {%Y-%m-%d %I:%M:%S %P} -gmt 1 -locale de} m]\nputs $m}",
    // A weekday in the input is checked against the date rather than ignored,
    // and `%w`'s Sunday 0 and `%u`'s Sunday 7 are the same day.
    "foreach {f s} {{%a %d %b %Y} {Fri 13 Feb 2009} {%u %d %b %Y} {5 13 Feb 2009} {%w %d %b %Y} {5 13 Feb 2009} {%A %d %b %Y} {Friday 13 Feb 2009} {%a %Y-%j} {Fri 2009-044}} {puts [clock scan $s -format $f -gmt 1]}",
    "foreach {f s} {{%a %d %b %Y} {Sat 13 Feb 2009} {%u %d %b %Y} {6 13 Feb 2009} {%w %d %b %Y} {6 13 Feb 2009} {%u %d %b %Y} {0 13 Feb 2009} {%u %d %b %Y} {7 13 Feb 2009} {%w %d %b %Y} {0 13 Feb 2009} {%A %d %b %Y} {Saturday 13 Feb 2009} {%a %Y-%j} {Sat 2009-044} {%u %d %b %Y} {9 13 Feb 2009}} {puts [catch {clock scan $s -format $f -gmt 1} m]\nputs $m}",
    "foreach {f s} {{%a %d %b %Y} {Fr 13 Feb 2009} {%a %d %b %Y} {Freitag 13 Feb 2009}} {puts [clock scan $s -format $f -gmt 1 -locale de]}",
    // `%J`, `%EJ` and `%Ej`: a Julian day whole names a local day, and one
    // written with a fraction is the instant itself, which no zone touches.
    "foreach s {0 2440588 2451545 -1 1721426} {puts [clock scan $s -format %J -gmt 1]}",
    "foreach s {2440588 2451545} {puts [clock scan $s -format %J -timezone :America/New_York]}",
    "foreach s {2440588 2440588.0 2440588.25 2440588.5 2440587.75 2451545.123456} {puts [clock scan $s -format %EJ -gmt 1]}",
    "foreach s {2440588 2440587.5 2440588.0 2440588.25} {puts [clock scan $s -format %Ej -gmt 1]}",
    "foreach s {2440588 2440588.25} {puts [clock scan $s -format %EJ -timezone :America/New_York]}\nputs [clock scan 2440588 -format %Ej -timezone :America/New_York]",
    // `%Es` is a local instant where `%s` is an absolute one.
    "foreach s {0 1234567890 -1000000} {puts [list [clock scan $s -format %Es -gmt 1] [clock scan $s -format %s -gmt 1]]}",
    "puts [list [clock scan 1234567890 -format %Es -timezone :America/New_York] [clock scan 1234567890 -format %s -timezone :America/New_York]]",
    // Round-tripping the two Julian-day tokens through their own formatter.
    "foreach t {0 1 43200 86399 1234567890 -757382400} {puts [clock scan [clock format $t -format %EJ -gmt 1] -format %EJ -gmt 1]}",
    "foreach t {0 43200 1234567890} {puts [clock scan [clock format $t -format %Ej -gmt 1] -format %Ej -gmt 1]}",
    // `%Q` round-trips through the stardate, and refuses a number too short to
    // carry both a year and its thousandths.
    "foreach t {-757382400 1482857280 1234567890 0} {set d [clock format $t -format %Q -gmt 1]\nputs [list $d [clock scan $d -format %Q -gmt 1]]}",
    "foreach s {{Stardate 999.9} {Stardate 70986} {stardate} {Star 70986.7}} {puts [catch {clock scan $s -format %Q -gmt 1} m]\nputs $m}",
    "foreach s {{stardate 70986.7} {STARDATE 70986.7} {Stardate    70986.7} {Stardate 070986.75} {Stardate +70986.7}} {puts [clock scan $s -format %Q -gmt 1]}",
    "puts [clock scan {Stardate 00000.0} -format %Q -timezone :America/New_York]",
    // `%O…` reads a locale numeral, which in the root catalogue is always two
    // digits — so a bare `5` is refused where `05` is read.
    "foreach s {05 12 31} {puts [clock scan \"1970 Jan $s\" -format {%Y %b %Od} -gmt 1]}",
    "foreach s {00 32 99} {puts [catch {clock scan \"1970 Jan $s\" -format {%Y %b %Od} -gmt 1} m]\nputs $m}",
    "puts [clock scan {1970 05 05} -format {%Y %Om %Od} -gmt 1]",
    "puts [clock scan {70 01 05} -format {%Oy %Om %Od} -gmt 1]",
    "puts [clock scan {1970 01 05 12 30 45} -format {%Y %Om %Od %OH %OM %OS} -gmt 1]",
    "foreach s {5 1 x} {puts [catch {clock scan \"1970 Jan $s\" -format {%Y %b %Od} -gmt 1} m]\nputs $m}",
    "foreach s {03 3} {puts [catch {clock scan $s -format %Ou -gmt 1} m]}",
    "foreach s {05 03} {puts [catch {clock scan \"1970 01 02 $s\" -format {%Y %Om %Od %Ou} -gmt 1} m]\nputs $m}",
    // `%Ey` matches a numeral and captures nothing.
    "puts [list [clock scan 70 -format %Ey -gmt 1] [clock scan {} -format {} -gmt 1]]",
    // `%EE`: the catalogue words and the four fixed spellings, in the
    // abbreviations each of them still resolves at.
    "foreach s {C.E. c.e. a.d. A.D. c. a.} {puts [clock scan \"$s 1970\" -format {%EE %Y} -gmt 1]}",
    "foreach s {C.E. a.d.} {puts [clock scan \"$s 1970\" -format {%EE %Y} -gmt 1 -locale de]}",
    // Every date field the format leaves out comes from the base day, and the
    // time starts at midnight rather than following it.
    "foreach f {%Y {%Y %m} {%Y %d} {%m %d} %m %d %y {%C %y} %C {%Y %j} %j {%Y %b} %b} {puts [clock format [clock scan [clock format 0 -format $f -gmt 1] -format $f -gmt 1] -format {%Y-%m-%d %H:%M:%S} -gmt 1]}",
    "puts [clock format [clock scan 12 -format %H -gmt 1] -format %H:%M:%S -gmt 1]",
    // Each scanned field is held to its own range, named in its own refusal,
    // in the order tclsh checks them.
    "foreach d {01 28 29 30 31} {puts [clock scan \"1970 01 $d\" -format {%Y %m %d} -gmt 1]}",
    "foreach d {00 32 99} {puts [catch {clock scan \"1970 01 $d\" -format {%Y %m %d} -gmt 1} m]\nputs $m}",
    "foreach d {28 29 30} {puts [catch {clock scan \"1970 02 $d\" -format {%Y %m %d} -gmt 1} m]\nputs $m}",
    "foreach d {28 29 30} {puts [catch {clock scan \"1972 02 $d\" -format {%Y %m %d} -gmt 1} m]\nputs $m}",
    "foreach m {00 13 99} {puts [catch {clock scan \"1970 $m 01\" -format {%Y %m %d} -gmt 1} m2]\nputs $m2}",
    "foreach h {00 23 24 25 99} {puts [catch {clock scan \"1970 01 01 $h\" -format {%Y %m %d %H} -gmt 1} m]\nputs $m}",
    "foreach x {00 59 60 99} {puts [catch {clock scan \"1970 01 01 00 $x\" -format {%Y %m %d %H %M} -gmt 1} m]\nputs $m}",
    "foreach x {00 59 60 99} {puts [catch {clock scan \"1970 01 01 00 00 $x\" -format {%Y %m %d %H %M %S} -gmt 1} m]\nputs $m}",
    "foreach j {000 001 365 366 367 999} {puts [catch {clock scan \"1970 $j\" -format {%Y %j} -gmt 1} m]\nputs $m}",
    "foreach j {365 366 367} {puts [catch {clock scan \"1972 $j\" -format {%Y %j} -gmt 1} m]\nputs $m}",
    "foreach h {00 01 12 13 24} {puts [catch {clock scan \"1970 01 01 $h am\" -format {%Y %m %d %I %P} -gmt 1} m]\nputs $m}",
    "foreach h {00 12 13} {puts [catch {clock scan \"1970 01 01 $h pm\" -format {%Y %m %d %I %P} -gmt 1} m]\nputs $m}",
    // The earlier field is the one reported: a bad month is named even when
    // the day is bad too.
    "foreach p {{1970 13 32} {1970 01 32}} {puts [catch {clock scan $p -format {%Y %m %d} -gmt 1} m]\nputs $m}",
    "puts [catch {clock scan {1970 01 32 25} -format {%Y %m %d %H} -gmt 1} m]\nputs $m",
    // `-base` is the instant the fields the format did not carry come from,
    // in place of the current one.
    "foreach b {0 1234567890 -1009843200 946684800} {puts [clock scan {} -format {} -gmt 1 -base $b]}",
    "foreach f {%Y %m %d %H {%Y %m} {%m %d} %j %y} {puts [clock scan [clock format 1234567890 -format $f -gmt 1] -format $f -gmt 1 -base 0]}",
    "foreach b {0 1234567890} {puts [clock scan 1999 -format %Y -gmt 1 -base $b]}",
    "puts [clock scan {} -format {} -base 0 -timezone :America/New_York]",
    "puts [clock scan {Jan 02} -format {%b %d} -base -1009843200 -gmt 1]",
    "puts [clock scan 05 -format %d -base 1234567890 -gmt 1]",
    // `%s` and a whole Julian day are the instant itself, so the base cannot
    // reach them.
    "puts [list [clock scan 1234567890 -format %s -gmt 1 -base 0] [clock scan 2440588 -format %J -gmt 1 -base 999]]",
    // `-base now` is the reading the default already is, and a base that is not
    // a number is refused in the words `clock format`'s value is.
    "puts [expr {[clock scan {} -format {} -gmt 1 -base now] == [clock scan {} -format {} -gmt 1]}]",
    "puts [catch {clock scan {} -format {} -gmt 1 -base abc} m]\nputs $m",
    // The same catalogue read twice answers the same, which is what the
    // memoised merge has to preserve.
    "foreach i {1 2 3} {puts [clock format 1234567890 -gmt 1 -locale de_AT -format {%B %x %A}]}",
];

/// Programs that need a zone file from the system's `tzdata`.
const ZONE_PROGRAMS: &[&str] = &[
    "foreach t {1234567890 0 1000000000 1609459200 4102444800} {puts [clock format $t -format {%Y-%m-%dT%H:%M:%S %Z %z} -timezone :America/New_York]}",
    "foreach t {1234567890 0 1000000000 1609459200 4102444800} {puts [clock format $t -format {%Y-%m-%dT%H:%M:%S %Z %z} -timezone :Europe/Berlin]}",
    "foreach t {1234567890 0 1000000000 1609459200} {puts [clock format $t -format {%Y-%m-%dT%H:%M:%S %Z %z} -timezone :Asia/Kolkata]}",
    "foreach t {1234567890 1583020800} {puts [clock format $t -format {%Y-%m-%dT%H:%M:%S %Z %z} -timezone :Australia/Lord_Howe]}",
    // The instants either side of a daylight change in both directions.
    "foreach t {1236495600 1236499200 1257051600 1257055200} {puts [clock format $t -format {%Y-%m-%dT%H:%M:%S %Z %z} -timezone :America/New_York]}",
    "puts [clock scan {2009-02-13 18:31:30} -format {%Y-%m-%d %H:%M:%S} -timezone :America/New_York]",
    "puts [clock scan {2009-07-13 12:00:00} -format {%Y-%m-%d %H:%M:%S} -timezone :America/New_York]",
    "puts [clock add 1234567890 1 month -timezone :America/New_York]",
    "puts [clock add 1234567890 1 day -timezone :Europe/Berlin]",
    "puts [clock format 1234567890 -format {%Z %z} -timezone EST5EDT]",
    "puts [clock format 1234567890 -format {%Z %z} -timezone CET]",
];

fn tclsh() -> Option<PathBuf> {
    for name in ["tclsh9.0", "tclsh", "tclsh8.6"] {
        let Ok(out) = Command::new("sh")
            .arg("-c")
            .arg(format!("command -v {name}"))
            .output()
        else {
            continue;
        };
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if path.is_empty() {
            continue;
        }
        // Only the exact release this port is written against is an oracle.
        // tclrs targets 9.0.4 (`src/cmd_info.rs`'s `TCL_PATCHLEVEL`), and a
        // reference from any other release reports ITS version's differences
        // as tclrs failures: 8.6 words errors differently ("couldn't compile
        // regular expression" for "cannot compile") and has a different
        // ensemble membership, while 9.0.3 predates the lseq fixes (a zero
        // step yields the empty list where the manual says it yields `count`
        // elements, and a bareword argument is still an expr). The ubuntu CI
        // image ships 8.6, so CI skips these and they run against a matching
        // tclsh locally.
        let Ok(v) = Command::new("sh")
            .arg("-c")
            .arg(format!("printf 'puts [info patchlevel]\\n' | {path}"))
            .output()
        else {
            continue;
        };
        if String::from_utf8_lossy(&v.stdout).trim() == "9.0.4" {
            return Some(PathBuf::from(path));
        }
    }
    None
}

/// Distinct per call: the two tests in this file run at the same time, and a
/// shared scratch name let one delete the other's program mid-run.
static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn reference_output(tclsh: &PathBuf, program: &str) -> String {
    let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("tclrs-clock-{}-{serial}.tcl", std::process::id()));
    std::fs::write(&path, program).expect("write program");
    let out = Command::new(tclsh).arg(&path).output().expect("run tclsh");
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "tclsh rejected program:\n{program}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn compare(tclsh: &PathBuf, programs: &[&str]) {
    let mut failures = Vec::new();
    for program in programs {
        let expected = reference_output(tclsh, program);
        match tclrs::eval(program) {
            Ok(outcome) if outcome.output == expected => {}
            Ok(outcome) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs: {:?}",
                outcome.output
            )),
            Err(e) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs failed: {e}"
            )),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} programs diverge:\n\n{}",
        failures.len(),
        programs.len(),
        failures.join("\n\n")
    );
}

#[test]
fn clock_matches_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    compare(&tclsh, PROGRAMS);
}

#[test]
fn locales_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    compare(&tclsh, LOCALE_PROGRAMS);
}

/// Every catalogue the release ships, over instants spread across the year and
/// either side of the epoch, against every token group. This is the sweep that
/// would catch a catalogue read one field out; the programs above are the ones
/// that say what each behaviour is.
#[test]
fn every_shipped_locale_matches_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    // `he` is left out: its first use raises the parser's error and its later
    // ones do not, so a sweep that reaches it in one tclrs process cannot line
    // up with tclsh's one process per program. The program above covers it.
    let program = "\
set locales [list af_za af ar_in ar_jo ar_lb ar_sy ar be bg bn_in bn ca cs da de_at de_be de el \
en_au en_be en_bw en_ca en_gb en_hk en_ie en_in en_nz en_ph en_sg en_za en_zw eo es_ar es_bo \
es_cl es_co es_cr es_do es_ec es_gt es_hn es_mx es_ni es_pa es_pe es_pr es_py es_sv es_uy es_ve \
es et eu_es eu fa_in fa_ir fa fi fo_fo fo fr_be fr_ca fr_ch fr ga_ie ga gl_es gl gv_gb gv hi_in \
hi hr hu id_id id is it_ch it ja kl_gl kl ko_kr ko kok_in kok kw_gb kw lt lv mk mr_in mr ms_my \
ms mt nb nl_be nl nn pl pt_br pt ro ru_ua ru sh sk sl sq sr sv sw ta_in ta te_in te th tr uk vi \
zh_cn zh_hk zh_sg zh_tw zh]
foreach loc $locales {
  foreach t {0 1234567890 987654321 -100000000 946684800 1600000000} {
    foreach f {{%a %A %b %B %p %P %x %X %c %r} {%EE %EY %Ey %EC %EJ %Ej %Es} \\
               {%Od %Oe %Om %Oy %OH %Ok %OI %Ol %OM %OS %Ou %Ow} {%Ex %EX %Ec %D %T %R %+}} {
      if {[catch {clock format $t -format $f -gmt 1 -locale $loc} r]} { set r \"ERR: $r\" }
      puts \"$loc|$t|$f => $r\"
    }
  }
}";
    compare(&tclsh, &[program]);
}

#[test]
fn named_time_zones_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    if !std::path::Path::new("/usr/share/zoneinfo/America/New_York").exists() {
        eprintln!("skipping: no zone database on this machine");
        return;
    }
    compare(&tclsh, ZONE_PROGRAMS);
}
