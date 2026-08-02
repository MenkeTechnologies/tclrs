//! `tclrs --tk script.tcl`: the product binary hosting Tk, driven by the
//! script rather than by a probe's hardcoded sequence.
//!
//! Everything here runs the real binary against the real toolkit, because that
//! is the claim: `package require Tk` in a script loads Tk, the widget commands
//! Tk registers are callable from the same script, and the process then sits in
//! Tk's own main loop.
//!
//! # Two things every script here has to do, and why
//!
//! **stdin may not be `/dev/null`.** `TkpInit` reads `isatty(0)` and `fstat(0)`
//! and, when stdin is a character device with no blocks, either opens a console
//! window or `dup2`s `/dev/null` over stdout and stderr
//! (`tk9.0.4/macosx/tkMacOSXInit.c:493-494, 585-620`). `wish` behaves the same
//! way. Every child below therefore gets a pipe, which is neither a terminal
//! nor a character device.
//!
//! **A script that is not testing the main loop has to fail.** `Tcl_Main` calls
//! the main-loop procedure only when the script succeeded
//! (`generic/tclMain.c:589-598`), and this binary follows it, so a trailing
//! `error` is how a Tk script here returns rather than sitting in the loop
//! forever. The one test that *is* about the loop leaves it out and kills the
//! child itself.
//!
//! Needs a Tk dylib, and is skipped without one.

#![cfg(feature = "tk")]

use std::io::Read;
use std::process::{Child, Command, Stdio};

/// Write `src` to a scratch file named after the test, and give back the path.
fn script(name: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("tclrs-tk-{name}-{}.tcl", std::process::id()));
    std::fs::write(&path, src).expect("write the script");
    path
}

/// Spawn `tclrs --tk` on a script, with a pipe on stdin.
fn spawn(path: &std::path::Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_tclrs"))
        .arg("--tk")
        .arg(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tclrs --tk")
}

/// What one run produced. `None` when there is no Tk to run against, which is
/// the skip every test in this file honours.
struct Ran {
    stdout: String,
    stderr: String,
    status: Option<i32>,
}

fn run(name: &str, src: &str) -> Option<Ran> {
    let path = script(name, src);
    let out = spawn(&path)
        .wait_with_output()
        .expect("wait for tclrs --tk");
    let _ = std::fs::remove_file(&path);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if stderr.contains("no Tk dylib at") || stderr.contains("dlopen(") {
        eprintln!("skipping: {}", stderr.trim());
        return None;
    }
    Some(Ran {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr,
        status: out.status.code(),
    })
}

/// The line `puts NAME=value` produced, or `None`.
fn field<'a>(stdout: &'a str, name: &str) -> Option<&'a str> {
    stdout
        .lines()
        .find_map(|l| l.strip_prefix(name)?.strip_prefix('='))
}

/// `package require Tk` is what loads the toolkit. Nothing else does: a `--tk`
/// run of a script that never asks for Tk never opens the dylib.
#[test]
fn package_require_tk_loads_and_initialises_the_toolkit() {
    let Some(ran) = run(
        "require",
        "puts VERSION=[package require Tk]\n\
         puts EXISTS=[winfo exists .]\n\
         puts CLASS=[winfo class .]\n\
         puts PROVIDED=[package provide Tk]\n\
         puts AGAIN=[package require Tk]\n\
         error stop-before-the-main-loop\n",
    ) else {
        return;
    };
    assert_eq!(ran.status, Some(1), "{ran:?}", ran = ran.stderr);
    // The version is whatever the dylib provides itself as, which is Tk's own
    // `TK_PATCH_LEVEL` (`tk9.0.4/generic/tkWindow.c:3461-3469`) and not
    // something written here.
    let version = field(&ran.stdout, "VERSION").expect(&ran.stdout);
    assert!(
        version.starts_with("9."),
        "expected a Tk 9 version, got {version:?}"
    );
    assert_eq!(field(&ran.stdout, "EXISTS"), Some("1"));
    assert_eq!(field(&ran.stdout, "CLASS"), Some("Tk"));
    // `package require` answers out of the registry Tk's own
    // `Tcl_PkgProvideEx` wrote into, so `provide` reports the same string.
    assert_eq!(field(&ran.stdout, "PROVIDED"), Some(version));
    // A second require is the registry lookup, not a second `Tk_Init`.
    assert_eq!(field(&ran.stdout, "AGAIN"), Some(version));
}

