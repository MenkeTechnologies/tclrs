//! Rendering the measurement as markdown.
//!
//! Every number here is counted from the verdicts of the run that produced it.
//! Nothing is rounded up, nothing is left out: the per-file table lists all
//! files, including the ones that contributed no cases, and the caveats
//! section names every file whose extraction stopped early.

use std::fmt::Write as _;
use std::path::Path;
use std::time::Duration;

use crate::classify::{self, Verdict};
use crate::record::Extraction;
use crate::{histogram, percent, Coverage, FileResult, Tally};

pub struct Inputs<'a> {
    pub results: &'a [FileResult],
    pub coverage: &'a Coverage,
    pub tclsh_patchlevel: &'a str,
    pub suite: &'a Path,
    /// How long a stage may go without producing an outcome before its process
    /// is killed and the case it was on is recorded as an abort.
    pub stall: Duration,
}

/// The suite's identity, not where this machine happens to keep it: the last
/// two path components, so the report is the same document wherever it was
/// generated.
fn suite_name(suite: &Path) -> String {
    let tail = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    match suite.parent() {
        Some(parent) if !tail(parent).is_empty() => format!("{}/{}", tail(parent), tail(suite)),
        _ => tail(suite),
    }
}

pub fn render(input: &Inputs) -> String {
    let mut out = String::new();
    let mut total = Tally::default();
    let mut per_file = Vec::new();
    for result in input.results {
        let mut tally = Tally::default();
        for row in &result.rows {
            tally.add(&row.judgement);
        }
        total.merge(&tally);
        per_file.push((result, tally));
    }

    header(&mut out, input, &total);
    method(&mut out);
    totals(&mut out, &total);
    skips(&mut out, input);
    failures(&mut out, input, &total);
    coverage(&mut out, input.coverage);
    per_file_table(&mut out, &per_file);
    caveats(&mut out, input, &per_file);
    reproduce(&mut out);
    out
}

fn header(out: &mut String, input: &Inputs, total: &Tally) {
    let _ = writeln!(
        out,
        "# tclrs conformance against the official Tcl test suite\n"
    );
    let _ = writeln!(
        out,
        "Reference interpreter: **tclsh {}**. Suite: `{}` — the `tests/` directory of the \
         matching Tcl source release, fetched and checksum-verified by \
         `conformance/fetch-suite.sh`.\n",
        input.tclsh_patchlevel,
        suite_name(input.suite)
    );
    let _ = writeln!(
        out,
        "**{} of {} attempted cases pass — {}.** Over every case the suite \
         contains, including the ones that cannot be run here, that is {} of {} — {}.\n",
        total.passed,
        total.attempted(),
        percent(total.passed, total.attempted()),
        total.passed,
        total.extracted,
        percent(total.passed, total.extracted)
    );
}

