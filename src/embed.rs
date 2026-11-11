//! Running a Tcl script from a host program and collecting what it printed.

/// Run `src` as a complete program with `input` as its stdin and return
/// everything it wrote to stdout.
///
/// Each call runs on a thread of its own, sized [`crate::runtime::RECOMMENDED_STACK`]
/// like the `tclrs` binary's, with a fresh interpreter ([`crate::eval_captured`])
/// and a fresh channel table — so nothing carries over between calls and
/// concurrent calls cannot see each other's output. `puts`, `puts stdout` and
/// any other write to the `stdout` channel are collected; `gets stdin` and
/// `read stdin` read `input`.
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
            let (result, output) = crate::eval_captured(&src);
            result.map(|_| output)
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
