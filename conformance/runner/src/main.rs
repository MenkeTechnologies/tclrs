//! Measure how much of the official Tcl test suite tclrs passes.
//!
//! Four stages, each of which leaves its intermediate files on disk so a run
//! can be inspected, resumed, or audited afterwards:
//!
//! 1. **extract** — `tclsh extract.tcl` lifts every `test` invocation out of a
//!    suite file into a case record. Mechanical: no file and no case is chosen
//!    by hand, and there is deliberately no option to run a subset.
//! 2. **reference** — `tclsh reference.tcl` runs each case and records the
//!    outcome the reference interpreter produces.
//! 3. **candidate** — this binary, re-invoked as `worker`, runs each case
//!    through `tclrs::eval_captured` and records what tclrs produces.
//! 4. **report** — the two are compared and the verdicts aggregated.
//!
//! Stages 2 and 3 are supervised: a case that hangs or takes its process down
//! is killed, marked, and stepped over, so one pathological body cannot stall
//! or silently shrink the measurement.

mod b64;
mod classify;
mod record;
mod report;

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use classify::{judge, Judgement, Verdict};
use record::{format_outcome, parse_cases, parse_outcomes, Case, Extraction, Outcome};

struct Config {
    suite: PathBuf,
    work: PathBuf,
    scripts: PathBuf,
    out: PathBuf,
    tclsh: String,
    jobs: usize,
    stall: Duration,
    extract_timeout: Duration,
}

/// Everything one suite file contributed to the measurement.
pub struct FileResult {
    pub name: String,
    pub extraction: Extraction,
    /// Child interpreters the file created while being read — see
    /// [`record::Cases::child_interps`].
    pub child_interps: usize,
    pub rows: Vec<Row>,
}

/// One case, its verdict, and the two outcomes the verdict came from — kept so
/// that every number in the report can be traced back to a concrete pair.
pub struct Row {
    pub case: Case,
    pub judgement: Judgement,
    pub reference: Option<Outcome>,
    pub candidate: Option<Outcome>,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("worker") {
        let start = args[4].parse().unwrap_or(0);
        if let Err(e) = worker(Path::new(&args[2]), Path::new(&args[3]), start) {
            eprintln!("worker: {e}");
            std::process::exit(2);
        }
        return;
    }
    if let Err(e) = drive(config(&args)) {
        eprintln!("conformance: {e}");
        std::process::exit(1);
    }
}

fn config(args: &[String]) -> Config {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the runner lives inside conformance/")
        .to_path_buf();
    let mut cfg = Config {
        suite: root.join("vendor/tcl9.0.4/tests"),
        work: root.join("work"),
        scripts: root.clone(),
        out: root.join("REPORT.md"),
        tclsh: std::env::var("TCLSH").unwrap_or_else(|_| "tclsh".to_string()),
        jobs: std::thread::available_parallelism().map_or(4, |n| n.get()),
        stall: Duration::from_secs(15),
        extract_timeout: Duration::from_secs(300),
    };
    let mut it = args[1..].iter();
    while let Some(flag) = it.next() {
        let mut value = || it.next().cloned().unwrap_or_default();
        match flag.as_str() {
            "--suite" => cfg.suite = PathBuf::from(value()),
            "--work" => cfg.work = PathBuf::from(value()),
            "--out" => cfg.out = PathBuf::from(value()),
            "--tclsh" => cfg.tclsh = value(),
            "--jobs" => cfg.jobs = value().parse().unwrap_or(cfg.jobs),
            "--stall-secs" => cfg.stall = Duration::from_secs(value().parse().unwrap_or(15)),
            "--extract-timeout-secs" => {
                cfg.extract_timeout = Duration::from_secs(value().parse().unwrap_or(300));
            }
            other => {
                eprintln!("unknown option {other}");
                std::process::exit(64);
            }
        }
    }
    cfg
}

// ── the driver ───────────────────────────────────────────────────────────────

