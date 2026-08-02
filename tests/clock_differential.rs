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
    for name in ["tclsh", "tclsh9.0", "tclsh8.6"] {
        if let Ok(out) = Command::new("sh")
            .arg("-c")
            .arg(format!("command -v {name}"))
            .output()
        {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
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
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    compare(&tclsh, PROGRAMS);
}

#[test]
fn named_time_zones_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    if !std::path::Path::new("/usr/share/zoneinfo/America/New_York").exists() {
        eprintln!("skipping: no zone database on this machine");
        return;
    }
    compare(&tclsh, ZONE_PROGRAMS);
}
