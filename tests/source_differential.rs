//! Differential execution of `source` and `tcl_findLibrary`.
//!
//! Both commands read the file system, so every program below is written against
//! a fixture tree this test builds and is given its absolute path — the `{DIR}`
//! placeholder — rather than against whatever happens to be installed. tclsh and
//! tclrs are handed the same tree and the same program, and their stdouts are
//! compared byte for byte, so the search order `tcl_findLibrary` walks and the
//! wording `source` fails with are checked against the reference implementation.
//!
//! `tcl_findLibrary` is reached the same way in both: it is a procedure of
//! Tcl's own library there and a command here, and `TCLRS_FIXTURE_LIBRARY` is
//! the environment variable the fixture package honours, so the first branch of
//! the canonical search — the one that exists so an end user can work around
//! everything else — is what both interpreters take.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Programs run in both interpreters. `{DIR}` is the fixture directory.
const PROGRAMS: &[&str] = &[
    // ── source: the value, and what the file leaves behind ──
    "puts [source {DIR}/simple.tcl]\nputs $fromfile",
    "source {DIR}/ns.tcl\nputs $::sourced::tag\nputs $::sourced::extra\nputs [namespace exists ::sourced]",
    // Sourcing the same file twice runs it twice.
    "set ::times 0\nsource {DIR}/counter.tcl\nsource {DIR}/counter.tcl\nputs $::times",
    // A file the caller's variables reach into, and back out of.
    "set into 3\nsource {DIR}/reads.tcl\nputs $out",
    // `-encoding utf-8` is the encoding a Tcl 9 script is read in anyway.
    "puts [source -encoding utf-8 {DIR}/simple.tcl]",
    // Non-ASCII text survives the round trip.
    "source {DIR}/unicode.tcl\nputs $::greek",

    // ── source: the failures ──
    "puts [catch {source /nonexistent/x.tcl} m]\nputs $m",
    "puts [catch {source {DIR}} m]\nputs $m",
    "puts [catch {source} m]\nputs $m\nputs [catch {source a b c} m2]\nputs $m2",
    // An error inside the file reaches the caller, and `catch` traps it.
    "puts [catch {source {DIR}/raises.tcl} m]\nputs $m",

    // ── tcl_findLibrary ──
    "tcl_findLibrary fixture 1.0 1.0.0 fixture.tcl TCLRS_FIXTURE_LIBRARY fixture_library\nputs $fixture_library\nputs $::fixture_loaded",
    // The hardwired-path branch: a library variable that is already set is the
    // only directory searched.
    "set other_library {DIR}/fixture1.0\ntcl_findLibrary other 9.9 9.9.9 fixture.tcl NOT_SET_ANYWHERE other_library\nputs $other_library\nputs $::fixture_loaded",
    // The failure message, down to the sentence that ends it.
    "puts [catch {tcl_findLibrary nope 1.0 1.0.0 nope.tcl NOPE_NOT_SET nope_library} m]\nputs [string match \"Can't find a usable nope.tcl*\" $m]\nputs [string match \"*This probably means that nope wasn't installed properly.*\" $m]\nputs [catch {set nope_library}]",
    // Wrong argument count.
    "puts [catch {tcl_findLibrary a b c} m]\nputs $m",

    // ── the shape `tkInit` has ──
    // `Tk_Init` evaluates a procedure whose body deletes the procedure and then
    // asks `tcl_findLibrary` for the package's script directory. Both halves
    // have to work together: the self-deletion has to take effect for the call
    // that follows, and the search has to source what it finds.
    "set ::fixture_version 1.0\nset ::fixture_patchLevel 1.0.0\nproc fixtureInit {} {\n    rename fixtureInit {}\n    tcl_findLibrary fixture $::fixture_version $::fixture_patchLevel fixture.tcl TCLRS_FIXTURE_LIBRARY fixture_library\n}\nfixtureInit\nputs $::fixture_library\nputs $::fixture_loaded\nputs [catch {fixtureInit} m]\nputs $m",
];

/// The files the programs read. Written fresh per run, so the test carries its
/// own world rather than depending on what is installed.
const FIXTURES: &[(&str, &str)] = &[
    ("simple.tcl", "set fromfile 1\nexpr {6*7}\n"),
    (
        "ns.tcl",
        "namespace eval ::sourced {\n    variable tag loaded\n}\nset ::sourced::extra 2\n",
    ),
    ("counter.tcl", "incr ::times\n"),
    ("reads.tcl", "set out [expr {$into * 100}]\n"),
    ("unicode.tcl", "set ::greek \u{3b1}\u{3b2}\u{3b3}\n"),
    ("raises.tcl", "error \"from the sourced file\"\n"),
    ("fixture1.0/fixture.tcl", "set ::fixture_loaded yes\n"),
];

fn tclsh() -> Option<PathBuf> {
    for name in ["tclsh", "tclsh9.0", "tclsh8.6"] {
        if let Ok(out) = Command::new("sh")
            .arg("-c")
            .arg(format!("command -v {name}"))
            .output()
        {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }
    }
    None
}

static SEQ: AtomicUsize = AtomicUsize::new(0);