fn drive(cfg: Config) -> Result<(), String> {
    let files = suite_files(&cfg.suite)?;
    if files.is_empty() {
        return Err(format!(
            "no *.test files under {} — run conformance/fetch-suite.sh first",
            cfg.suite.display()
        ));
    }
    for dir in ["cases", "reference", "candidate", "scratch"] {
        fs::create_dir_all(cfg.work.join(dir)).map_err(io(&cfg.work))?;
    }

    let patchlevel = tclsh_patchlevel(&cfg)?;
    eprintln!(
        "conformance: {} files, tclsh {patchlevel}, {} jobs",
        files.len(),
        cfg.jobs
    );

    let queue = Mutex::new(files.clone());
    let done = Mutex::new(0usize);
    let results: Mutex<Vec<FileResult>> = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..cfg.jobs {
            scope.spawn(|| loop {
                let Some(file) = queue.lock().expect("queue").pop() else {
                    return;
                };
                let result = process(&cfg, &file);
                let mut n = done.lock().expect("counter");
                *n += 1;
                eprintln!(
                    "[{}/{}] {} — {} cases",
                    n,
                    files.len(),
                    result.name,
                    result.rows.len()
                );
                drop(n);
                results.lock().expect("results").push(result);
            });
        }
    });

    let mut results = results.into_inner().expect("results");
    results.sort_by(|a, b| a.name.cmp(&b.name));

    dump_failures(&cfg, &results)?;
    let coverage = command_coverage(&cfg)?;
    let text = report::render(&report::Inputs {
        results: &results,
        coverage: &coverage,
        tclsh_patchlevel: &patchlevel,
        suite: &cfg.suite,
        stall: cfg.stall,
    });
    fs::write(&cfg.out, text).map_err(io(&cfg.out))?;
    eprintln!("wrote {}", cfg.out.display());
    Ok(())
}

fn suite_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(io(dir))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension() == Some(OsStr::new("test")))
        .collect();
    files.sort();
    Ok(files)
}

fn process(cfg: &Config, test_file: &Path) -> FileResult {
    let name = test_file
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let stem = test_file.file_stem().unwrap_or_default().to_string_lossy();
    let cases_path = cfg.work.join("cases").join(format!("{stem}.cases"));
    let reference_path = cfg.work.join("reference").join(format!("{stem}.out"));
    let candidate_path = cfg.work.join("candidate").join(format!("{stem}.out"));
    let scratch = cfg.work.join("scratch").join(stem.as_ref());

    if !cases_path.exists() {
        extract(cfg, test_file, &cases_path, &scratch);
    }
    let parsed = fs::read_to_string(&cases_path)
        .map_err(|e| e.to_string())
        .and_then(|t| parse_cases(&t));
    let parsed = match parsed {
        Ok(parsed) => parsed,
        Err(e) => {
            return FileResult {
                name,
                extraction: Extraction::Partial(format!("unreadable case file: {e}")),
                child_interps: 0,
                rows: Vec::new(),
            }
        }
    };
    let total = parsed.cases.len();

    let references = supervise(total, &reference_path, cfg.stall, |start| {
        let mut cmd = Command::new(&cfg.tclsh);
        cmd.arg(cfg.scripts.join("reference.tcl"))
            .arg(&cases_path)
            .arg(&reference_path)
            .arg(start.to_string())
            .arg(&scratch);
        cmd
    });
    let candidates = supervise(total, &candidate_path, cfg.stall, |start| {
        let mut cmd = Command::new(std::env::current_exe().unwrap_or_else(|_| "runner".into()));
        cmd.arg("worker")
            .arg(&cases_path)
            .arg(&candidate_path)
            .arg(start.to_string());
        cmd
    });

    let rows = parsed
        .cases
        .into_iter()
        .map(|case| {
            let reference = references.get(case.index).and_then(Option::as_ref);
            let candidate = candidates.get(case.index).and_then(Option::as_ref);
            let judgement = judge(&case, reference, candidate);
            Row {
                judgement,
                reference: reference.cloned(),
                candidate: candidate.cloned(),
                case,
            }
        })
        .collect();
    FileResult {
        name,
        extraction: parsed.extraction,
        child_interps: parsed.child_interps,
        rows,
    }
}

