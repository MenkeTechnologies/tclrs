//! Replay every committed differential-fuzz finding.
//!
//! `scripts/fuzz_parity.sh -m` minimises each case it finds and writes two files
//! into `tests/fuzz_corpus/`: the reduced program, and a `.expected` record of
//! what both engines did with it — stdout, exit status and the error message, for
//! tclsh and for tclrs. This test runs every one of those cases again, through
//! the same driver the fuzzer used, and compares both engines against the record.
//!
//! What it pins, and why that is the useful thing to pin:
//!
//! * **tclsh's half** is ground truth. If the reference interpreter no longer
//!   produces what was recorded, the record was made against a different tclsh
//!   and the finding needs re-measuring — not silently accepting.
//! * **tclrs's half** is the current behavior. A fix changes it, and this test
//!   fails to say so: that is the prompt to re-record the case with
//!   `scripts/fuzz_parity.sh -R`, move the now-fixed finding into
//!   `tests/parity_fuzz_findings.rs` as an assertion against tclsh, and note it
//!   in BUGS.md. A corpus that quietly tolerated either side changing would
//!   measure nothing.
//!
//! No expectation here is written by hand; every byte came from a run of the two
//! binaries.

use std::path::{Path, PathBuf};
use std::process::Command;

const TCLRS: &str = env!("CARGO_BIN_EXE_tclrs");

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

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// What one `.expected` record says the two engines did.
#[derive(Debug, Default)]
struct Record {
    verdict: String,
    tclsh_status: i32,
    tclsh_out: String,
    tclsh_msg: String,
    tclrs_status: i32,
    tclrs_out: String,
    tclrs_err: String,
}