fn method(out: &mut String) {
    let _ = writeln!(out, "## How the number is produced\n");
    let _ = out.write_str(
        "The suite drives every test through the `tcltest` package. tclrs cannot load it — \
         tcltest is Tcl code built on namespaces, `proc`, `catch`, `regexp` and channel IO, \
         none of which this frontend has — so the cases are lifted out of the suite files \
         instead of being run in place.\n\n\
         The lifting is done by `conformance/extract.tcl`, running under tclsh with the real \
         tcltest loaded and only `::tcltest::test` replaced by a recorder. The recorder is a \
         port of tcltest's own argument parsing, so both the `-option value` form and the \
         historical `test name desc ?constraints? body result` form are read exactly as \
         tcltest reads them, and constraint state comes from tcltest's own evaluation rather \
         than a re-implementation of it. Every suite file is extracted; there is no option to \
         select a subset, and the runner has no way to run one.\n\n\
         Each extracted case becomes a standalone program — its `-setup` followed by its \
         `-body` — and is run twice: once by tclsh in a fresh child interpreter, once by \
         tclrs through `tclrs::eval_captured`. The outcome of a run is the triple (return \
         code, result string, everything written to stdout), and a case passes only when the \
         two triples are identical byte for byte. The suite's own `-result` and `-match` \
         values are not consulted: tclsh is the specification, and comparing against what it \
         actually does is stricter than comparing against what the suite says it should.\n\n\
         Verdicts are assigned in a fixed order, and agreement is checked before any excuse \
         for tclrs is considered, so no rule below can turn a pass into a skip. A case is set \
         aside only when it genuinely cannot be run:\n\n",
    );
    let _ = out.write_str(
        "| Skip reason | What it means |\n| --- | --- |\n\
         | tcltest constraint not met | tcltest's own constraint check says this build, \
         platform or configuration cannot run the case. |\n\
         | tclsh produced no reference outcome | the reference run hung and was killed, or \
         died on the case, so there is nothing to compare against. |\n\
         | needs a command plain tclsh has not got | the reference run failed with `invalid \
         command name`: the case needs the internal commands of the `tcl::test` package, or \
         a helper an earlier test body would have defined. Set aside even when tclrs happens \
         to report the same error, which costs passes rather than inventing them. |\n\
         | needs a package that is not installed | the reference run failed with `can't find \
         package`. |\n\
         | tclrs has no such command | tclrs refused with `invalid command name` for a \
         command it does not implement. |\n\n",
    );
    let _ = out.write_str(
        "Everything else is attempted, and anything attempted either matches or fails. A \
         *feature* tclrs declines inside a command it does have — `{*}` expansion, a missing \
         math function, an `lsort` option, an integer too wide for `i64` — counts as a \
         failure, not a skip. Those failures are also counted on their own below, so the \
         effect of the looser rule is visible rather than assumed.\n\n\
         Three things about the extraction are worth stating plainly. First, suite files set \
         variables at their top level and then write bodies that read them, so each case \
         carries the global variables its file had created by the time the test was declared, \
         replayed ahead of the body as `set` and `array set` commands. Only variables whose \
         name appears in the case's own text are carried — without that a file which builds a \
         large table at its top level would attach a copy of it to every one of its cases — \
         and both runs get exactly the same program, so whatever is left out is left out of \
         both. Second, procs are not replayed and bodies are not executed during extraction, \
         so a case that depends on a helper proc or on state an earlier body would have \
         produced fails under tclsh too, and is skipped as needing an unavailable command \
         rather than counted against tclrs. Third, `-cleanup` scripts are not run: they \
         execute after the value under test is produced and cannot change it.\n\n",
    );
}

fn totals(out: &mut String, total: &Tally) {
    let attempted = total.attempted();
    let lenient = attempted - total.declared_gaps;
    let _ = writeln!(out, "## Totals\n");
    let _ = writeln!(out, "| | Cases | Share |\n| --- | ---: | ---: |");
    let _ = writeln!(
        out,
        "| Extracted from the suite | {} | 100% |",
        total.extracted
    );
    let _ = writeln!(
        out,
        "| Skipped — cannot be run | {} | {} |",
        total.skipped,
        percent(total.skipped, total.extracted)
    );
    let _ = writeln!(
        out,
        "| Attempted | {attempted} | {} |",
        percent(attempted, total.extracted)
    );
    let _ = writeln!(
        out,
        "| ⤷ passed | {} | {} of attempted |",
        total.passed,
        percent(total.passed, attempted)
    );
    let _ = writeln!(
        out,
        "| ⤷ failed | {} | {} of attempted |\n",
        total.failed,
        percent(total.failed, attempted)
    );
    let _ = writeln!(
        out,
        "Of the {} failures, {} are a feature tclrs documents as not built yet rather than a \
         wrong answer. Counting those as skips instead would give {} of {} — {} — and that \
         looser number is stated here only so the choice of rule is visible. The headline \
         above uses the strict rule.\n",
        total.failed,
        total.declared_gaps,
        total.passed,
        lenient,
        percent(total.passed, lenient)
    );
}

fn skips(out: &mut String, input: &Inputs) {
    let _ = writeln!(out, "## Why cases were skipped\n");
    let judgements = || {
        input
            .results
            .iter()
            .flat_map(|r| r.rows.iter().map(|row| &row.judgement))
    };
    let buckets = histogram(
        judgements()
            .filter(|j| j.verdict == Verdict::Skip)
            .map(|j| j.bucket),
    );
    let _ = writeln!(out, "| Reason | Cases |\n| --- | ---: |");
    for (bucket, count) in &buckets {
        let _ = writeln!(out, "| {bucket} | {count} |");
    }
    let _ = out.write_str("\n");

    let missing = histogram(
        judgements()
            .filter(|j| j.bucket == classify::SKIP_TCLRS_COMMAND)
            .map(|j| j.detail.as_str()),
    );
    if !missing.is_empty() {
        let _ = writeln!(
            out,
            "### Commands tclrs does not have, by how many cases they block\n"
        );
        let _ = writeln!(
            out,
            "A case is attributed to the first command tclrs refused, so a body using several \
             missing commands is counted once, against the first of them.\n"
        );
        let _ = writeln!(out, "| Command | Cases |\n| --- | ---: |");
        for (name, count) in missing.iter().take(40) {
            let _ = writeln!(out, "| `{name}` | {count} |");
        }
        if missing.len() > 40 {
            let rest: usize = missing[40..].iter().map(|(_, c)| c).sum();
            let _ = writeln!(
                out,
                "| *{} further commands* | {rest} |",
                missing.len() - 40
            );
        }
        let _ = out.write_str("\n");
    }
}

