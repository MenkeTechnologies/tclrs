//! Turning a pair of outcomes into a verdict.
//!
//! A port of `conformance/runner/src/classify.rs`, with one rule changed and
//! the reason for the change stated here rather than buried.
//!
//! The rules are fixed and applied in one order, and the order is the point.
//! Agreement is checked *before* any excuse for tclrs is considered, so no
//! classification can turn a passing case into a skipped one. A case is only
//! set aside when it genuinely cannot be run:
//!
//! * tcltest's own constraint check says this configuration cannot run it;
//! * the reference could not produce an outcome at all;
//! * the reference run needs a command plain `tclsh` + Tk has not got — the
//!   internal commands of the `tk::test` package, or a proc an earlier test
//!   body would have defined — or a package that is not installed;
//! * tclrs refused the case with `invalid command name`, for a command it does
//!   not implement.
//!
//! **A stub-table trap is a failure, not a skip.** This is the rule that
//! differs from the Tcl harness, and it is the stricter of the two readings. Tk
//! reaches this host through 691 function pointers, and one with no body calls
//! `std::process::abort()` (`src/tk/trace.rs:101-123`). That is not tclrs
//! declining and saying so, the way `invalid command name` is; it is the
//! process dying. The Tcl harness already counts a crash as a failure, and a
//! trap is a crash. Treating the 540 slots without bodies as an excuse would
//! turn almost the whole suite into skips and make the pass rate a statement
//! about a handful of cases.
//!
//! The slots that stopped a run are counted separately, so what the number is
//! waiting on is visible rather than inferred.

use crate::record::{Case, Outcome};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Fail,
    Skip,
}

#[derive(Debug, Clone)]
pub struct Judgement {
    pub verdict: Verdict,
    /// The skip reason or the failure cause, one of a fixed set.
    pub bucket: &'static str,
    /// What distinguishes this case within its bucket, for the histograms.
    pub detail: String,
}

impl Judgement {
    fn of(verdict: Verdict, bucket: &'static str, detail: impl Into<String>) -> Judgement {
        Judgement {
            verdict,
            bucket,
            detail: detail.into(),
        }
    }
}

pub const SKIP_CONSTRAINT: &str = "tcltest constraint not met";
pub const SKIP_NO_REFERENCE: &str = "the reference produced no outcome";
pub const SKIP_NEEDS_COMMAND: &str = "needs a command the reference has not got";
pub const SKIP_NEEDS_PACKAGE: &str = "needs a package that is not installed";
pub const SKIP_TCLRS_COMMAND: &str = "tclrs has no such command";

pub const FAIL_ABORT: &str = "tclrs was killed or crashed";
pub const FAIL_ERROR_VS_OK: &str = "tclrs raised an error, the reference did not";
pub const FAIL_OK_VS_ERROR: &str = "the reference raised an error, tclrs did not";
pub const FAIL_MESSAGE: &str = "both raised an error, messages differ";
pub const FAIL_CODE: &str = "return codes differ";
pub const FAIL_RESULT: &str = "results differ";
pub const FAIL_STDOUT: &str = "stdout differs";
pub const FAIL_NO_OUTCOME: &str = "tclrs produced no outcome line";

/// `invalid command name "x"`, with or without tclrs's trailing line note.
pub fn invalid_command(message: &str) -> Option<&str> {
    let rest = message.strip_prefix("invalid command name \"")?;
    let (name, tail) = rest.rsplit_once('"')?;
    if tail.is_empty() || strip_line_note(tail).is_empty() {
        Some(name)
    } else {
        None
    }
}

/// tclrs appends ` (line N)` to a compile error. Remove it.
pub fn strip_line_note(message: &str) -> &str {
    let Some(head) = message.strip_suffix(')') else {
        return message;
    };
    let Some((before, digits)) = head.rsplit_once(" (line ") else {
        return message;
    };
    if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
        before
    } else {
        message
    }
}

/// Collapse the variable part of a message so that a histogram groups the
/// cases that share a cause.
pub fn normalize_message(message: &str) -> String {
    let message = strip_line_note(message);
    let mut out = String::with_capacity(message.len());
    let mut in_quotes = false;
    for c in message.chars() {
        if c == '"' {
            if !in_quotes {
                out.push_str("\"…\"");
            }
            in_quotes = !in_quotes;
        } else if !in_quotes {
            out.push(c);
        }
    }
    out
}

