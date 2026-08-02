//! Rendering the measurement as markdown.
//!
//! Every number here is counted from the verdicts of the run that produced it.
//! Nothing is rounded up, nothing is left out: the per-file table lists all
//! files, including the ones that contributed no cases, and the caveats section
//! names every file whose extraction stopped early.

use std::fmt::Write as _;
use std::path::Path;
use std::time::Duration;

use crate::classify::{self, Verdict};
use crate::record::Extraction;
use crate::{histogram, percent, FileResult, Tally};

pub struct Inputs<'a> {
    pub results: &'a [FileResult],
    /// How far Tk's own shipped sample application gets, when it could be run
    /// at all.
    pub demo: Option<&'a crate::Demo>,
    pub tk_patchlevel: &'a str,
    pub suite: &'a Path,
    /// How long a stage may go without producing an outcome before its process
    /// is killed and the case it was on is recorded as an abort.
    pub stall: Duration,
    pub slots_with_bodies: usize,
    pub slots_total: usize,
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
    method(&mut out, input);
    totals(&mut out, &total);
    skips(&mut out, input);
    failures(&mut out, input);
    traps(&mut out, input);
    widget_demo(&mut out, input);
    per_file_table(&mut out, &per_file);
    caveats(&mut out, input, &per_file);
    reproduce(&mut out);
    out
}

fn header(out: &mut String, input: &Inputs, total: &Tally) {
    let _ = writeln!(
        out,
        "# tclrs conformance against the official Tk test suite\n"
    );
    let _ = writeln!(
        out,
        "Reference: **tclsh {} with the real Tk {} loaded**. Suite: `{}` — the `tests/` \
         directory of the matching Tk source release, fetched and checksum-verified by \
         `tk-conformance/fetch-suite.sh`.\n",
        input.tk_patchlevel,
        input.tk_patchlevel,
        suite_name(input.suite)
    );
    let _ = writeln!(
        out,
        "The candidate is not a reimplementation of Tk. It is the same `libtcl9tk9.0.dylib` \
         the reference uses, loaded against tclrs's own Tcl stub table: {} of the {} \
         `TclStubs` slots have bodies, and Tk reaches this frontend through them.\n",
        input.slots_with_bodies, input.slots_total
    );
    let _ = writeln!(
        out,
        "**{} of {} attempted cases pass — {}.** Over every case the suite contains, \
         including the ones that cannot be run here, that is {} of {} — {}.\n",
        total.passed,
        total.attempted(),
        percent(total.passed, total.attempted()),
        total.passed,
        total.extracted,
        percent(total.passed, total.extracted)
    );
}

