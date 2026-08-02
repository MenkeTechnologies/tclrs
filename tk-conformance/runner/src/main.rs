//! Measure how much of the official Tk test suite tclrs passes while hosting
//! the real Tk.
//!
//! The shape is `conformance/runner`'s, and the four stages are the same:
//!
//! 1. **extract** — `tclsh extract.tcl` lifts every `test` invocation out of a
//!    suite file into a case record. Mechanical: no file and no case is chosen
//!    by hand, and there is deliberately no option to run a subset.
//! 2. **reference** — `tclsh reference.tcl` runs each case with the real Tk
//!    loaded and records the outcome.
//! 3. **candidate** — this binary, re-invoked as `worker`, loads libtk against
//!    tclrs's stub table, calls `Tk_Init`, and runs each case through the
//!    host's own evaluator.
//! 4. **report** — the two are compared and the verdicts aggregated.
//!
//! Stages 2 and 3 are supervised: a case that hangs or takes its process down
//! is killed, marked, and stepped over, so one pathological body cannot stall
//! or silently shrink the measurement.
//!
//! # What is different from the Tcl runner, and why
//!
//! **The candidate is a process, not a function call.** The Tcl harness runs a
//! case with `tclrs::eval_captured`, and a panicking case is caught and stepped
//! over in-process. Nothing can be caught here: a stub slot with no body calls
//! `std::process::abort()` (`src/tk/trace.rs:101-123`), on the argument that
//! answering a plausible zero from a slot whose contract is "a live `Tcl_Obj *`"
//! turns a precise diagnosis into a crash several frames later. So the
//! candidate has to be its own process, and the supervisor's restart-past-it
//! path is the normal case rather than the exception.
//!
//! **One `Tk_Init` per process.** Every restart pays for it — the main window,
//! `NSApplication`, the connection to the window server. That is what makes
//! this run take the time it takes, and it is not avoidable while a trap ends
//! the process.
//!
//! **The trap is recorded.** Before each case the worker writes `tkcase <n>` to
//! stderr, and the trace log writes `tktrap … <slot name>` when a slot with no
//! body is called. Pairing the two says which slot stopped which case, which
//! turns "it crashed" into a ranked list of what to implement next.

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
    pub child_interps: usize,
    pub rows: Vec<Row>,
}

/// One case, its verdict, and the two outcomes the verdict came from.
pub struct Row {
    pub case: Case,
    pub judgement: Judgement,
    pub reference: Option<Outcome>,
    pub candidate: Option<Outcome>,
    /// The stub slot that had no body when this case was being run, when the
    /// case is one that took its worker process down.
    pub trap: Option<String>,
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
    if args.get(1).map(String::as_str) == Some("demo") {
        let start = args[5].parse().unwrap_or(0);
        if let Err(e) = demo_worker(
            Path::new(&args[2]),
            Path::new(&args[3]),
            Path::new(&args[4]),
            start,
        ) {
            eprintln!("demo: {e}");
            std::process::exit(2);
        }
        return;
    }
    if let Err(e) = drive(config(&args)) {
        eprintln!("tk-conformance: {e}");
        std::process::exit(1);
    }
}