/// The reason the session is opened before the script is compiled.
///
/// `button` and `pack` do not exist when this script is compiled — Tk
/// registers them during `Tk_Init` (`tk9.0.4/generic/tkWindow.c:1004-1096`),
/// which the first line of the script triggers. If the host were built by
/// `package require` rather than by `--tk`, both names would already have been
/// lowered as a deferred `invalid command name`.
#[test]
fn widget_commands_are_callable_from_the_script_that_loaded_tk() {
    let Some(ran) = run(
        "widgets",
        "package require Tk\n\
         puts MADE=[button .b -text hello]\n\
         pack .b\n\
         puts MANAGER=[winfo manager .b]\n\
         puts CLASS=[winfo class .b]\n\
         puts CHILDREN=[winfo children .]\n\
         puts TEXT=[.b cget -text]\n\
         error stop-before-the-main-loop\n",
    ) else {
        return;
    };
    assert_eq!(field(&ran.stdout, "MADE"), Some(".b"), "{}", ran.stderr);
    assert_eq!(field(&ran.stdout, "CLASS"), Some("Button"));
    assert_eq!(field(&ran.stdout, "CHILDREN"), Some(".b"));
    assert_eq!(field(&ran.stdout, "MANAGER"), Some("pack"));
    // The widget command Tk created for `.b`, answering about the widget's own
    // state — a second name that did not exist when this script was compiled.
    assert_eq!(field(&ran.stdout, "TEXT"), Some("hello"));
    // `winfo ismapped` is deliberately not asserted: mapping happens on an
    // idle handler, and this script ends before the event loop runs one. The
    // window-server evidence for the mapped window is in
    // `a_script_that_ends_normally_stays_in_the_tk_event_loop`, which does let
    // the loop run.
}

/// A `-command` callback runs in the interpreter the script is running in, not
/// in one of the host's own.
///
/// The callback reads a variable the script set before registering it and
/// writes one the script reads afterwards. Both directions have to work, and
/// neither does if the host paired itself with a fresh interpreter.
#[test]
fn a_callback_runs_in_the_scripts_own_interpreter() {
    let Some(ran) = run(
        "callback",
        "package require Tk\n\
         set before from-the-script\n\
         set after untouched\n\
         button .b -command {puts SAW=$before; set after from-the-callback}\n\
         .b invoke\n\
         puts AFTER=$after\n\
         error stop-before-the-main-loop\n",
    ) else {
        return;
    };
    assert_eq!(
        field(&ran.stdout, "SAW"),
        Some("from-the-script"),
        "the callback could not see the script's variables: {}",
        ran.stderr
    );
    assert_eq!(
        field(&ran.stdout, "AFTER"),
        Some("from-the-callback"),
        "what the callback set did not reach the script"
    );
}