pub fn judge(case: &Case, reference: Option<&Outcome>, candidate: Option<&Outcome>) -> Judgement {
    if case.constraint_skipped {
        // The suite writes a constraint list as a braced word, a bare word, or
        // one spread over several lines, so the same constraint arrives spelt
        // several ways. Collapsing the whitespace makes the histogram below
        // count them once; it changes no verdict and no total.
        let names: Vec<&str> = case.constraints.split_whitespace().collect();
        let detail = if names.is_empty() {
            "unnamed".to_string()
        } else {
            names.join(" ")
        };
        return Judgement::of(Verdict::Skip, SKIP_CONSTRAINT, detail);
    }

    let Some(reference) = reference else {
        return Judgement::of(Verdict::Skip, SKIP_NO_REFERENCE, "no outcome line");
    };
    if reference.is_abort() {
        return Judgement::of(Verdict::Skip, SKIP_NO_REFERENCE, reference.result_text());
    }

    if reference.status == "err" {
        let message = reference.result_text();
        if let Some(name) = invalid_command(&message) {
            return Judgement::of(Verdict::Skip, SKIP_NEEDS_COMMAND, name);
        }
        if let Some(rest) = message.strip_prefix("can't find package ") {
            return Judgement::of(Verdict::Skip, SKIP_NEEDS_PACKAGE, rest);
        }
    }

    let Some(candidate) = candidate else {
        return Judgement::of(Verdict::Fail, FAIL_NO_OUTCOME, "");
    };

    // Agreement first, so that nothing below can reclassify a pass.
    if candidate == reference {
        return Judgement::of(Verdict::Pass, "", "");
    }

    if candidate.is_abort() {
        return Judgement::of(Verdict::Fail, FAIL_ABORT, candidate.result_text());
    }

    if candidate.status == "err" {
        let message = candidate.result_text();
        if let Some(name) = invalid_command(&message) {
            return Judgement::of(Verdict::Skip, SKIP_TCLRS_COMMAND, name);
        }
        if reference.status == "err" {
            let detail = if strip_line_note(&message) == reference.result_text() {
                "identical text apart from tclrs's trailing (line N)".to_string()
            } else {
                normalize_message(&message)
            };
            return Judgement::of(Verdict::Fail, FAIL_MESSAGE, detail);
        }
        return Judgement::of(Verdict::Fail, FAIL_ERROR_VS_OK, normalize_message(&message));
    }

    if reference.status == "err" {
        return Judgement::of(
            Verdict::Fail,
            FAIL_OK_VS_ERROR,
            normalize_message(&reference.result_text()),
        );
    }
    if candidate.status != reference.status {
        return Judgement::of(
            Verdict::Fail,
            FAIL_CODE,
            format!("reference {}, tclrs {}", reference.status, candidate.status),
        );
    }
    if candidate.result != reference.result {
        return Judgement::of(Verdict::Fail, FAIL_RESULT, "");
    }
    Judgement::of(Verdict::Fail, FAIL_STDOUT, "")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case() -> Case {
        Case {
            index: 0,
            name: "bell-1.1".to_string(),
            constraints: String::new(),
            constraint_skipped: false,
            state: Vec::new(),
            setup: Vec::new(),
            body: b"winfo exists .".to_vec(),
        }
    }

    fn outcome(status: &str, result: &str) -> Outcome {
        Outcome {
            status: status.to_string(),
            result: result.as_bytes().to_vec(),
            stdout: Vec::new(),
        }
    }

    #[test]
    fn agreement_is_reached_before_any_excuse_for_tclrs() {
        let ok = outcome("ok", "1");
        assert_eq!(judge(&case(), Some(&ok), Some(&ok)).verdict, Verdict::Pass);
        let boom = outcome("err", "bad window path name \"gorp\"");
        assert_eq!(
            judge(&case(), Some(&boom), Some(&boom)).verdict,
            Verdict::Pass
        );
    }

    /// The rule this harness does not share with the Tcl one. A trap in the
    /// stub table takes the process down, and a process that died measured
    /// nothing — counting it as a skip would quietly remove almost the whole
    /// suite from the denominator.
    #[test]
    fn a_stub_table_trap_is_a_failure_and_never_a_skip() {
        let reference = outcome("ok", "1");
        let trapped = Outcome::aborted("the stage process died on this case");
        let judged = judge(&case(), Some(&reference), Some(&trapped));
        assert_eq!(judged.verdict, Verdict::Fail);
        assert_eq!(judged.bucket, FAIL_ABORT);
    }

    #[test]
    fn a_reference_that_needs_a_missing_command_is_set_aside_even_when_tclrs_agrees() {
        // The reference is plain tclsh with Tk, so a body reaching for a
        // `tk::test` internal command errors there too, and both sides can
        // agree for the wrong reason. The rule drops the case rather than bank
        // a pass it did not earn.
        let same = outcome("err", "invalid command name \"testmetrics\"");
        let judged = judge(&case(), Some(&same), Some(&same));
        assert_eq!(judged.verdict, Verdict::Skip);
        assert_eq!(judged.bucket, SKIP_NEEDS_COMMAND);
        assert_eq!(judged.detail, "testmetrics");
    }

    /// The same constraint reaches this spelt several ways, and a histogram
    /// that counted `win` and `win\n` as two different reasons said so in the
    /// report.
    #[test]
    fn a_constraint_is_named_the_same_way_however_the_suite_spelt_it() {
        let mut spaced = case();
        spaced.constraint_skipped = true;
        spaced.constraints = "  unix   notAqua\n".to_string();
        assert_eq!(judge(&spaced, None, None).detail, "unix notAqua");
    }

    #[test]
    fn a_command_tclrs_refuses_by_name_is_a_skip() {
        let reference = outcome("ok", "1");
        let missing = outcome("err", "invalid command name \"regexp\" (line 3)");
        let judged = judge(&case(), Some(&reference), Some(&missing));
        assert_eq!(judged.verdict, Verdict::Skip);
        assert_eq!(judged.detail, "regexp");
    }

    #[test]
    fn stdout_is_compared_even_when_the_result_matches() {
        let reference = Outcome {
            status: "ok".into(),
            result: b"".to_vec(),
            stdout: b"Bell should ring now ...\n".to_vec(),
        };
        let candidate = Outcome {
            status: "ok".into(),
            result: b"".to_vec(),
            stdout: b"".to_vec(),
        };
        assert_eq!(
            judge(&case(), Some(&reference), Some(&candidate)).bucket,
            FAIL_STDOUT
        );
    }
}
