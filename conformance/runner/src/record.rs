//! The two line formats the stages exchange: extracted cases, and the outcome
//! of running one.

use std::fmt::Write as _;

use crate::b64;

/// One test case lifted out of a suite file.
#[derive(Debug, Clone)]
pub struct Case {
    pub index: usize,
    pub name: String,
    /// The `-constraints` word, verbatim, for the skip report.
    pub constraints: String,
    /// tcltest's own verdict on whether this configuration can run the case.
    pub constraint_skipped: bool,
    /// The global variables the file had created by the time this test was
    /// declared, as `set` and `array set` commands.
    pub state: Vec<u8>,
    pub setup: Vec<u8>,
    pub body: Vec<u8>,
}

impl Case {
    /// The standalone program for this case: the ambient state its file had
    /// built up, then its setup, then its body. `conformance/reference.tcl`
    /// assembles the same three fields in the same order — the two sides have
    /// to run character-identical programs for the comparison to mean anything.
    ///
    /// `cleanup` is left out: it runs after the value under test is produced
    /// and cannot change it.
    pub fn program(&self) -> Vec<u8> {
        let mut prog = self.state.clone();
        prog.push(b'\n');
        prog.extend_from_slice(&self.setup);
        prog.push(b'\n');
        prog.extend_from_slice(&self.body);
        prog
    }
}

/// How a file's extraction ended, from its trailing `# status` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Extraction {
    /// The whole file was read.
    Complete,
    /// Its top level raised an error partway through, or called `exit`.
    Partial(String),
    /// No status line: extraction was killed before it finished.
    Killed,
}

pub struct Cases {
    pub cases: Vec<Case>,
    pub extraction: Extraction,
    /// How many child interpreters the file created while it was being read.
    /// The recorder only sees `test` calls made in the interpreter it runs in,
    /// so a file with a child interpreter may declare tests the extraction
    /// never saw, and its case count is a floor rather than a total.
    pub child_interps: usize,
}

pub fn parse_cases(text: &str) -> Result<Cases, String> {
    let mut cases = Vec::new();
    let mut extraction = Extraction::Killed;
    let mut child_interps = 0;
    for line in text.lines() {
        if let Some(status) = line.strip_prefix("# status ") {
            let parts: Vec<&str> = status.split(' ').collect();
            let kind = parts.first().copied().unwrap_or("");
            let message = parts.get(1).copied().unwrap_or("");
            let message = String::from_utf8_lossy(&b64::decode(message)?).into_owned();
            child_interps = parts.get(3).and_then(|n| n.parse().ok()).unwrap_or(0);
            extraction = match kind {
                "ok" => Extraction::Complete,
                _ => Extraction::Partial(message),
            };
            continue;
        }
        if line.is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() != 7 {
            return Err(format!("case line has {} fields, want 7", f.len()));
        }
        cases.push(Case {
            index: f[0].parse().map_err(|_| format!("bad index {:?}", f[0]))?,
            name: String::from_utf8_lossy(&b64::decode(f[1])?).into_owned(),
            constraints: String::from_utf8_lossy(&b64::decode(f[2])?).into_owned(),
            constraint_skipped: f[3] == "1",
            state: b64::decode(f[4])?,
            setup: b64::decode(f[5])?,
            body: b64::decode(f[6])?,
        });
    }
    Ok(Cases {
        cases,
        extraction,
        child_interps,
    })
}

/// What running a case produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// `ok` for return code 0, `err` for 1, `code<N>` for the rest, and
    /// `abort` when the run had to be killed or died.
    pub status: String,
    pub result: Vec<u8>,
    pub stdout: Vec<u8>,
}

impl Outcome {
    pub fn aborted(reason: &str) -> Outcome {
        Outcome {
            status: "abort".to_string(),
            result: reason.as_bytes().to_vec(),
            stdout: Vec::new(),
        }
    }

    pub fn is_abort(&self) -> bool {
        self.status == "abort"
    }

    pub fn result_text(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.result)
    }
}

pub fn format_outcome(index: usize, outcome: &Outcome) -> String {
    let mut line = String::new();
    write!(
        line,
        "{index}\t{}\t{}\t{}",
        outcome.status,
        b64::encode(&outcome.result),
        b64::encode(&outcome.stdout)
    )
    .expect("writing to a String cannot fail");
    line
}

/// Read an outcome file, keeping only whole lines: a run killed mid-write can
/// leave a partial last line, and that case is re-run rather than guessed at.
pub fn parse_outcomes(text: &str) -> Vec<(usize, Outcome)> {
    let mut out = Vec::new();
    for line in text.split_inclusive('\n') {
        let Some(line) = line.strip_suffix('\n') else {
            break;
        };
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() != 4 {
            break;
        }
        let (Ok(index), Ok(result), Ok(stdout)) =
            (f[0].parse::<usize>(), b64::decode(f[2]), b64::decode(f[3]))
        else {
            break;
        };
        out.push((
            index,
            Outcome {
                status: f[1].to_string(),
                result,
                stdout,
            },
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_case_line_round_trips_through_the_parser() {
        let line = format!(
            "7\t{}\t{}\t1\t{}\t{}\t{}",
            b64::encode(b"append-1.1"),
            b64::encode(b"unix knownBug"),
            b64::encode(b"set d {a b}\n"),
            b64::encode(b"set x 1"),
            b64::encode(b"list\t[append x 2]\n")
        );
        let parsed = parse_cases(&format!("{line}\n# status ok  1\n")).expect("parses");
        assert_eq!(parsed.extraction, Extraction::Complete);
        let case = &parsed.cases[0];
        assert_eq!(case.index, 7);
        assert_eq!(case.name, "append-1.1");
        assert_eq!(case.constraints, "unix knownBug");
        assert!(case.constraint_skipped);
        assert_eq!(
            case.program(),
            b"set d {a b}\n\nset x 1\nlist\t[append x 2]\n"
        );
    }

    #[test]
    fn a_truncated_outcome_line_is_dropped_rather_than_guessed() {
        let good = format_outcome(
            0,
            &Outcome {
                status: "ok".into(),
                result: b"a".to_vec(),
                stdout: vec![],
            },
        );
        let text = format!("{good}\n1\tok\tYQ");
        let parsed = parse_outcomes(&text);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].0, 0);
        assert_eq!(parsed[0].1.result, b"a");
    }
}
