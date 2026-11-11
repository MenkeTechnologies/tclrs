//! Running a Tcl script from a host program and collecting what it printed.

use crate::runtime::{eval_until_exit, Ended, Interp};

/// Run `src` as a complete program with `input` as its stdin and return
/// everything it wrote to stdout.
///
/// Each call runs on a thread of its own, sized [`crate::runtime::RECOMMENDED_STACK`]
/// like the `tclrs` binary's, with a fresh capturing interpreter and a fresh
/// channel table — so nothing carries over between calls and concurrent calls
/// cannot see each other's output. `puts`, `puts stdout`, any other write to
/// the `stdout` channel, and what an `exec` pipeline sends to `>@stdout` or
/// `2>@stdout` are collected; `gets stdin` and `read stdin` read `input`. A
/// background `exec … &` pipeline still writes to the process's stdout.
///
/// When the script ends without calling `exit`, `exit 0` (or `exit 1` after a
/// failure) is invoked as the binary does, so a script that redefined `exit`
/// has its procedure run.
///
/// A script that fails returns `Err` with the error message (and its line,
/// when the failure was located). `exit` / `exit 0` returns `Ok` with the
/// output written so far; `exit N` for any other `N` is `Err("exit status N")`.
/// A panic inside the interpreter is `Err` as well.
pub fn run_program(src: &str, input: &str) -> Result<String, String> {
    let (src, input) = (src.to_owned(), input.as_bytes().to_vec());
    std::thread::Builder::new()
        .stack_size(crate::runtime::RECOMMENDED_STACK)
        .spawn(move || {
            crate::cmd_channel::feed_stdin(input);
            let mut interp = Interp::capturing();
            let status = match eval_until_exit(&mut interp, &src) {
                Ended::Returned(result) => leave(&mut interp, result.map(drop)),
                Ended::Exited(code) => exit_status(code),
            };
            status.map(|()| interp.take_output())
        })
        .map_err(|e| format!("tclrs: cannot start the interpreter thread: {e}"))?
        .join()
        .map_err(|panic| {
            let msg = panic
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("unknown panic");
            format!("tclrs: panic: {msg}")
        })?
}

/// The binary's `leave` (`src/main.rs`), after `Tcl_Main`: invoke the `exit`
/// command with the script's status instead of ending on our own. The builtin
/// ends the run with that status; a replacement that returns (or fails) leaves
/// the status as it was. After a successful script, a replacement that exits
/// with a nonzero code makes the run fail with it; after a failed one the
/// script's error is kept, since the binary has already reported it.
fn leave(interp: &mut Interp, status: Result<(), String>) -> Result<(), String> {
    let code = if status.is_ok() { 0 } else { 1 };
    match eval_until_exit(interp, &format!("exit {code}")) {
        Ended::Exited(exited) if status.is_ok() => exit_status(exited),
        _ => status,
    }
}

fn exit_status(code: i32) -> Result<(), String> {
    match code {
        0 => Ok(()),
        _ => Err(format!("exit status {code}")),
    }
}