fn config(args: &[String]) -> Config {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the runner lives inside tk-conformance/")
        .to_path_buf();
    let mut cfg = Config {
        suite: root.join("vendor/tk9.0.4/tests"),
        work: root.join("work"),
        scripts: root.clone(),
        out: root.join("REPORT.md"),
        tclsh: std::env::var("TCLSH").unwrap_or_else(|_| "tclsh".to_string()),
        // Lower than the Tcl runner's default. Every candidate process opens a
        // connection to the window server and every reference process opens a
        // toplevel per case, so the ceiling here is the display, not the CPU.
        jobs: std::thread::available_parallelism()
            .map_or(4, |n| n.get())
            .min(4),
        stall: Duration::from_secs(20),
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
            "--stall-secs" => cfg.stall = Duration::from_secs(value().parse().unwrap_or(20)),
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
            "no *.test files under {} — run tk-conformance/fetch-suite.sh first",
            cfg.suite.display()
        ));
    }
    for dir in ["cases", "reference", "candidate", "scratch", "trap"] {
        fs::create_dir_all(cfg.work.join(dir)).map_err(io(&cfg.work))?;
    }

    let patchlevel = tk_patchlevel(&cfg)?;
    eprintln!(
        "tk-conformance: {} files, Tk {patchlevel}, {} jobs",
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
    let demo = widget_demo(&cfg);
    let text = report::render(&report::Inputs {
        results: &results,
        demo: demo.as_ref(),
        tk_patchlevel: &patchlevel,
        suite: &cfg.suite,
        stall: cfg.stall,
        slots_with_bodies: tclrs::tk::host::implemented_at(tclrs::tk::host::Level::Hosting).len(),
        slots_total: tclrs::tk::abi::TCL_STUBS_SLOTS,
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
    let trap_path = cfg.work.join("trap").join(format!("{stem}.log"));
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
        quiet(cmd)
    });
    let candidates = supervise(total, &candidate_path, cfg.stall, |start| {
        let mut cmd = Command::new(std::env::current_exe().unwrap_or_else(|_| "runner".into()));
        cmd.arg("worker")
            .arg(&cases_path)
            .arg(&candidate_path)
            .arg(start.to_string());
        cmd.stdin(Stdio::null()).stdout(Stdio::null());
        // Kept, not discarded: this is where the trap that ended the process
        // names the slot it wanted.
        match fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&trap_path)
        {
            Ok(log) => cmd.stderr(Stdio::from(log)),
            Err(_) => cmd.stderr(Stdio::null()),
        };
        cmd
    });
    let traps = traps_by_case(&trap_path);

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
                trap: traps.get(&case.index).cloned(),
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

/// Which stub slot had no body when each case was being run.
///
/// The worker writes `tkcase <index>` before every case and
/// `src/tk/trace.rs` writes `tktrap <n> <table> <slot> <name>` from the slot
/// that has none, both to stderr and both unbuffered, so the last `tkcase`
/// before a `tktrap` is the case that asked for it.
fn traps_by_case(path: &Path) -> BTreeMap<usize, String> {
    let mut found = BTreeMap::new();
    let Ok(text) = fs::read_to_string(path) else {
        return found;
    };
    let mut current: Option<usize> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("tkcase ") {
            current = rest.trim().parse().ok();
        } else if line.starts_with("tktrap ") {
            if let (Some(index), Some(slot)) = (current, line.split_whitespace().nth(4)) {
                found.insert(index, slot.to_string());
            }
        }
    }
    found
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
        let Ok(mut child) = build(next).spawn() else {
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
                "=== {} {} [{}]{}\n--- program\n{}\n--- reference {}\n--- tclrs     {}\n",
                result.name,
                row.case.name,
                row.judgement.bucket,
                row.trap
                    .as_ref()
                    .map(|s| format!(" trap={s}"))
                    .unwrap_or_default(),
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

/// One process: load libtk, build the hosting stub table, call `Tk_Init`, and
/// run cases until one of them ends the process.
///
/// Runs on the process's main thread and never leaves it, because
/// `Tk_MacOSXSetupTkNotifier` panics anywhere else
/// (`tk9.0.4/macosx/tkMacOSXNotify.c:258-272`).
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

    // Everything a case prints — through tclrs's `puts`, or from Tk's own C —
    // goes to a file this process can read back, so the two sides' stdout can
    // be compared. Installed before Tk is loaded, since `Tk_Init` prints too.
    let capture = out_path.with_extension("stdout");
    let sink = fs::File::create(&capture).map_err(io(&capture))?;
    unsafe { libc::dup2(std::os::unix::io::AsRawFd::as_raw_fd(&sink), 1) };

    let lib = tclrs::tk::load::Libtk::open().map_err(|e| e.to_string())?;
    let interp = tclrs::tk::host::build_hosting();
    let interp_ptr = interp as *mut std::ffi::c_void;

    // Say what is true: this process was started to run a script. `TkpInit`
    // opens a console window when stdin is not a terminal *and* there is no
    // startup script (`tk9.0.4/macosx/tkMacOSXInit.c:583-598`), and the console
    // needs the channel subsystem, which this host has not got. Under a
    // supervisor stdin is always `/dev/null`, so without this every case would
    // be measuring `Tcl_CreateChannel` instead of itself.
    unsafe { set_startup_script(interp_ptr, cases_path) };

    // `TkpInit` then takes the other branch, and that one points stdout *and*
    // stderr at `/dev/null` (`tk9.0.4/macosx/tkMacOSXInit.c:607-618`) — it is
    // guarding against a `puts` that blocks forever when the process was
    // launched as a macOS application. Both of this harness's channels go
    // through those descriptors: the capture the two sides' stdout is compared
    // on, and the log the trap attribution is read from. So they are duplicated
    // here and put back afterwards. Nothing about the measurement changes; the
    // harness only keeps the descriptors it opened.
    let (saved_out, saved_err) = unsafe { (libc::dup(1), libc::dup(2)) };
    let init = unsafe { tclrs::tk::load::call_tk_init(&lib, interp) };
    unsafe {
        libc::dup2(saved_out, 1);
        libc::dup2(saved_err, 2);
        libc::close(saved_out);
        libc::close(saved_err);
    }
    eprintln!("tkinit {init:?}");

    for case in cases.cases.iter().filter(|c| c.index >= start) {
        // The marker the report pairs with a `tktrap` line. Unbuffered, so it
        // is on disk before the case that may abort the process runs.
        eprintln!("tkcase {}", case.index);
        let program = String::from_utf8_lossy(&case.program()).into_owned();
        truncate_capture();

        let code = unsafe { tclrs::tk::eval::eval_script(interp_ptr, &program) };
        let result = unsafe { tclrs::tk::host::result_bytes(interp_ptr) };
        let printed = read_capture(&capture);

        let outcome = Outcome {
            status: if code == tclrs::tk::abi::TCL_OK {
                "ok".to_string()
            } else {
                "err".to_string()
            },
            result,
            stdout: printed,
        };
        writeln!(file, "{}", format_outcome(case.index, &outcome)).map_err(io(out_path))?;
        file.flush().map_err(io(out_path))?;
    }
    Ok(())
}