fn extract(cfg: &Config, test_file: &Path, cases_path: &Path, scratch: &Path) {
    let mut cmd = Command::new(&cfg.tclsh);
    cmd.arg(cfg.scripts.join("extract.tcl"))
        .arg(&cfg.suite)
        .arg(test_file)
        .arg(cases_path)
        .arg(scratch);
    let Ok(mut child) = quiet(cmd).spawn() else {
        return;
    };
    let deadline = Instant::now() + cfg.extract_timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) => {}
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Run a stage until every case has an outcome.
///
/// The child appends one flushed line per case, so its progress is visible in
/// the file. When it stops without having written the case it was on — killed
/// for making no progress, or taken down by that case — the parent records the
/// abort against exactly that index and restarts the child after it. That is
/// the only way a case leaves this function without a real outcome, and it is
/// recorded as an abort rather than dropped.
fn supervise(
    total: usize,
    out_path: &Path,
    stall: Duration,
    mut build: impl FnMut(usize) -> Command,
) -> Vec<Option<Outcome>> {
    let mut have: Vec<Option<Outcome>> = vec![None; total];
    load_outcomes(out_path, &mut have);
    while let Some(next) = have.iter().position(Option::is_none) {
        trim_partial_line(out_path);
        let Ok(mut child) = quiet(build(next)).spawn() else {
            have[next] = Some(Outcome::aborted("could not start the stage process"));
            append_outcome(out_path, next, have[next].as_ref().expect("just set"));
            continue;
        };
        let killed = watch(&mut child, out_path, stall);
        trim_partial_line(out_path);
        load_outcomes(out_path, &mut have);
        if have[next].is_none() {
            let reason = if killed {
                format!("killed after {}s without progress", stall.as_secs())
            } else {
                "the stage process died on this case".to_string()
            };
            have[next] = Some(Outcome::aborted(&reason));
            append_outcome(out_path, next, have[next].as_ref().expect("just set"));
        }
    }
    have
}

/// Wait for a child, killing it when the outcome file stops growing. Returns
/// whether it had to be killed.
fn watch(child: &mut Child, out_path: &Path, stall: Duration) -> bool {
    let mut size = file_size(out_path);
    let mut last_progress = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return false,
            Ok(None) => {}
        }
        std::thread::sleep(Duration::from_millis(100));
        let now = file_size(out_path);
        if now != size {
            size = now;
            last_progress = Instant::now();
        } else if last_progress.elapsed() >= stall {
            let _ = child.kill();
            let _ = child.wait();
            return true;
        }
    }
}

fn file_size(path: &Path) -> u64 {
    fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Cut a half-written trailing line off an outcome file.
///
/// A child killed mid-write can leave one. The next writer appends, so without
/// this the two halves glue into a single unparseable line — and since the
/// parser stops at the first line it cannot read, the case that line belongs to
/// would never acquire an outcome and the supervisor would restart forever.
fn trim_partial_line(path: &Path) {
    let Ok(bytes) = fs::read(path) else {
        return;
    };
    let keep = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    if keep == bytes.len() {
        return;
    }
    if let Ok(file) = fs::OpenOptions::new().write(true).open(path) {
        let _ = file.set_len(keep as u64);
    }
}

fn load_outcomes(path: &Path, into: &mut [Option<Outcome>]) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    for (index, outcome) in parse_outcomes(&text) {
        if let Some(slot) = into.get_mut(index) {
            *slot = Some(outcome);
        }
    }
}

fn append_outcome(path: &Path, index: usize, outcome: &Outcome) {
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{}", format_outcome(index, outcome));
    }
}

