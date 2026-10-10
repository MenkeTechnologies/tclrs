//! Pinned-output regression cases that mean something without a reference
//! interpreter.
//!
//! The `*_differential.rs` harnesses compare tclrs against whatever `tclsh` 9.0.4
//! is on `PATH`, and CI installs none, so there they skip. A case here carries
//! the answer — stdout and exit status as the reference interpreter printed them
//! when the case was written — and is checked against tclrs unconditionally.
//! When a 9.0.x `tclsh` happens to be present the same pinned answer is also
//! checked against it, so a stale pin (a record made from a misread) is caught
//! locally instead of silently becoming the expectation.
//!
//! Only stdout and the exit status are pinned. A case that wants an error
//! message wraps the command in `catch` and prints the result.

use std::path::{Path, PathBuf};
use std::process::Command;

const TCLRS: &str = env!("CARGO_BIN_EXE_tclrs");

/// One pinned program.
pub struct Case {
    pub name: &'static str,
    pub script: &'static str,
    /// Everything the program writes to stdout.
    pub stdout: &'static str,
    /// The process exit status.
    pub status: i32,
}

/// A pinned program that exits 0.
pub const fn ok(name: &'static str, script: &'static str, stdout: &'static str) -> Case {
    Case {
        name,
        script,
        stdout,
        status: 0,
    }
}

struct Run {
    stdout: String,
    status: i32,
    stderr: String,
}

fn run(binary: &Path, script: &Path) -> Run {
    let out = Command::new(binary)
        .arg(script)
        .current_dir(script.parent().expect("script directory"))
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("spawn {}: {e}", binary.display()));
    Run {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        status: out.status.code().unwrap_or(-1),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// A `tclsh` of the 9.0 series, if one is on `PATH`. The patchlevel is not
/// pinned: the pinned answers are the ones the whole 9.0 series agrees on.
fn oracle() -> Option<PathBuf> {
    for name in ["tclsh9.0", "tclsh"] {
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
        let Ok(v) = Command::new("sh")
            .arg("-c")
            .arg(format!("printf 'puts [info patchlevel]\\n' | {path}"))
            .output()
        else {
            continue;
        };
        if String::from_utf8_lossy(&v.stdout)
            .trim()
            .starts_with("9.0.")
        {
            return Some(PathBuf::from(path));
        }
    }
    None
}

/// Run every case under tclrs and, when available, the reference interpreter,
/// and fail with every disagreement with the pin.
pub fn check(group: &str, cases: &[Case]) {
    let dir = std::env::temp_dir().join(format!("tclrs-golden-{group}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch directory");
    let reference = oracle();
    let mut failures = Vec::new();
    for case in cases {
        let file = dir.join(format!("{}.tcl", case.name));
        std::fs::write(&file, case.script).expect("write case");
        let mut engines: Vec<(&str, PathBuf)> = vec![("tclrs", PathBuf::from(TCLRS))];
        if let Some(r) = &reference {
            engines.push(("tclsh", r.clone()));
        }
        for (label, binary) in engines {
            let got = run(&binary, &file);
            if got.stdout != case.stdout || got.status != case.status {
                failures.push(format!(
                    "{}: {label}\n  script:   {:?}\n  expected: {:?} (status {})\n  got:      {:?} (status {})\n  stderr:   {:?}",
                    case.name, case.script, case.stdout, case.status, got.stdout, got.status, got.stderr
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} pinned cases disagree:\n\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n\n")
    );
}