/// Empty the capture file and put file descriptor 1 back at its start.
///
/// `set_len` alone would leave the descriptor's offset where it was and the
/// next write would open a hole of NUL bytes in front of it.
fn truncate_capture() {
    unsafe {
        libc::ftruncate(1, 0);
        libc::lseek(1, 0, libc::SEEK_SET);
    }
}

fn read_capture(path: &Path) -> Vec<u8> {
    let _ = std::io::stdout().flush();
    fs::read(path).unwrap_or_default()
}

/// `Tcl_SetStartupScript(Tcl_NewStringObj(path, -1), NULL)` through the stub
/// table, which is where the two bodies live.
///
/// # Safety
/// `interp` is the `Tcl_Interp *` this process built.
unsafe fn set_startup_script(interp: *mut std::ffi::c_void, path: &Path) {
    use std::ffi::{c_char, c_void};
    type SetStartupScript = unsafe extern "C" fn(*mut tclrs::tk::abi::TclObj, *const c_char);
    let table = &*(*(interp as *mut tclrs::tk::host::HostInterp))
        .prefix
        .stub_table;
    let raw = table.slots[tclrs::tk::host::slot_index("tcl_SetStartupScript")];
    let f: SetStartupScript = std::mem::transmute::<_, SetStartupScript>(raw);
    let obj = tclrs::tk::obj::new_string(path.as_os_str().as_encoded_bytes());
    f(obj, std::ptr::null());
    let _ = std::ptr::null_mut::<c_void>();
}

fn io(path: &Path) -> impl Fn(std::io::Error) -> String + '_ {
    move |e| format!("{}: {e}", path.display())
}

fn tk_patchlevel(cfg: &Config) -> Result<String, String> {
    let script = cfg.work.join("patchlevel.tcl");
    fs::write(
        &script,
        "package require Tk\nputs [package require Tk]\nexit 0\n",
    )
    .map_err(io(&script))?;
    let out = Command::new(&cfg.tclsh)
        .arg(&script)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {}: {e}", cfg.tclsh))?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
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
}

impl Tally {
    pub fn attempted(&self) -> usize {
        self.passed + self.failed
    }

    pub fn add(&mut self, judgement: &Judgement) {
        self.extracted += 1;
        match judgement.verdict {
            Verdict::Pass => self.passed += 1,
            Verdict::Fail => self.failed += 1,
            Verdict::Skip => self.skipped += 1,
        }
    }

