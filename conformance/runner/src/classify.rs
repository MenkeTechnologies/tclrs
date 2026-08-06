//! Turning a pair of outcomes into a verdict.
//!
//! The rules are fixed and applied in one order, and the order is the point.
//! Agreement is checked *before* any excuse for tclrs is considered, so no
//! classification can turn a passing case into a skipped one. A case is only
//! set aside when it genuinely cannot be run:
//!
//! * tcltest's own constraint check says this configuration cannot run it;
//! * tclsh could not produce a reference outcome at all;
//! * the reference run needs a command plain tclsh has not got — the internal
//!   commands of the `tcl::test` package, or a proc an earlier test body would
//!   have defined — or a package that is not installed;
//! * tclrs has no such command.
//!
//! Everything else is attempted, and anything attempted either matches the
//! reference byte for byte or is a failure. In particular a *feature* tclrs
//! declines inside a command it does have — a missing math function, an `lsort`
//! option, an array element where a scalar is expected, an integer too wide for
//! `i64` — is a failure,
//! not a skip. Those are counted separately as well, so the report can also
//! state the rate that a looser rule would have produced.

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
    /// A failure whose cause is a feature tclrs documents as not built yet.
    pub declared_gap: bool,
}

impl Judgement {
    fn of(verdict: Verdict, bucket: &'static str, detail: impl Into<String>) -> Judgement {
        Judgement {
            verdict,
            bucket,
            detail: detail.into(),
            declared_gap: false,
        }
    }

    fn gap(mut self, declared: bool) -> Judgement {
        self.declared_gap = declared;
        self
    }
}

pub const SKIP_CONSTRAINT: &str = "tcltest constraint not met";
pub const SKIP_NO_REFERENCE: &str = "tclsh produced no reference outcome";
pub const SKIP_NEEDS_COMMAND: &str = "needs a command plain tclsh has not got";
pub const SKIP_NEEDS_PACKAGE: &str = "needs a package that is not installed";
pub const SKIP_TCLRS_COMMAND: &str = "tclrs has no such command";

pub const FAIL_ABORT: &str = "tclrs was killed or crashed";
pub const FAIL_ERROR_VS_OK: &str = "tclrs raised an error, tclsh did not";
pub const FAIL_OK_VS_ERROR: &str = "tclsh raised an error, tclrs did not";
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

/// Whether an error names something tclrs documents as not built yet, as
/// opposed to being wrong about something it claims to implement.
pub fn is_declared_gap(message: &str) -> bool {
    const MARKERS: &[&str] = &[
        "is not supported yet",
        "are not supported yet",
        "must be a literal in this phase",
        "does not take an array element yet",
        "integer value too large to represent",
    ];
    MARKERS.iter().any(|m| message.contains(m))
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
        let detail = if case.constraints.is_empty() {
            "unnamed".to_string()
        } else {
            case.constraints.clone()
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
        let gap = is_declared_gap(&message);
        if reference.status == "err" {
            let detail = if strip_line_note(&message) == reference.result_text() {
                "identical text apart from tclrs's trailing (line N)".to_string()
            } else {
                normalize_message(&message)
            };
            return Judgement::of(Verdict::Fail, FAIL_MESSAGE, detail).gap(gap);
        }
        return Judgement::of(Verdict::Fail, FAIL_ERROR_VS_OK, normalize_message(&message))
            .gap(gap);
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
            format!("tclsh {}, tclrs {}", reference.status, candidate.status),
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
            name: "t-1.1".to_string(),
            constraints: String::new(),
            constraint_skipped: false,
            state: Vec::new(),
            setup: Vec::new(),
            body: b"list 1".to_vec(),
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
        // Even an error both sides get right is a pass, so long as the
        // reference is one tclsh could produce on its own.
        let boom = outcome("err", "divide by zero");
        assert_eq!(
            judge(&case(), Some(&boom), Some(&boom)).verdict,
            Verdict::Pass
        );
    }

    #[test]
    fn a_reference_that_needs_a_missing_command_is_set_aside_even_when_tclrs_agrees() {
        // The reference interpreter is plain tclsh, so a body reaching for a
        // `tcl::test` internal command errors there too, and both sides can
        // agree for the wrong reason. The rule drops the case rather than bank
        // a pass it did not earn — it costs a pass, which is the safe
        // direction for the number to move.
        let same = outcome("err", "invalid command name \"testobj\"");
        let judged = judge(&case(), Some(&same), Some(&same));
        assert_eq!(judged.verdict, Verdict::Skip);
        assert_eq!(judged.bucket, SKIP_NEEDS_COMMAND);
        assert_eq!(judged.detail, "testobj");
    }

    #[test]
    fn a_missing_tclrs_command_is_a_skip_but_a_missing_feature_is_a_failure() {
        let reference = outcome("ok", "1");
        let missing = outcome("err", "invalid command name \"regexp\" (line 3)");
        let judged = judge(&case(), Some(&reference), Some(&missing));
        assert_eq!(judged.verdict, Verdict::Skip);
        assert_eq!(judged.detail, "regexp");

        let refused = outcome(
            "err",
            "array startsearch is not supported yet (line 1)",
        );
        let judged = judge(&case(), Some(&reference), Some(&refused));
        assert_eq!(judged.verdict, Verdict::Fail);
        assert!(judged.declared_gap);
    }

    #[test]
    fn the_line_note_is_recognised_but_never_silently_forgiven() {
        let reference = outcome("err", "wrong # args: should be \"set varName ?newValue?\"");
        let candidate = outcome(
            "err",
            "wrong # args: should be \"set varName ?newValue?\" (line 2)",
        );
        let judged = judge(&case(), Some(&reference), Some(&candidate));
        assert_eq!(judged.verdict, Verdict::Fail);
        assert_eq!(judged.bucket, FAIL_MESSAGE);
        assert!(judged.detail.contains("trailing (line N)"));
    }

    #[test]
    fn stdout_is_compared_even_when_the_result_matches() {
        let reference = Outcome {
            status: "ok".into(),
            result: b"".to_vec(),
            stdout: b"hello\n".to_vec(),
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

    #[test]
    fn invalid_command_only_matches_the_whole_message() {
        assert_eq!(invalid_command("invalid command name \"foo\""), Some("foo"));
        assert_eq!(
            invalid_command("invalid command name \"foo\" (line 12)"),
            Some("foo")
        );
        // A message that merely mentions one is a different error.
        assert_eq!(
            invalid_command("invalid command name \"foo\" while executing bar"),
            None
        );
    }
}