fn method(out: &mut String, input: &Inputs) {
    let _ = writeln!(out, "## How the number is produced\n");
    let _ = writeln!(
        out,
        "The suite drives every test through the `tcltest` package. tclrs cannot load it — \
         tcltest is Tcl code built on namespaces, `proc`, `catch`, `regexp` and channel IO, \
         none of which this frontend has — so the cases are lifted out of the suite files \
         instead of being run in place.\n"
    );
    let _ = writeln!(
        out,
        "The lifting is done by `tk-conformance/extract.tcl`, running under tclsh with the \
         real tcltest and the real Tk loaded and only `::tcltest::test` replaced by a \
         recorder. The recorder is a port of tcltest's own argument parsing, so both the \
         `-option value` form and the historical `test name desc ?constraints? body result` \
         form are read exactly as tcltest reads them, and constraint state comes from \
         tcltest's own evaluation rather than a re-implementation of it. Tk has to be \
         loaded for the extraction and not only for the run: `tests/constraints.tcl` calls \
         `tk windowingsystem`, `winfo`, `font` and `image` at a file's top level, so a \
         tclsh without Tk extracts nothing at all. Every suite file is extracted; there is \
         no option to select a subset, and the runner has no way to run one.\n"
    );
    let _ = writeln!(
        out,
        "Each extracted case becomes a standalone program — its `-setup` followed by its \
         `-body` — and is run twice. The reference run is a fresh child interpreter with Tk \
         loaded into it, so the case has a main window and the widget commands. The \
         candidate run is a separate process that opens `libtcl9tk9.0.dylib`, builds \
         tclrs's hosting stub table, calls `Tk_Init`, and evaluates the case through the \
         host's own evaluator. The outcome of a run is the triple (return code, result \
         string, everything written to stdout), and a case passes only when the two triples \
         are identical byte for byte. The suite's own `-result` and `-match` values are not \
         consulted: the reference is the specification, and comparing against what it \
         actually does is stricter than comparing against what the suite says it should.\n"
    );
    let _ = writeln!(
        out,
        "Verdicts are assigned in a fixed order, and agreement is checked before any excuse \
         for tclrs is considered, so no rule below can turn a pass into a skip. A case is \
         set aside only when it genuinely cannot be run:\n"
    );
    let _ = writeln!(out, "| Skip reason | What it means |");
    let _ = writeln!(out, "| --- | --- |");
    let _ = writeln!(
        out,
        "| {} | tcltest's own constraint check says this build, platform or configuration \
         cannot run the case. |",
        classify::SKIP_CONSTRAINT
    );
    let _ = writeln!(
        out,
        "| {} | the reference run hung and was killed, or died on the case, so there is \
         nothing to compare against. |",
        classify::SKIP_NO_REFERENCE
    );
    let _ = writeln!(
        out,
        "| {} | the reference run failed with `invalid command name`: the case needs the \
         internal commands of the `tk::test` package, or a helper an earlier test body \
         would have defined. Set aside even when tclrs happens to report the same error, \
         which costs passes rather than inventing them. |",
        classify::SKIP_NEEDS_COMMAND
    );
    let _ = writeln!(
        out,
        "| {} | the reference run failed with `can't find package`. |",
        classify::SKIP_NEEDS_PACKAGE
    );
    let _ = writeln!(
        out,
        "| {} | tclrs refused with `invalid command name` for a command it does not \
         implement. |\n",
        classify::SKIP_TCLRS_COMMAND
    );
    let _ = writeln!(
        out,
        "**A stub-table trap is a failure, not a skip.** This is the one rule that differs \
         from `conformance/`, and it is the stricter reading. Tk reaches this host through \
         {} function pointers; {} of them have no body, and calling one ends the process \
         (`src/tk/trace.rs:101-123`, which argues that answering a plausible zero from a \
         slot whose contract is a live `Tcl_Obj *` turns a precise diagnosis into a crash \
         several frames later). That is not tclrs declining and saying so, the way \
         `invalid command name` is — it is the process dying, and a process that died \
         measured nothing. Excusing it would move almost the whole suite into the skip \
         column and leave a pass rate computed over a handful of cases. The slots that \
         stopped a run are counted on their own below instead.\n",
        input.slots_total,
        input.slots_total - input.slots_with_bodies
    );
    let _ = writeln!(
        out,
        "Everything else is attempted, and anything attempted either matches or fails.\n"
    );
    let _ = writeln!(
        out,
        "Two things about the extraction are worth stating plainly, and both are inherited \
         from `conformance/extract.tcl` unchanged. First, suite files set variables at their \
         top level and then write bodies that read them, so each case carries the global \
         variables its file had created by the time the test was declared, replayed ahead \
         of the body as `set` and `array set` commands; only variables whose name appears \
         in the case's own text are carried, and both runs get exactly the same program. \
         Second, procs are not replayed and bodies are not executed during extraction, so a \
         case that depends on a helper proc or on state an earlier body would have produced \
         fails under the reference too, and is set aside as needing an unavailable command \
         rather than counted against tclrs. `-cleanup` scripts are not run: they execute \
         after the value under test is produced and cannot change it.\n"
    );
}

fn totals(out: &mut String, total: &Tally) {
    let _ = writeln!(out, "## Totals\n");
    let _ = writeln!(out, "| | Cases | Share |");
    let _ = writeln!(out, "| --- | ---: | ---: |");
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
        "| Attempted | {} | {} |",
        total.attempted(),
        percent(total.attempted(), total.extracted)
    );
    let _ = writeln!(
        out,
        "| ⤷ passed | {} | {} of attempted |",
        total.passed,
        percent(total.passed, total.attempted())
    );
    let _ = writeln!(
        out,
        "| ⤷ failed | {} | {} of attempted |\n",
        total.failed,
        percent(total.failed, total.attempted())
    );
}

fn skips(out: &mut String, input: &Inputs) {
    let _ = writeln!(out, "## Why cases were skipped\n");
    let buckets = histogram(
        input
            .results
            .iter()
            .flat_map(|r| &r.rows)
            .filter(|r| r.judgement.verdict == Verdict::Skip)
            .map(|r| r.judgement.bucket),
    );
    if buckets.is_empty() {
        let _ = writeln!(out, "No case was skipped.\n");
        return;
    }
    let _ = writeln!(out, "| Reason | Cases |");
    let _ = writeln!(out, "| --- | ---: |");
    for (reason, n) in &buckets {
        let _ = writeln!(out, "| {reason} | {n} |");
    }
    let _ = writeln!(out);

    for (bucket, title) in [
        (
            classify::SKIP_CONSTRAINT,
            "Constraints that set cases aside",
        ),
        (
            classify::SKIP_TCLRS_COMMAND,
            "Commands tclrs does not have, by how many cases they block",
        ),
        (
            classify::SKIP_NEEDS_COMMAND,
            "Commands the reference has not got either",
        ),
    ] {
        let ranked = histogram(
            input
                .results
                .iter()
                .flat_map(|r| &r.rows)
                .filter(|r| r.judgement.verdict == Verdict::Skip && r.judgement.bucket == bucket)
                .map(|r| r.judgement.detail.as_str()),
        );
        if ranked.is_empty() {
            continue;
        }
        let _ = writeln!(out, "### {title}\n");
        let _ = writeln!(out, "| Name | Cases |");
        let _ = writeln!(out, "| --- | ---: |");
        ranked_table(out, &ranked, 40);
        let _ = writeln!(out);
    }
}