    pub fn merge(&mut self, other: &Tally) {
        self.extracted += other.extracted;
        self.skipped += other.skipped;
        self.passed += other.passed;
        self.failed += other.failed;
    }
}

pub fn percent(part: usize, whole: usize) -> String {
    if whole == 0 {
        return "—".to_string();
    }
    format!("{:.1}%", part as f64 * 100.0 / whole as f64)
}

// ── the shipped widget demonstration ─────────────────────────────────────────

/// How far Tk's own sample application gets under this host.
pub struct Demo {
    /// The demo script, as the reference interpreter's `tk_library` names it.
    pub path: PathBuf,
    pub lines: usize,
    /// One entry per statement, in order: the line it ends on, and what
    /// happened.
    pub statements: Vec<(usize, Outcome)>,
}

impl Demo {
    /// How many statements ran without a refusal before the first that did
    /// not.
    pub fn ran(&self) -> usize {
        self.statements
            .iter()
            .position(|(_, o)| o.status != "ok")
            .unwrap_or(self.statements.len())
    }

    /// The first statement that did not run, if there is one.
    pub fn stopped_at(&self) -> Option<&(usize, Outcome)> {
        self.statements.iter().find(|(_, o)| o.status != "ok")
    }
}

/// Where the reference interpreter keeps `demos/widget`.
///
/// Asked of the reference rather than hardcoded, so the report describes the Tk
/// that was measured against and not a path that happened to be right on one
/// machine.
fn demo_path(cfg: &Config) -> Option<PathBuf> {
    let script = cfg.work.join("demopath.tcl");
    fs::write(
        &script,
        "package require Tk\nputs [file join $tk_library demos widget]\nexit 0\n",
    )
    .ok()?;
    let out = Command::new(&cfg.tclsh)
        .arg(&script)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    path.is_file().then_some(path)
}

/// Run the demo one statement at a time and record what each one did.
///
/// Every statement is attempted, including the ones after the first refusal.
/// "How far does it get" is the first refusal, and that is what the report
/// leads with; the rest is what makes the answer more than one bit — a demo
/// that stops on its first command because `package` is missing and one that
/// stops two hundred statements in are different situations, and the tail says
/// which of the two this is.
fn widget_demo(cfg: &Config) -> Option<Demo> {
    let path = demo_path(cfg)?;
    let text = fs::read_to_string(&path).ok()?;
    let lines = text.lines().count();

    let bounds_path = cfg.work.join("demo.bounds");
    let ok = Command::new(&cfg.tclsh)
        .arg(cfg.scripts.join("boundaries.tcl"))
        .arg(&path)
        .arg(&bounds_path)
        .stdin(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        return None;
    }
    let bounds = read_bounds(&bounds_path)?;

    let out_path = cfg.work.join("demo.out");
    let trap_path = cfg.work.join("trap").join("demo.log");
    let outcomes = supervise(bounds.len(), &out_path, cfg.stall, |start| {
        let mut cmd = Command::new(std::env::current_exe().unwrap_or_else(|_| "runner".into()));
        cmd.arg("demo")
            .arg(&path)
            .arg(&bounds_path)
            .arg(&out_path)
            .arg(start.to_string());
        cmd.stdin(Stdio::null()).stdout(Stdio::null());
        match fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&trap_path)
        {
            Ok(log) => cmd.stderr(Stdio::from(log)),
            Err(_) => cmd.stderr(Stdio::null()),
        };
        cmd
    });
    let traps = traps_by_case(&trap_path);

    let statements = bounds
        .iter()
        .enumerate()
        .map(|(i, (_, last))| {
            let outcome = outcomes
                .get(i)
                .and_then(Option::clone)
                .unwrap_or_else(|| Outcome::aborted("no outcome line"));
            // A statement that ended the process is reported by the slot it
            // wanted rather than by the supervisor's generic wording.
            let outcome = match traps.get(&i) {
                Some(slot) if outcome.is_abort() => {
                    Outcome::aborted(&format!("called the stub slot {slot}, which has no body"))
                }
                _ => outcome,
            };
            (*last, outcome)
        })
        .collect();
    eprintln!("wrote {}", out_path.display());
    Some(Demo {
        path,
        lines,
        statements,
    })
}

