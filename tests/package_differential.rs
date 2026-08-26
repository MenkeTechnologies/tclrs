//! `package`, against the reference interpreter, one command at a time.
//!
//! Every expectation here is `tclsh`'s own answer: each case is run through
//! both interpreters and the completion code and the result string have to
//! match. Nothing is written by hand, so a wrong reading of `generic/tclPkg.c`
//! cannot be baked into the expectation as well as into the implementation.
//!
//! The cases are one script, not one script each, because `package` is stateful
//! — `provide` is what makes `require` answer, `ifneeded` is what makes it load
//! — and the interesting behaviour is in the sequence. The two interpreters run
//! the same script and their outputs are compared line for line, so a case that
//! diverges names itself.
//!
//! Two answers are deliberately outside the comparison, and both are
//! `tclsh`'s startup rather than `Tcl_PackageObjCmd`'s behaviour: `package
//! names` includes the packages `init.tcl` has already provided (`Tcl`, `tcl`,
//! `TclOO`, …), and `package unknown` is the handler `init.tcl` installs.
//! tclrs provides nothing about itself and installs no handler, so those two
//! are compared only for the packages this script itself creates.
//!
//! Skipped when no tclsh is installed.

use std::path::PathBuf;
use std::process::Command;

/// The cases, in order. Each is run as `catch {CASE} r` and reported as
/// `code | result`, which is the whole of what a Tcl command produces.
const CASES: &[&str] = &[
    // Argument checking and subcommand resolution.
    "package",
    "package bogus",
    "package requires foo",
    "package require",
    "package present",
    "package provide",
    "package ifneeded",
    "package ifneeded a",
    "package versions",
    "package vcompare 1",
    "package vsatisfies 1",
    "package names extra",
    "package unknown a b",
    "package require -exact foo",
    "package require -exact foo 1 2",
    // `Tcl_GetIndexFromObj` accepts a unique abbreviation
    // (`generic/tclIndexObj.c:242-296`).
    "package prov nothing-provided-under-this-name",
    // provide / require / present over one package.
    "package provide foo",
    "package provide foo 1.2",
    "package provide foo",
    // `9.0` and `9.0.0` are the same version, so this is not a conflict.
    "package provide foo 1.2.0",
    "package provide foo 1.3",
    "package require foo",
    "package require foo 1.2",
    "package require foo 2",
    "package require -exact foo 1.2",
    "package require -exact foo 1.3",
    "package present foo",
    "package present -exact foo 1.2",
    "package present -exact foo 9.9",
    "package versions foo",
    "package files foo",
    // A package nothing knows about.
    "package require nosuch",
    "package require nosuch 1.0",
    "package require nosuch 1.0 2.0",
    "package present nosuch",
    "package present nosuch 1.0",
    "package present -exact nosuch 1.0",
    "package present nosuch bogus",
    // Requirement syntax.
    "package require foo 1.0-2.0",
    "package require foo 2.0-",
    "package require foo 0.5-",
    "package require foo -",
    "package require foo 1.0-1.0",
    "package require foo 1-1-1",
    // The version arithmetic, which is what everything above rests on.
    "package vcompare 1.2 1.3",
    "package vcompare 1.3 1.2",
    "package vcompare 9.0 9.0.0",
    "package vcompare 010 10",
    "package vcompare 1.2 1.2.0.0",
    "package vcompare 1.2a3 1.2",
    "package vcompare 1.2b1 1.2",
    "package vcompare bogus 1",
    "package vsatisfies 1.2 1.0",
    "package vsatisfies 2.0 1.0",
    "package vsatisfies 1.2 1.0-2.0",
    "package vsatisfies 1.0 1.0-1.0",
    "package vsatisfies 1.2a1 1.2",
    "package vsatisfies 1.2 1.2a1",
    "package vsatisfies 1.2 bogus",
    // Everything from a `+` is ignored by the comparison but kept by the
    // string (`generic/tclPkg.c:1692`).
    "package provide plus 1.0+abc",
    "package provide plus",
    "package vcompare 1.0+abc 1.0",
    // `package prefer` only ever moves towards `latest`.
    "package prefer",
    "package prefer bogus",
    "package prefer latest",
    "package prefer stable",
    "package prefer",
    // `ifneeded`: register, read back, and load.
    "package ifneeded bar 1.0",
    "package ifneeded bar 1.0 {package provide bar 1.0}",
    "package ifneeded bar 1.0",
    "package versions bar",
    "package require bar",
    "package require bar",
    // A second registration for the same version replaces the script rather
    // than adding one (`generic/tclPkg.c:1193-1206`).
    "package ifneeded baz 1.0 {error first}",
    "package ifneeded baz 1.0 {package provide baz 1.0}",
    "package versions baz",
    "package require baz",
    // The three ways an `ifneeded` script can fail to deliver.
    "package ifneeded silent 1.0 {}",
    "package require silent",
    "package ifneeded wrong 1.0 {package provide wrong 2.0}",
    "package require wrong",
    "package ifneeded raises 1.0 {error boom}",
    "package require raises",
    // Circular dependency detection (`generic/tclPkg.c:663-671`).
    "package ifneeded circ 1.0 {package require circ}",
    "package require circ",
    // Version selection among several `ifneeded` scripts: newest stable wins
    // over a newer unstable one, because `package prefer` starts at `stable`.
    "package ifneeded many 1.0 {package provide many 1.0}",
    "package ifneeded many 2.0 {package provide many 2.0}",
    "package ifneeded many 2.1a1 {package provide many 2.1a1}",
    "package versions many",
    "package require many",
    // `forget` drops the version and every script.
    "package forget bar",
    "package versions bar",
    "package require bar",
    "package forget no-such-package-was-ever-mentioned",
    // A `package unknown` script gets the name and the requirements appended,
    // and runs before the package is declared missing.
    "package unknown {puts UNKNOWN-RAN; lappend ::seen}",
    "package require after-unknown 1.0 2.0-3.0",
    "package unknown {}",
    "package require after-unknown",
];