/// The main loop is Tk's own, and the process sits in it.
///
/// `Tk_MainLoop` is `while (Tk_GetNumMainWindows() > 0) Tcl_DoOneEvent(0)`
/// (`tk9.0.4/generic/tkEvent.c`), registered with `Tcl_SetMainLoop` as
/// `Tk_Init` finishes (`tk9.0.4/generic/tkWindow.c:3477`) and driven by this
/// crate's ported notifier through the `Tcl_DoOneEvent` slot. So a script that
/// creates a widget and ends normally does not exit: it stays up, servicing
/// events, until its last window is gone.
#[test]
fn a_script_that_ends_normally_stays_in_the_tk_event_loop() {
    let path = script(
        "mainloop",
        "package require Tk\n\
         button .b -text hello\n\
         pack .b\n\
         puts READY\n",
    );
    let mut child = spawn(&path);
    // Long enough for `Tk_Init`, the widget, and several passes of the loop.
    // The assertion is not about the duration; it is that the process is still
    // there after work that would have ended a script that simply ran out.
    std::thread::sleep(std::time::Duration::from_secs(5));
    let alive = child.try_wait().expect("poll the child").is_none();
    let _ = child.kill();
    let out = child.wait_with_output().expect("reap the child");
    let _ = std::fs::remove_file(&path);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stderr.contains("no Tk dylib at") || stderr.contains("dlopen(") {
        eprintln!("skipping: {}", stderr.trim());
        return;
    }
    assert!(
        alive,
        "the process left the event loop; stderr: {stderr}\nstdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// A `--tk` run of a script that never mentions Tk loads nothing and ends.
///
/// The session builds the host so that an unknown name can be looked up at run
/// time, and that is all it builds. If it opened the dylib as well, this script
/// would sit in the main loop and never return.
#[test]
fn a_tk_session_without_a_require_neither_loads_tk_nor_waits() {
    let Some(ran) = run("noload", "puts PLAIN=[expr {6 * 7}]\n") else {
        return;
    };
    assert_eq!(ran.status, Some(0), "{}", ran.stderr);
    assert_eq!(field(&ran.stdout, "PLAIN"), Some("42"));
    assert_eq!(ran.stderr, "", "a session wrote to stderr on its own");
}

/// What `Tk_Init` returned, as a measurement rather than as an expectation.
///
/// The completion code is a property of how much of the Tcl language this
/// frontend has, not of the Tk ABI: `Tk_Init` returns the code of its last
/// statement, which evaluates the `tkInit` script
/// (`tk9.0.4/generic/tkWindow.c:3508-3518`) — long after it has created the
/// main window, registered every widget command and provided itself as a
/// package (`:3461-3469`). So this pins the *mechanism* — that the code is
/// reported, and that `package require Tk` answers out of the registry either
/// way — and deliberately not the number, which is expected to change as the
/// frontend grows.
#[test]
fn the_tk_init_completion_code_is_reported_and_does_not_decide_the_require() {
    let path = script(
        "initcode",
        "puts VERSION=[package require Tk]\nerror stop-before-the-main-loop\n",
    );
    let out = Command::new(env!("CARGO_BIN_EXE_tclrs"))
        .arg("--tk")
        .arg(&path)
        .env("TCLRS_TK_TRACE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tclrs --tk")
        .wait_with_output()
        .expect("wait for tclrs --tk");
    let _ = std::fs::remove_file(&path);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if stderr.contains("no Tk dylib at") || stderr.contains("dlopen(") {
        eprintln!("skipping: {}", stderr.trim());
        return;
    }
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let report = stderr
        .lines()
        .find(|l| l.starts_with("tkinit code "))
        .unwrap_or_else(|| panic!("no tkinit line in:\n{stderr}"));
    eprintln!("{report}");
    assert!(
        field(&stdout, "VERSION").is_some_and(|v| v.starts_with("9.")),
        "`package require Tk` did not answer: {stdout}\n{report}"
    );
}

/// The call log belongs to the probe, not to an application.
///
/// `Tk_Init` serves thousands of stub calls, and a line each would be output
/// the script never asked for. `TCLRS_TK_TRACE` is what puts it back.
#[test]
fn a_session_does_not_print_the_stub_call_log() {
    let Some(ran) = run(
        "quiet",
        "package require Tk\nputs READY\nerror stop-before-the-main-loop\n",
    ) else {
        return;
    };
    assert!(
        !ran.stderr.contains("tkslot "),
        "the session printed its call log:\n{}",
        ran.stderr
    );
    assert_eq!(
        ran.stderr, "stop-before-the-main-loop\n",
        "a session wrote something the script did not ask for"
    );
    assert!(ran.stdout.contains("READY"));
}

/// The startup script has to be recorded, because Tk reads it back.
///
/// `TkpInit` opens a console window — and with it the channel subsystem this
/// host does not have — when stdin is not a terminal and there is *no* startup
/// script (`tk9.0.4/macosx/tkMacOSXInit.c:585`). `Tcl_MainEx` records the file
/// it was given (`generic/tclMain.c:336-338`); a session that did not would
/// send Tk down a branch `wish script.tcl` never takes.
#[test]
fn the_script_file_is_recorded_as_the_startup_script() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_tclrs"))
        .arg("--tk")
        .arg(script(
            "startup",
            "package require Tk\nputs READY\nerror stop\n",
        ))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tclrs --tk");
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr pipe")
        .read_to_string(&mut stderr)
        .expect("read stderr");
    let status = child.wait().expect("wait");
    if stderr.contains("no Tk dylib at") || stderr.contains("dlopen(") {
        eprintln!("skipping: {}", stderr.trim());
        return;
    }
    // stdin is `/dev/null` here on purpose: that is the case the recorded
    // startup script exists for. Tk redirects stdout and stderr to /dev/null
    // in this configuration (`:607-620`), so nothing can be read back — what
    // is asserted is that the process reached its own end rather than aborting
    // inside `Tk_InitConsoleChannels`.
    assert_eq!(
        status.code(),
        Some(1),
        "a console-less run did not reach the script's own failure"
    );
}