fn failures(out: &mut String, input: &Inputs) {
    let _ = writeln!(out, "## Why cases failed\n");
    let buckets = histogram(
        input
            .results
            .iter()
            .flat_map(|r| &r.rows)
            .filter(|r| r.judgement.verdict == Verdict::Fail)
            .map(|r| r.judgement.bucket),
    );
    if buckets.is_empty() {
        let _ = writeln!(out, "No case failed.\n");
        return;
    }
    let _ = writeln!(out, "| Cause | Cases | For example |");
    let _ = writeln!(out, "| --- | ---: | --- |");
    for (bucket, n) in &buckets {
        let example = input
            .results
            .iter()
            .flat_map(|r| &r.rows)
            .find(|r| r.judgement.verdict == Verdict::Fail && r.judgement.bucket == bucket)
            .map(|r| r.judgement.detail.clone())
            .unwrap_or_default();
        let _ = writeln!(out, "| {bucket} | {n} | {} |", cell(&example));
    }
    let _ = writeln!(out);
}

fn traps(out: &mut String, input: &Inputs) {
    let ranked = histogram(
        input
            .results
            .iter()
            .flat_map(|r| &r.rows)
            .filter_map(|r| r.trap.as_deref()),
    );
    let _ = writeln!(out, "## Which stub slot stopped the run\n");
    if ranked.is_empty() {
        let _ = writeln!(
            out,
            "No case ended its worker process on a slot with no body.\n"
        );
        return;
    }
    let attributed: usize = ranked.iter().map(|(_, n)| n).sum();
    let _ = writeln!(
        out,
        "{attributed} cases took their worker process down by calling a `TclStubs` slot \
         that has no body. Each is attributed to the slot named on the `tktrap` line that \
         followed its `tkcase` marker, so this is a ranked list of what to implement next \
         rather than an estimate.\n"
    );
    let _ = writeln!(out, "| Slot | Cases |");
    let _ = writeln!(out, "| --- | ---: |");
    ranked_table(out, &ranked, 40);
    let _ = writeln!(out);
}

fn ranked_table(out: &mut String, ranked: &[(String, usize)], limit: usize) {
    for (name, n) in ranked.iter().take(limit) {
        let _ = writeln!(out, "| `{}` | {n} |", cell(name));
    }
    if ranked.len() > limit {
        let rest: usize = ranked.iter().skip(limit).map(|(_, n)| n).sum();
        let _ = writeln!(out, "| *{} further* | {rest} |", ranked.len() - limit);
    }
}

fn widget_demo(out: &mut String, input: &Inputs) {
    let _ = writeln!(out, "## Tk's own widget demonstration\n");
    let Some(demo) = input.demo else {
        let _ = writeln!(
            out,
            "The reference Tk did not ship a `demos/widget`, so there was nothing to run.\n"
        );
        return;
    };
    let name = demo
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let _ = writeln!(
        out,
        "`demos/{name}` is the sample application `wish` ships with: {} lines, which \
         `info complete` divides into {} statements (`tk-conformance/boundaries.tcl`; runs of \
         blank lines and comments are not counted as statements). It is run here one \
         statement at a time against one host, in order, the way `wish` runs it — and every \
         statement is attempted, including the ones after the first refusal, so the answer \
         is more than one bit. A statement that ends the process is stepped over when the \
         run is restarted, so one fatal statement does not take its successors with it.\n",
        demo.lines,
        demo.statements.len()
    );
    let ran = demo.ran();
    match demo.stopped_at() {
        None => {
            let _ = writeln!(out, "**All {} statements ran.**\n", demo.statements.len());
        }
        Some((line, outcome)) => {
            let _ = writeln!(
                out,
                "**It gets {} of {} statements in, and stops at line {} of the file.** That \
                 statement {}:\n",
                ran,
                demo.statements.len(),
                line,
                if outcome.is_abort() {
                    "ended the process"
                } else {
                    "was refused"
                }
            );
            let _ = writeln!(out, "```text\n{}\n```\n", outcome.result_text().trim());
        }
    }
    let ok = demo
        .statements
        .iter()
        .filter(|(_, o)| o.status == "ok")
        .count();
    let _ = writeln!(
        out,
        "Attempted individually, {} of the {} statements complete and {} do not. The \
         refusals, ranked:\n",
        ok,
        demo.statements.len(),
        demo.statements.len() - ok
    );
    let refusals: Vec<String> = demo
        .statements
        .iter()
        .filter(|(_, o)| o.status != "ok")
        .map(|(_, o)| classify::normalize_message(&o.result_text()))
        .collect();
    let ranked = histogram(refusals.iter().map(String::as_str));
    let _ = writeln!(out, "| Refusal | Statements |");
    let _ = writeln!(out, "| --- | ---: |");
    ranked_table(out, &ranked, 20);
    let _ = writeln!(out);
}