fn failures(out: &mut String, input: &Inputs, total: &Tally) {
    let _ = writeln!(out, "## Why cases failed\n");
    let judgements = || {
        input
            .results
            .iter()
            .flat_map(|r| r.rows.iter().map(|row| &row.judgement))
            .filter(|j| j.verdict == Verdict::Fail)
    };
    let buckets = histogram(judgements().map(|j| j.bucket));
    let _ = writeln!(
        out,
        "| Cause | Cases | Share of failures | For example |\n| --- | ---: | ---: | --- |"
    );
    for (bucket, count) in &buckets {
        let examples: Vec<String> = input
            .results
            .iter()
            .flat_map(|r| r.rows.iter().map(move |row| (r.name.as_str(), row)))
            .filter(|(_, row)| row.judgement.bucket == bucket)
            .take(3)
            .map(|(file, row)| format!("`{file}` {}", row.case.name))
            .collect();
        let _ = writeln!(
            out,
            "| {bucket} | {count} | {} | {} |",
            percent(*count, total.failed),
            examples.join(", ")
        );
    }
    let _ = writeln!(
        out,
        "\nEvery failing case is written out in full — its program, the tclsh outcome and the \
         tclrs outcome — to `conformance/work/failures.txt` by the same run that produced this \
         table.\n"
    );

    let details = histogram(
        judgements()
            .filter(|j| !j.detail.is_empty())
            .map(|j| j.detail.as_str()),
    );
    if !details.is_empty() {
        let _ = writeln!(out, "### The most frequent failing messages\n");
        let _ = writeln!(
            out,
            "Error text with the quoted part elided and tclrs's trailing `(line N)` removed, \
             so that one cause groups into one row.\n"
        );
        let _ = writeln!(out, "| Message | Cases |\n| --- | ---: |");
        for (detail, count) in details.iter().take(30) {
            let _ = writeln!(out, "| {} | {count} |", escape(detail));
        }
        let _ = out.write_str("\n");
    }
}

fn coverage(out: &mut String, coverage: &Coverage) {
    let total = coverage.implemented.len() + coverage.missing.len();
    let _ = writeln!(out, "## Command coverage\n");
    let _ = writeln!(
        out,
        "Independently of the suite: of the {total} commands the reference interpreter defines \
         in the global namespace, tclrs answers to {} — {}. A name counts as answered when \
         tclrs does not refuse it with `invalid command name`.\n",
        coverage.implemented.len(),
        percent(coverage.implemented.len(), total)
    );
    let _ = writeln!(
        out,
        "Implemented: {}\n",
        coverage
            .implemented
            .iter()
            .map(|c| format!("`{c}`"))
            .collect::<Vec<_>>()
            .join(", ")
    );
}

fn per_file_table(out: &mut String, per_file: &[(&FileResult, Tally)]) {
    let _ = writeln!(out, "## Per file\n");
    let _ = writeln!(
        out,
        "| File | Extracted | Skipped | Attempted | Passed | Failed | Pass rate |\n\
         | --- | ---: | ---: | ---: | ---: | ---: | ---: |"
    );
    for (result, tally) in per_file {
        let _ = writeln!(
            out,
            "| `{}` | {} | {} | {} | {} | {} | {} |",
            result.name,
            tally.extracted,
            tally.skipped,
            tally.attempted(),
            tally.passed,
            tally.failed,
            percent(tally.passed, tally.attempted())
        );
    }
    let _ = out.write_str("\n");
}