/// The scratch path of the file the engines ran is not stable between machines,
/// so the location tclrs prints is compared without it — the line number, which
/// is the part that carries meaning, is kept.
fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("(file \"") {
        out.push_str(&rest[..i]);
        let after = &rest[i + "(file \"".len()..];
        match after.find('"') {
            Some(j) => {
                out.push_str("(file ");
                rest = &after[j + 1..];
                // Drop the space that separated the path from `line N`.
                rest = rest.strip_prefix(' ').unwrap_or(rest);
            }
            None => {
                out.push_str(after);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// Undo `scripts/fuzz/record.sh`'s escaping: `\n`, `\t`, `\xNN`, `\\`.
///
/// A captured stdout ends with the driver's record separator and no newline, so
/// the record keeps each stream escaped onto one header line; anything else could
/// not be read back without guessing where a field ended.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some('x') => {
                let hex: String = chars.by_ref().take(2).collect();
                match u8::from_str_radix(&hex, 16) {
                    Ok(b) => out.push(b as char),
                    Err(_) => panic!("bad \\x escape in record: {hex:?}"),
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn parse_record(text: &str) -> Record {
    let mut rec = Record::default();
    let mut seen = 0;
    for line in text.lines() {
        let Some(field) = line.strip_prefix("# ") else {
            continue;
        };
        let Some((name, value)) = field.split_once(": ").or_else(|| {
            // A field whose value is empty has no trailing space.
            field.strip_suffix(':').map(|n| (n, ""))
        }) else {
            continue;
        };
        seen += 1;
        match name {
            "verdict" => rec.verdict = value.to_string(),
            "tclsh-status" => rec.tclsh_status = value.trim().parse().expect("tclsh-status"),
            "tclsh-stdout" => rec.tclsh_out = unescape(value),
            "tclsh-stderr" => rec.tclsh_msg = unescape(value),
            "tclrs-status" => rec.tclrs_status = value.trim().parse().expect("tclrs-status"),
            "tclrs-stdout" => rec.tclrs_out = unescape(value),
            "tclrs-stderr" => rec.tclrs_err = unescape(value),
            _ => seen -= 1,
        }
    }
    assert_eq!(
        seen, 7,
        "a record must carry all seven observation fields, found {seen}"
    );
    rec
}

/// The case, spliced into `scripts/fuzz/drive.tcl` exactly as the fuzzer splices
/// it — the recorded output includes the driver's completion marker and the
/// driver's line offsets, so anything else would compare against a different
/// program.
fn driven(case: &Path) -> String {
    let template = std::fs::read_to_string(root().join("scripts/fuzz/drive.tcl"))
        .expect("read scripts/fuzz/drive.tcl");
    let body = std::fs::read_to_string(case).expect("read case");
    let mut out = String::with_capacity(template.len() + body.len());
    let mut spliced = false;
    for line in template.lines() {
        if !spliced && line == "#<<CASE>>" {
            out.push_str(&body);
            spliced = true;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    assert!(spliced, "scripts/fuzz/drive.tcl has no case marker");
    out
}

struct Run {
    status: i32,
    stdout: String,
    stderr: String,
}

/// How long a single case may take before it counts as a hang, in seconds.
/// The same default `scripts/fuzz_parity.sh` uses, so a case that times out
/// here is one that would time out there.
const TIMEOUT_SECS: u32 = 10;

/// Run one case, bounded the way the harness that recorded it bounds a run.
///
/// The corpus can hold a case that does not terminate: it holds one today,
/// `message-compile-time-693dbe3e.tcl`, whose record reads `verdict: CRITICAL
/// hang` and `tclrs-status: 142`. That 142 is SIGALRM — `scripts/fuzz/check_case.sh`
/// runs every engine through `perl -e 'alarm shift; exec @ARGV'`, so a run that
/// outlasts the timeout is a *classified* hang rather than a wedged harness.
///
/// Replaying that record with a plain `output()` cannot reproduce it and does
/// not try to: it waits forever, and `cargo test` never returns. So the replay
/// bounds each run by the same mechanism, which is what makes the recorded
/// status reproducible at all rather than merely un-hanging the suite.
fn run(binary: &Path, script: &Path) -> Run {
    let out = Command::new("perl")
        .arg("-e")
        .arg("alarm shift; exec @ARGV or die")
        .arg(TIMEOUT_SECS.to_string())
        .arg(binary)
        .arg(script)
        .output()
        .unwrap_or_else(|e| panic!("spawn {}: {e}", binary.display()));
    Run {
        // A signalled exit has no code of its own; `alarm` kills with SIGALRM,
        // and the recorded status for that is the shell's 128 + 14.
        status: out.status.code().unwrap_or(128 + libc::SIGALRM),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn cases() -> Vec<PathBuf> {
    let dir = root().join("tests/fuzz_corpus");
    if !dir.exists() {
        return Vec::new();
    }
    let mut v: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("read tests/fuzz_corpus")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("tcl"))
        .collect();
    v.sort();
    v
}

#[test]
fn every_committed_finding_still_behaves_as_recorded() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    let cases = cases();
    assert!(
        !cases.is_empty(),
        "tests/fuzz_corpus has no cases — run scripts/fuzz_parity.sh -m to fill it"
    );

    let dir = std::env::temp_dir().join(format!("tclrs-fuzz-corpus-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch directory");

    let mut failures = Vec::new();
    for case in &cases {
        let record = parse_record(
            &std::fs::read_to_string(case.with_extension("expected"))
                .unwrap_or_else(|e| panic!("read record for {}: {e}", case.display())),
        );
        let script = dir.join(case.file_name().expect("case file name"));
        std::fs::write(&script, driven(case)).expect("write driven script");

        let name = case.file_name().unwrap().to_string_lossy().into_owned();
        let reference = run(&tclsh, &script);
        let actual = run(Path::new(TCLRS), &script);

        let mut note = |what: &str, expected: &str, got: &str| {
            if expected != got {
                failures.push(format!(
                    "{name}: {what}\n  recorded: {expected:?}\n  now:      {got:?}"
                ));
            }
        };
        note("tclsh stdout", &record.tclsh_out, &reference.stdout);
        note(
            "tclsh status",
            &record.tclsh_status.to_string(),
            &reference.status.to_string(),
        );
        // The record holds what `head -1` wrote: the first line *with* its
        // terminator, or nothing at all when there was no error.
        let got_msg = match reference.stderr.lines().next() {
            Some(l) => format!("{l}\n"),
            None => String::new(),
        };
        note("tclsh message", &record.tclsh_msg, &got_msg);
        note("tclrs stdout", &record.tclrs_out, &actual.stdout);
        note(
            "tclrs status",
            &record.tclrs_status.to_string(),
            &actual.status.to_string(),
        );
        // The record keeps every stderr line, so compare every line. It kept
        // three once, which quietly stopped covering the `(file … line N)`
        // trailer as soon as a diagnostic grew to four.
        let got_err: String = actual.stderr.lines().map(|l| format!("{l}\n")).collect();

        note(
            "tclrs stderr",
            &normalize(&record.tclrs_err),
            &normalize(&got_err),
        );
    }

    assert!(
        failures.is_empty(),
        "{} of {} committed fuzz findings no longer match their record.\n\
         A tclsh mismatch means the record was made against another tclsh; a tclrs \
         mismatch means the behavior changed — re-record with `scripts/fuzz_parity.sh -R \
         tests/fuzz_corpus`, and move anything now fixed into tests/parity_fuzz_findings.rs.\n\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n\n")
    );
}

/// Every case must carry a record, and every record must name the seed that
/// produced it: a case nobody can regenerate is not reproducible.
#[test]
fn every_committed_finding_carries_its_provenance() {
    let mut problems = Vec::new();
    for case in cases() {
        let record = case.with_extension("expected");
        if !record.exists() {
            problems.push(format!("{}: no .expected record", case.display()));
            continue;
        }
        let text = std::fs::read_to_string(&record).expect("read record");
        for needle in ["# seed:", "# verdict:", "# tclsh:", "# tclrs:"] {
            if !text.contains(needle) {
                problems.push(format!("{}: record has no {needle} line", record.display()));
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