/// Build the fixture tree and answer where it is.
fn fixtures() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tclrs-source-fixtures-{}", std::process::id()));
    for (name, body) in FIXTURES {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().expect("a fixture has a directory"))
            .expect("create fixture directory");
        std::fs::write(&path, body).expect("write fixture");
    }
    dir
}

fn reference_output(tclsh: &PathBuf, program: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "tclrs-source-{}-{}.tcl",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path, program).expect("write program");
    let out = Command::new(tclsh)
        .arg(&path)
        .env("TCLRS_FIXTURE_LIBRARY", fixture_library())
        .output()
        .expect("run tclsh");
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "tclsh rejected program:\n{program}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn fixture_library() -> String {
    fixtures().join("fixture1.0").to_string_lossy().into_owned()
}

#[test]
fn source_and_find_library_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    let dir = fixtures();
    let dir = dir.to_string_lossy();
    // Both interpreters read it: tclsh through `env(TCLRS_FIXTURE_LIBRARY)` and
    // tclrs through the process environment, which is where an `env` array's
    // entries come from.
    std::env::set_var("TCLRS_FIXTURE_LIBRARY", fixture_library());

    let mut failures = Vec::new();
    for template in PROGRAMS {
        let program = template.replace("{DIR}", &dir);
        let expected = reference_output(&tclsh, &program);
        match tclrs::eval(&program) {
            Ok(outcome) if outcome.output == expected => {}
            Ok(outcome) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs: {:?}",
                outcome.output
            )),
            Err(e) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs failed: {e}"
            )),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} programs diverge:\n\n{}",
        failures.len(),
        PROGRAMS.len(),
        failures.join("\n\n")
    );
}

/// `source` evaluates a file in the interpreter that asked for it, so what the
/// file sets is set for the caller and what the caller set is readable in the
/// file — including across a namespace, which is the property `tk.tcl` needs.
#[test]
fn a_sourced_file_shares_the_callers_variables() {
    let dir = fixtures();
    let mut interp = tclrs::Interp::capturing();
    interp
        .eval(&format!(
            "set into 4\nsource {}/reads.tcl",
            dir.to_string_lossy()
        ))
        .expect("the file evaluates");
    assert_eq!(interp.global("out").as_deref(), Some("400"));

    interp
        .eval(&format!("source {}/ns.tcl", dir.to_string_lossy()))
        .expect("the namespace file evaluates");
    // A namespace variable is a global under its qualified name, so this is what
    // a later evaluation in the same interpreter sees.
    assert_eq!(interp.global("sourced::tag").as_deref(), Some("loaded"));
    assert_eq!(
        interp
            .eval("puts [namespace exists ::sourced]")
            .expect("the namespace survived the evaluation"),
        ""
    );
    assert_eq!(interp.take_output(), "1\n");
}

/// `tcl_findLibrary` locates a real installed library and hands it to `source`.
///
/// This is the step `tkInit` is made of. Whether the file it finds then compiles
/// is a separate question — `tk.tcl` defines procedures inside `if` blocks,
/// which this frontend does not yet lower — so what is asserted here is that the
/// search reached the file and the failure, if any, came from *evaluating* it
/// rather than from not finding it. The test is skipped where no Tk is
/// installed.
#[test]
fn find_library_reaches_an_installed_tk() {
    let Some(dir) = ["/usr/local/lib", "/usr/lib", "/opt/homebrew/lib"]
        .into_iter()
        .find(|d| Path::new(d).join("tk9.0").join("tk.tcl").exists())
    else {
        eprintln!("skipping: no installed tk9.0/tk.tcl to find");
        return;
    };

    let mut interp = tclrs::Interp::capturing();
    interp.set_global("auto_path", dir);
    let err = interp
        .eval("tcl_findLibrary tk 9.0 9.0.4 tk.tcl TK_LIBRARY tk_library")
        .expect_err("tk.tcl does not compile in this frontend yet");
    // The message the original ends with lists what each candidate failed
    // with, so a candidate that was *found* and then failed to evaluate names
    // itself in it. A search that never reached the file names no file at all.
    let attempted = format!("{dir}/tk9.0/tk.tcl:");
    assert!(
        err.msg.contains(&attempted),
        "the search never reached {attempted} — it reported:\n{}",
        err.msg
    );
}

/// The library variables `tcl_findLibrary` reads, seeded for a host that has not
/// set them itself.
#[test]
fn the_library_environment_is_seeded() {
    let mut interp = tclrs::Interp::capturing();
    assert_eq!(interp.global("auto_path"), None);
    tclrs::cmd_source::seed_library_environment(&mut interp);
    assert!(
        interp.global("auto_path").is_some(),
        "auto_path is what the canonical search walks"
    );
    assert!(interp.global("tcl_libPath").is_some());

    // A host that already set one keeps it.
    let mut interp = tclrs::Interp::capturing();
    interp.set_global("auto_path", "/somewhere/of/my/own");
    tclrs::cmd_source::seed_library_environment(&mut interp);
    assert_eq!(
        interp.global("auto_path").as_deref(),
        Some("/somewhere/of/my/own")
    );
}