fn read_bounds(path: &Path) -> Option<Vec<(usize, usize)>> {
    let text = fs::read_to_string(path).ok()?;
    let bounds: Vec<(usize, usize)> = text
        .lines()
        .filter_map(|line| {
            let (a, b) = line.split_once(' ')?;
            Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
        })
        .collect();
    (!bounds.is_empty()).then_some(bounds)
}

/// The demo stage's own worker: one host, and the demo's statements run in
/// order against it, exactly as `wish` would run them.
fn demo_worker(
    script: &Path,
    bounds_path: &Path,
    out_path: &Path,
    start: usize,
) -> Result<(), String> {
    let text = fs::read_to_string(script).map_err(io(script))?;
    let lines: Vec<&str> = text.lines().collect();
    let bounds = read_bounds(bounds_path).ok_or("no statement boundaries")?;

    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(start > 0)
        .write(true)
        .truncate(start == 0)
        .open(out_path)
        .map_err(io(out_path))?;

    let capture = out_path.with_extension("stdout");
    let sink = fs::File::create(&capture).map_err(io(&capture))?;
    unsafe { libc::dup2(std::os::unix::io::AsRawFd::as_raw_fd(&sink), 1) };

    let lib = tclrs::tk::load::Libtk::open().map_err(|e| e.to_string())?;
    let interp = tclrs::tk::host::build_hosting();
    let interp_ptr = interp as *mut std::ffi::c_void;
    unsafe { set_startup_script(interp_ptr, script) };
    let (saved_out, saved_err) = unsafe { (libc::dup(1), libc::dup(2)) };
    let init = unsafe { tclrs::tk::load::call_tk_init(&lib, interp) };
    unsafe {
        libc::dup2(saved_out, 1);
        libc::dup2(saved_err, 2);
        libc::close(saved_out);
        libc::close(saved_err);
    }
    eprintln!("tkinit {init:?}");

    // A restart replays the statements before `start` without recording them,
    // so the state the next statement expects — the variables and widgets its
    // predecessors created — is the state it would have had in one run.
    for (i, (first, last)) in bounds.iter().enumerate() {
        let program = lines[first - 1..*last].join("\n");
        if i < start {
            unsafe { tclrs::tk::eval::eval_script(interp_ptr, &program) };
            continue;
        }
        eprintln!("tkcase {i}");
        truncate_capture();
        let code = unsafe { tclrs::tk::eval::eval_script(interp_ptr, &program) };
        let result = unsafe { tclrs::tk::host::result_bytes(interp_ptr) };
        let outcome = Outcome {
            status: if code == tclrs::tk::abi::TCL_OK {
                "ok".to_string()
            } else {
                "err".to_string()
            },
            result,
            stdout: read_capture(&capture),
        };
        writeln!(file, "{}", format_outcome(i, &outcome)).map_err(io(out_path))?;
        file.flush().map_err(io(out_path))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A child killed mid-write leaves a half line; the next writer must append
    /// after a line boundary, or the two halves merge into a line nothing can
    /// read and the case it belongs to never gets an outcome.
    #[test]
    fn a_half_written_line_is_cut_before_the_next_writer_appends() {
        let path = std::env::temp_dir().join("tclrs-tk-conformance-trim-test.out");
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

    /// The trap log is what turns "the process died" into "the process died
    /// wanting `Tcl_CreateChannel`", so the pairing rule is pinned.
    #[test]
    fn a_trap_is_attributed_to_the_case_that_was_running() {
        let path = std::env::temp_dir().join("tclrs-tk-conformance-trap-test.log");
        fs::write(
            &path,
            "tkinit Ok(1)\n\
             tkcase 0\n\
             tkslot 1 Tcl 96 tcl_CreateObjCommand\n\
             tkcase 1\n\
             tkslot 2 Tcl 56 tcl_NewStringObj\n\
             tktrap 3 Tcl 88 tcl_CreateChannel\n",
        )
        .expect("write");
        let traps = traps_by_case(&path);
        assert_eq!(traps.len(), 1, "one trap, attributed once");
        assert_eq!(traps.get(&1).map(String::as_str), Some("tcl_CreateChannel"));
        assert!(!traps.contains_key(&0), "the case before it finished");
        let _ = fs::remove_file(&path);
    }
}