fn per_file_table(out: &mut String, per_file: &[(&FileResult, Tally)]) {
    let _ = writeln!(out, "## By suite file\n");
    let _ = writeln!(
        out,
        "| File | Extracted | Skipped | Attempted | Passed | Rate |"
    );
    let _ = writeln!(out, "| --- | ---: | ---: | ---: | ---: | ---: |");
    for (result, tally) in per_file {
        let _ = writeln!(
            out,
            "| `{}` | {} | {} | {} | {} | {} |",
            result.name,
            tally.extracted,
            tally.skipped,
            tally.attempted(),
            tally.passed,
            percent(tally.passed, tally.attempted())
        );
    }
    let _ = writeln!(out);
}

fn caveats(out: &mut String, input: &Inputs, per_file: &[(&FileResult, Tally)]) {
    let _ = writeln!(out, "## What this measurement does not cover\n");
    let partial: Vec<&FileResult> = per_file
        .iter()
        .map(|(r, _)| *r)
        .filter(|r| !matches!(r.extraction, Extraction::Complete))
        .collect();
    if partial.is_empty() {
        let _ = writeln!(
            out,
            "Every suite file was read to its end, so no file's case count is a floor.\n"
        );
    } else {
        let _ = writeln!(
            out,
            "These files stopped being read before their end, so their case counts are a \
             floor rather than a total:\n"
        );
        let _ = writeln!(out, "| File | Cases recorded | Why it stopped |");
        let _ = writeln!(out, "| --- | ---: | --- |");
        for result in partial {
            let why = match &result.extraction {
                Extraction::Complete => String::new(),
                Extraction::Partial(msg) => msg.clone(),
                Extraction::Killed => "the extraction was killed before it finished".to_string(),
            };
            let _ = writeln!(
                out,
                "| `{}` | {} | {} |",
                result.name,
                result.rows.len(),
                cell(&why)
            );
        }
        let _ = writeln!(out);
    }

    let children: Vec<&FileResult> = per_file
        .iter()
        .map(|(r, _)| *r)
        .filter(|r| r.child_interps > 0)
        .collect();
    if !children.is_empty() {
        let _ = writeln!(
            out,
            "These files declare some of their tests inside a child interpreter, where the \
             recorder cannot see them, so their case counts are a floor too: {}.\n",
            children
                .iter()
                .map(|r| format!("`{}`", r.name))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let _ = writeln!(
        out,
        "A case whose stage process made no progress for {}s is killed and recorded as an \
         abort against the case it was on — a failure on the tclrs side, a set-aside on the \
         reference side. Nothing is dropped.\n",
        input.stall.as_secs()
    );
    let _ = writeln!(
        out,
        "Every failing case is written out in full — its program, the reference outcome and \
         the tclrs outcome — to `tk-conformance/work/failures.txt`, so any number above can \
         be checked one case at a time rather than taken on trust.\n"
    );
}

fn reproduce(out: &mut String) {
    let _ = writeln!(out, "## Reproducing this\n");
    let _ = writeln!(out, "```sh\ntk-conformance/run.sh\n```\n");
    let _ = writeln!(
        out,
        "From a fresh checkout, with a `tclsh` that can `package require Tk` on `PATH` and \
         a stable Rust toolchain, that is the whole reproduction: it fetches the suite, \
         verifies its checksum, extracts every case, runs both sides, and rewrites this \
         file. Intermediate artifacts land in `tk-conformance/work/` and are reused on a \
         rerun, so an interrupted run is cheap to resume; delete that directory to force \
         everything to be recomputed.\n"
    );
    let _ = writeln!(
        out,
        "The run needs a window server. Both sides open real windows — that is the point of \
         hosting the real Tk — so a headless machine measures nothing here.\n"
    );
}

/// Markdown table cells cannot contain a raw `|` or a newline.
fn cell(text: &str) -> String {
    text.replace('|', "\\|")
        .replace('\n', " ")
        .trim()
        .to_string()
}
