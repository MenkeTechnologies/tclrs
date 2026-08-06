//! The console branch of `TkpInit`, which is the branch the channel slots
//! exist for.
//!
//! `TkpInit` decides whether to open a console window by looking at stdin: if
//! it is "nullish" — a character device with no blocks — and there is no
//! startup script, it takes the console branch
//! (`tk9.0.4/macosx/tkMacOSXInit.c:493-494`, `:585-598`). Under a test harness
//! that is exactly what `/dev/null` is, so this file runs `tk-host` with stdin
//! bound to `/dev/null` and `tests/tk_utf16_window.rs` binds a pipe to take the
//! other branch. The two are the same binary; only stdin differs.
//!
//! What the console branch then does is fixed
//! (`tk9.0.4/generic/tkConsole.c:262-311`, `tk9.0.4/macosx/tkMacOSXInit.c:599-606`):
//!
//! ```text
//! Tcl_CreateChannel  ×3, each followed by Tcl_SetChannelOption ×3,
//!                        Tcl_SetStdChannel and Tcl_RegisterChannel
//! Tcl_GetStdChannel  ×3, each followed by Tcl_RegisterChannel
//! Tcl_CreateInterp
//! ```
//!
//! That shape is what is asserted, in order, because it is the account of
//! `Tk_InitConsoleChannels` actually running rather than of three unrelated
//! channel calls happening to occur.
//!
//! Needs a Tk dylib, and is skipped without one.

#![cfg(feature = "tk")]

use std::process::{Command, Stdio};

/// Run `tk-host` with stdin on `/dev/null`, or `None` if there is no Tk.
fn console_host() -> Option<String> {
    let exe = std::path::Path::new(env!("CARGO_BIN_EXE_tk-host"));
    let null = std::fs::File::open("/dev/null").expect("open /dev/null");
    let out = Command::new(exe)
        .stdin(Stdio::from(null))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run tk-host");
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    if err.contains("no Tk dylib at") || err.contains("dlopen(") {
        eprintln!("skipping: {}", err.trim());
        return None;
    }
    Some(err)
}

/// The names of the slots called, in order, one entry per call.
fn call_log(stderr: &str) -> Vec<&str> {
    stderr
        .lines()
        .filter(|l| l.starts_with("tkslot "))
        .filter_map(|l| l.split_whitespace().nth(4))
        .collect()
}

/// The slot the run stopped on, if it stopped on one.
fn trap(stderr: &str) -> Option<String> {
    let line = stderr.lines().find(|l| l.starts_with("tktrap "))?;
    Some(line.split_whitespace().nth(4)?.to_string())
}

/// `Tk_InitConsoleChannels` runs to completion: three channels created,
/// configured, installed as the standard channels and registered.
#[test]
fn the_console_branch_creates_and_installs_three_channels() {
    let Some(err) = console_host() else { return };
    let log = call_log(&err);

    // Only the channel slots, so that the `ckalloc(sizeof(ChannelData))` each
    // block starts with (`tk9.0.4/generic/tkConsole.c:263`) does not have to be
    // written into the expected shape. What is being pinned is the channel
    // sequence, not Tk's allocation pattern.
    let log: Vec<&str> = log.into_iter().filter(|n| n.contains("Chan")).collect();
    let first = log
        .iter()
        .position(|n| *n == "tcl_CreateChannel")
        .expect("the console branch never reached Tcl_CreateChannel");

    // tk9.0.4/generic/tkConsole.c:268-311, three times over.
    let expected: Vec<&str> = std::iter::repeat_n(
        [
            "tcl_CreateChannel",
            "tcl_SetChannelOption",
            "tcl_SetChannelOption",
            "tcl_SetChannelOption",
            "tcl_SetStdChannel",
            "tcl_RegisterChannel",
        ],
        3,
    )
    .flatten()
    // tk9.0.4/macosx/tkMacOSXInit.c:601-603: the three registrations TkpInit
    // does itself, each preceded by the lookup that supplies the channel.
    .chain(std::iter::repeat_n(["tcl_GetStdChannel", "tcl_RegisterChannel"], 3).flatten())
    .collect();

    assert_eq!(
        &log[first..first + expected.len()],
        &expected[..],
        "the console branch did not take the shape tkConsole.c gives it"
    );
}

/// The run gets past slot 88 and stops in `Tk_CreateConsoleWindow`, which
/// creates a second interpreter and calls `Tcl_Init` on it
/// (`tk9.0.4/generic/tkConsole.c:344-345`).
///
/// Pinned because it is the measurement this work moved: before the channel
/// subsystem existed the run stopped at `Tcl_CreateChannel` after 2639 calls.
/// `Tcl_Init` is not a channel slot — it runs Tcl's own `init.tcl` — so the
/// next thing in the way is the Tcl library, not the channel layer.
#[test]
fn the_console_branch_reaches_tcl_init_on_the_console_interpreter() {
    let Some(err) = console_host() else { return };
    let log = call_log(&err);

    assert!(
        log.contains(&"tcl_CreateChannel"),
        "the run never reached the channel subsystem"
    );
    assert_eq!(
        trap(&err).as_deref(),
        Some("tcl_Init"),
        "the console branch stopped somewhere other than Tcl_Init"
    );
    // Tk_CreateConsoleWindow's own first two statements, in order
    // (tk9.0.4/generic/tkConsole.c:344-345).
    let create_interp = log
        .iter()
        .rposition(|n| *n == "tcl_CreateInterp")
        .expect("no Tcl_CreateInterp");
    assert!(
        create_interp > log.iter().position(|n| *n == "tcl_CreateChannel").unwrap(),
        "the console interpreter was created before the console channels"
    );
}

/// Every channel Tk created is answered for by the same table a script's
/// channels live in: `Tcl_GetChannelType` hands back the very pointer Tk
/// registered, which is how `Tk_CreateConsoleWindow` recognises its own
/// channel (`tk9.0.4/generic/tkConsole.c:361-366`).
///
/// Measured indirectly, because the identity check happens inside Tk: the run
/// calls `Tcl_GetStdChannel` three times after the channels are installed and
/// does not stop, which it would if any of them came back null.
#[test]
fn the_standard_channel_slots_answer_with_the_channels_tk_installed() {
    let Some(err) = console_host() else { return };
    let log = call_log(&err);
    let installed = log.iter().filter(|n| **n == "tcl_SetStdChannel").count();
    let looked_up = log.iter().filter(|n| **n == "tcl_GetStdChannel").count();
    assert_eq!(
        installed, 3,
        "not all three standard channels were installed"
    );
    assert_eq!(looked_up, 3, "TkpInit did not look all three back up");
}