fn tclsh() -> Option<PathBuf> {
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
        // Only a 9.0 reference is an oracle for this port — see the same gate
        // in the sibling differential harnesses.
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

/// The script both interpreters run: every case, each reported as its
/// completion code and its result.
///
/// `catch` at the top level rather than inside a procedure, because that is
/// what both interpreters agree on today — `eval` inside a procedure is
/// refused by this frontend, so a shared helper proc could not be written.
fn script() -> String {
    let mut out = String::new();
    for case in CASES {
        out.push_str(&format!("set c [catch {{{case}}} r]; puts \"$c | $r\"\n"));
    }
    out
}

fn run(program: &std::path::Path, path: &std::path::Path) -> Vec<String> {
    let out = Command::new(program)
        .arg(path)
        .output()
        .unwrap_or_else(|e| panic!("run {}: {e}", program.display()));
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.stderr.is_empty(),
        "{} wrote to stderr: {}",
        program.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    text.lines().map(str::to_string).collect()
}

#[test]
fn every_case_answers_exactly_as_tclsh_answers() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on this machine");
        return;
    };
    let path = std::env::temp_dir().join(format!("tclrs-pkg-{}.tcl", std::process::id()));
    std::fs::write(&path, script()).expect("write the case script");

    let reference = run(&tclsh, &path);
    let ours = run(std::path::Path::new(env!("CARGO_BIN_EXE_tclrs")), &path);
    let _ = std::fs::remove_file(&path);

    // The `UNKNOWN-RAN` line the handler prints is interleaved with the
    // results, so the two runs are compared whole rather than case by case —
    // which also catches a handler that ran the wrong number of times.
    assert_eq!(
        reference.len(),
        ours.len(),
        "different number of output lines"
    );
    for (i, (want, got)) in reference.iter().zip(&ours).enumerate() {
        assert_eq!(
            want,
            got,
            "line {} disagrees\n  case: {}\n  tclsh: {want}\n  tclrs: {got}",
            i + 1,
            CASES
                .get(i)
                .copied()
                .unwrap_or("(the handler's own output)")
        );
    }
}

/// `package names` and `package unknown` cannot be compared whole — `init.tcl`
/// has already filled both in `tclsh` and tclrs has no `init.tcl` — so they are
/// compared over what the script itself created.
#[test]
fn names_and_unknown_report_what_this_script_put_there() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on this machine");
        return;
    };
    let src = "package provide a 1.0\n\
               package ifneeded b 2.0 {}\n\
               package unknown {my handler}\n\
               foreach p {a b} { puts \"[lsearch -exact [package names] $p] >= 0\" }\n\
               puts [package unknown]\n\
               package forget a\n\
               puts \"[lsearch -exact [package names] a] >= 0\"\n";
    let path = std::env::temp_dir().join(format!("tclrs-pkgn-{}.tcl", std::process::id()));
    std::fs::write(&path, src).expect("write the case script");
    let reference = run(&tclsh, &path);
    let ours = run(std::path::Path::new(env!("CARGO_BIN_EXE_tclrs")), &path);
    let _ = std::fs::remove_file(&path);

    // The index a package is found at differs — tclsh's table already holds
    // its own packages — so what is compared is whether it was found.
    let found = |lines: &[String]| -> Vec<bool> {
        lines
            .iter()
            .filter(|l| l.ends_with(">= 0"))
            .map(|l| l.split_whitespace().next().unwrap().parse::<i64>().unwrap() >= 0)
            .collect()
    };
    assert_eq!(found(&reference), found(&ours), "{reference:?} {ours:?}");
    assert_eq!(found(&ours), vec![true, true, false]);
    assert_eq!(
        reference.iter().find(|l| l.contains("my handler")),
        ours.iter().find(|l| l.contains("my handler")),
        "the handler a script sets is the handler it reads back"
    );
}

/// `package require Tk` outside a Tk session, which is two different truths
/// depending on how the binary was built — and neither of them is a panic.
///
/// A build with no `tk` feature has nothing to load Tk from, and says exactly
/// what `tclsh` says about a package it cannot find. A `--features tk` build
/// has the loader but cannot use it: Tk has to be initialised on the process
/// main thread (`tk9.0.4/macosx/tkMacOSXNotify.c:258-272`) and an ordinary run
/// puts the interpreter on a thread of its own, so it names that rather than
/// claiming the toolkit is missing when it is sitting right there.
#[test]
fn requiring_tk_without_a_session_is_refused_and_says_which_refusal_it_is() {
    let out = Command::new(env!("CARGO_BIN_EXE_tclrs"))
        .args(["-c", "package require Tk"])
        .output()
        .expect("run tclrs");
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    let expected = match cfg!(feature = "tk") {
        true => "Tk can only be initialised in a session started with tclrs --tk",
        false => "can't find package Tk",
    };
    assert!(err.contains(expected), "expected {expected:?}, got {err:?}");
    assert!(!err.contains("panicked"), "{err}");
}
