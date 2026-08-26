//! The example programs, as a regression gate.
//!
//! Every `examples/*.tcl` is a self-checking script: it exercises one slice of
//! the language, compares each result with a `check` procedure and raises a Tcl
//! error — so a non-zero exit — the moment one drifts. Two tests run them:
//!
//! * `examples_self_tests_pass` runs each script under the built binary and
//!   requires a clean exit. It needs no Tcl installed, so it runs everywhere.
//! * `examples_match_tclsh` runs each script under both `tclsh` and tclrs and
//!   compares stdout byte for byte, which is what keeps the expectations in the
//!   scripts from being wrong in the same direction as the implementation. It
//!   reports a skip when no tclsh is on `PATH`, like the other differential
//!   tests here.
//!
//! The binary path comes from `CARGO_BIN_EXE_tclrs`, which Cargo sets for an
//! integration test, so the build exercised is always the current one.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Sorted list of `examples/*.tcl`.
fn examples() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let mut scripts: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {dir:?}: {e}"))
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "tcl"))
        .collect();
    scripts.sort();
    assert!(!scripts.is_empty(), "no example scripts in {dir:?}");
    scripts
}

/// The reference interpreter, if one is installed.
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

fn name_of(script: &Path) -> String {
    script.file_name().unwrap().to_string_lossy().to_string()
}

/// Every example runs to a clean exit under the current build.
#[test]
fn examples_self_tests_pass() {
    let tclrs = env!("CARGO_BIN_EXE_tclrs");

    let mut failures = Vec::new();
    for script in examples() {
        let out = Command::new(tclrs)
            .arg(&script)
            .output()
            .expect("run tclrs");
        if !out.status.success() {
            failures.push(format!(
                "{}: exited {:?}\n--- stdout ---\n{}--- stderr ---\n{}",
                name_of(&script),
                out.status.code(),
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr),
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{} example(s) failed their own checks:\n\n{}",
        failures.len(),
        failures.join("\n\n"),
    );
}

/// Examples whose reference run depends on a `lseq` fix not in every 9.0.x,
/// with what the divergence is. An entry names a REFERENCE bug, never a tclrs
/// one — the port follows the documented behaviour and the older interpreter
/// does not.
///
/// `lseq 3 to 9 by 0` is `3` per the Tcl 9.0 manual ("lseq produces a list
/// with count elements, always, even if the step value is 0"), and tclrs
/// answers that. tclsh 9.0.3 answers with the empty list — the lseq bug-fix
/// branch landed after it. So `examples/lists.tcl` fails under 9.0.3 by its
/// own `check`, which the harness would otherwise report as the example being
/// wrong.
const REFERENCE_LSEQ_ZERO_STEP: &[&str] = &["lists.tcl"];

/// Every example prints exactly what tclsh prints for it.
#[test]
fn examples_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    let tclrs = env!("CARGO_BIN_EXE_tclrs");

    let mut failures = Vec::new();
    for script in examples() {
        let reference = Command::new(&tclsh)
            .arg(&script)
            .output()
            .expect("run tclsh");
        let got = Command::new(tclrs)
            .arg(&script)
            .output()
            .expect("run tclrs");

        let name = name_of(&script);
        if !reference.status.success() {
            // An older 9.0.x fails `lists.tcl` on the documented zero-step
            // `lseq`; that is the reference's bug, not the example's.
            if REFERENCE_LSEQ_ZERO_STEP.contains(&name.as_str())
                && String::from_utf8_lossy(&reference.stderr).contains("lseq zero step")
            {
                continue;
            }
            failures.push(format!(
                "{name}: tclsh itself failed it, so the script's own expectations are wrong:\n{}",
                String::from_utf8_lossy(&reference.stderr).trim(),
            ));
            continue;
        }
        let want = String::from_utf8_lossy(&reference.stdout);
        let have = String::from_utf8_lossy(&got.stdout);
        if have != want {
            failures.push(format!(
                "{name}: stdout differs from tclsh\n  tclsh: {want:?}\n  tclrs: {have:?}\n  stderr: {}",
                String::from_utf8_lossy(&got.stderr).trim(),
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{} example(s) diverge from tclsh:\n\n{}",
        failures.len(),
        failures.join("\n\n"),
    );
}