fn caveats(out: &mut String, input: &Inputs, per_file: &[(&FileResult, Tally)]) {
    let _ = writeln!(out, "## What the run could not reach\n");

    let incomplete: Vec<_> = per_file
        .iter()
        .filter(|(r, _)| r.extraction != Extraction::Complete)
        .collect();
    if incomplete.is_empty() {
        let _ = writeln!(
            out,
            "Every suite file was extracted to the end: no file contributed a partial set of \
             cases.\n"
        );
    } else {
        let _ = writeln!(
            out,
            "These files stopped part way through extraction, so the cases after the stopping \
             point are not in the measurement at all. The count column is what was recorded \
             before the stop.\n"
        );
        let _ = writeln!(
            out,
            "| File | Cases recorded | Why extraction stopped |\n| --- | ---: | --- |"
        );
        for (result, tally) in &incomplete {
            let why = match &result.extraction {
                Extraction::Complete => unreachable!(),
                Extraction::Partial(message) => escape(message),
                Extraction::Killed => {
                    "killed: extraction exceeded its time limit or died".to_string()
                }
            };
            let _ = writeln!(out, "| `{}` | {} | {why} |", result.name, tally.extracted);
        }
        let _ = out.write_str("\n");
    }

    let hidden: Vec<_> = per_file
        .iter()
        .filter(|(r, _)| r.child_interps > 0)
        .collect();
    if !hidden.is_empty() {
        let _ = writeln!(
            out,
            "The recorder only sees `test` calls made in the interpreter it runs in. These files \
             created a child interpreter while being read, and any test they declare inside one \
             was not extracted — their case counts are a floor, not a total.\n"
        );
        let _ = writeln!(
            out,
            "| File | Child interpreters | Cases extracted |\n| --- | ---: | ---: |"
        );
        for (result, tally) in &hidden {
            let _ = writeln!(
                out,
                "| `{}` | {} | {} |",
                result.name, result.child_interps, tally.extracted
            );
        }
        let _ = out.write_str("\n");
    }

    let empty: Vec<&str> = per_file
        .iter()
        .filter(|(_, tally)| tally.extracted == 0)
        .map(|(result, _)| result.name.as_str())
        .collect();
    if !empty.is_empty() {
        let _ = writeln!(
            out,
            "{} files contributed no cases at all: {}. A file lands here when it is empty, when \
             everything in it sits behind a constraint this configuration does not meet, or when \
             it declares its tests inside a child interpreter.\n",
            empty.len(),
            empty
                .iter()
                .map(|n| format!("`{n}`"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let aborts = input
        .results
        .iter()
        .flat_map(|r| r.rows.iter().map(|row| &row.judgement))
        .filter(|j| j.bucket == classify::FAIL_ABORT)
        .count();
    let _ = writeln!(
        out,
        "A stage that goes {}s without producing an outcome is killed and the case it was on \
         is recorded as an abort, so that one pathological body cannot stall the run. Aborts \
         on the tclrs side count as failures rather than skips, and this run had {aborts} of \
         them; aborts on the reference side are the `tclsh produced no reference outcome` \
         skips above. That timeout is the only bound in the pipeline, and nothing is dropped \
         without landing in one of those two counts.\n",
        input.stall.as_secs()
    );
    let _ = writeln!(
        out,
        "Some suite cases depend on the clock, the file system, the environment or the \
         network, so a rerun can move the totals by a few cases. Nothing else in the pipeline \
         is nondeterministic: the case set, the ordering and the comparison are fixed.\n"
    );
}

fn reproduce(out: &mut String) {
    let _ = writeln!(out, "## Reproducing this report\n");
    let _ = out.write_str(
        "From a fresh checkout, with a `tclsh` on `PATH` and a stable Rust toolchain:\n\n\
         ```sh\n\
         conformance/run.sh\n\
         ```\n\n\
         That fetches the Tcl source release, verifies it against a pinned SHA-256, unpacks \
         its `tests/` directory, and runs all four stages, rewriting this file. The \
         intermediate artifacts are left under `conformance/work/` — one case file, one \
         reference outcome file and one tclrs outcome file per suite file — so any number \
         here can be traced back to the case that produced it.\n\n\
         A rerun reuses whatever is already in `conformance/work/`, which is what makes an \
         interrupted run cheap to resume. To force everything to be recomputed, remove that \
         directory first. Some suite bodies leave read-only directories behind in the \
         per-file scratch areas, so a plain `rm -rf` can refuse:\n\n\
         ```sh\n\
         find conformance/work -type d -exec chmod u+rwx {} +\n\
         rm -rf conformance/work\n\
         ```\n\n\
         `TCLSH` selects the reference interpreter and `--jobs N` sets how many suite files \
         are processed at once; neither changes the case set or the verdicts.\n",
    );
}

/// Keep a message from breaking the markdown table it sits in.
fn escape(text: &str) -> String {
    text.replace('|', "\\|").replace(['\n', '\r'], " ")
}