/// Write every failing case out in full — its program, the reference outcome
/// and the tclrs outcome — so the report's numbers can be checked one by one
/// instead of taken on trust.
fn dump_failures(cfg: &Config, results: &[FileResult]) -> Result<(), String> {
    let path = cfg.work.join("failures.txt");
    let mut file = fs::File::create(&path).map_err(io(&path))?;
    for result in results {
        for row in result
            .rows
            .iter()
            .filter(|r| r.judgement.verdict == Verdict::Fail)
        {
            let show = |o: &Option<Outcome>| match o {
                Some(o) => format!(
                    "{} result={:?} stdout={:?}",
                    o.status,
                    String::from_utf8_lossy(&o.result),
                    String::from_utf8_lossy(&o.stdout)
                ),
                None => "<none>".to_string(),
            };
            writeln!(
                file,
                "=== {} {} [{}]\n--- program\n{}\n--- tclsh    {}\n--- tclrs    {}\n",
                result.name,
                row.case.name,
                row.judgement.bucket,
                String::from_utf8_lossy(&row.case.program()).trim_end(),
                show(&row.reference),
                show(&row.candidate),
            )
            .map_err(io(&path))?;
        }
    }
    eprintln!("wrote {}", path.display());
    Ok(())
}

fn quiet(mut cmd: Command) -> Command {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd
}

// ── the tclrs side ───────────────────────────────────────────────────────────

fn worker(cases_path: &Path, out_path: &Path, start: usize) -> Result<(), String> {
    let text = fs::read_to_string(cases_path).map_err(io(cases_path))?;
    let cases = parse_cases(&text)?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(start > 0)
        .write(true)
        .truncate(start == 0)
        .open(out_path)
        .map_err(io(out_path))?;

    // A panicking case is reported and stepped over; the default hook would
    // only add noise, since the payload is recorded as the outcome.
    std::panic::set_hook(Box::new(|_| {}));

    for case in cases.cases.iter().filter(|c| c.index >= start) {
        let program = String::from_utf8_lossy(&case.program()).into_owned();
        let outcome = match std::panic::catch_unwind(|| tclrs::eval_captured(&program)) {
            Ok((Ok(result), printed)) => Outcome {
                status: "ok".to_string(),
                result: result.into_bytes(),
                stdout: printed.into_bytes(),
            },
            Ok((Err(message), printed)) => Outcome {
                status: "err".to_string(),
                result: message.into_bytes(),
                stdout: printed.into_bytes(),
            },
            Err(payload) => Outcome::aborted(&format!("tclrs panicked: {}", panic_text(&payload))),
        };
        writeln!(file, "{}", format_outcome(case.index, &outcome)).map_err(io(out_path))?;
        file.flush().map_err(io(out_path))?;
    }
    Ok(())
}

fn panic_text(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        return (*s).to_string();
    }
    if let Some(s) = payload.downcast_ref::<String>() {
        return s.clone();
    }
    "unknown payload".to_string()
}

// ── command coverage ─────────────────────────────────────────────────────────

/// Which of the reference interpreter's own commands tclrs answers to.
///
/// A name tclrs does not know is refused with `invalid command name`, and that
/// is the whole test — the name is evaluated as a one-word script, which every
/// Tcl command either rejects for want of arguments or answers harmlessly.
pub struct Coverage {
    pub implemented: Vec<String>,
    pub missing: Vec<String>,
}

fn command_coverage(cfg: &Config) -> Result<Coverage, String> {
    let script = cfg.work.join("commands.tcl");
    fs::write(&script, "puts [join [lsort [info commands]] \\n]\n").map_err(io(&script))?;
    let out = Command::new(&cfg.tclsh)
        .arg(&script)
        .output()
        .map_err(io(&script))?;
    let listing = String::from_utf8_lossy(&out.stdout);

    let mut coverage = Coverage {
        implemented: Vec::new(),
        missing: Vec::new(),
    };
    for name in listing.lines().map(str::trim).filter(|n| !n.is_empty()) {
        let refused = match std::panic::catch_unwind(|| tclrs::eval(name)) {
            Ok(Err(message)) => classify::invalid_command(&message).is_some(),
            Ok(Ok(_)) => false,
            Err(_) => false,
        };
        if refused {
            coverage.missing.push(name.to_string());
        } else {
            coverage.implemented.push(name.to_string());
        }
    }
    Ok(coverage)
}

fn tclsh_patchlevel(cfg: &Config) -> Result<String, String> {
    let script = cfg.work.join("patchlevel.tcl");
    fs::write(&script, "puts [info patchlevel]\n").map_err(io(&script))?;
    let out = Command::new(&cfg.tclsh)
        .arg(&script)
        .output()
        .map_err(|e| format!("cannot run {}: {e}", cfg.tclsh))?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn io(path: &Path) -> impl Fn(std::io::Error) -> String + '_ {
    move |e| format!("{}: {e}", path.display())
}

// ── aggregation helpers shared with the report ───────────────────────────────

/// Count how often each detail occurs, most frequent first.
pub fn histogram<'a>(items: impl Iterator<Item = &'a str>) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for item in items {
        *counts.entry(item).or_default() += 1;
    }
    let mut ranked: Vec<(String, usize)> = counts
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    ranked
}

#[derive(Default, Clone, Copy)]
pub struct Tally {
    pub extracted: usize,
    pub skipped: usize,
    pub passed: usize,
    pub failed: usize,
    /// Failures whose cause is a feature tclrs documents as not built yet.
    pub declared_gaps: usize,
}

impl Tally {
    pub fn attempted(&self) -> usize {
        self.passed + self.failed
    }

    pub fn add(&mut self, judgement: &Judgement) {
        self.extracted += 1;
        match judgement.verdict {
            Verdict::Pass => self.passed += 1,
            Verdict::Fail => {
                self.failed += 1;
                self.declared_gaps += usize::from(judgement.declared_gap);
            }
            Verdict::Skip => self.skipped += 1,
        }
    }

    pub fn merge(&mut self, other: &Tally) {
        self.extracted += other.extracted;
        self.skipped += other.skipped;
        self.passed += other.passed;
        self.failed += other.failed;
        self.declared_gaps += other.declared_gaps;
    }
}

pub fn percent(part: usize, whole: usize) -> String {
    if whole == 0 {
        return "—".to_string();
    }
    format!("{:.1}%", part as f64 * 100.0 / whole as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A child killed mid-write leaves a half line; the next writer must append
    /// after a line boundary, or the two halves merge into a line nothing can
    /// read and the case it belongs to never gets an outcome.
    #[test]
    fn a_half_written_line_is_cut_before_the_next_writer_appends() {
        let path = std::env::temp_dir().join("tclrs-conformance-trim-test.out");
        let good = format_outcome(
            0,
            &Outcome {
                status: "ok".to_string(),
                result: b"a".to_vec(),
                stdout: Vec::new(),
            },
        );
        fs::write(&path, format!("{good}\n1\tok\tYQ")).expect("write");
        trim_partial_line(&path);
        append_outcome(&path, 1, &Outcome::aborted("killed"));

        let text = fs::read_to_string(&path).expect("read");
        let parsed = parse_outcomes(&text);
        assert_eq!(parsed.len(), 2, "both lines must survive the round trip");
        assert_eq!(parsed[1].0, 1);
        assert!(parsed[1].1.is_abort());
        let _ = fs::remove_file(&path);
    }

    /// A file that already ends cleanly must not lose its last line.
    #[test]
    fn a_complete_file_is_left_alone() {
        let path = std::env::temp_dir().join("tclrs-conformance-trim-intact.out");
        fs::write(&path, "0\tok\t\t\n").expect("write");
        trim_partial_line(&path);
        assert_eq!(fs::read_to_string(&path).expect("read"), "0\tok\t\t\n");
        let _ = fs::remove_file(&path);
    }
}
