//! The read-eval-print loop.
//!
//! One loop covers both of the reference interpreter's stdin modes, because
//! they differ only in what they print. With a terminal `tclsh` writes a `% `
//! prompt and echoes the value of each command; reading a piped script it
//! writes neither. Everything else is the same in both, and is not what a
//! script file gets:
//!
//! * commands are evaluated one at a time as they complete, not as one script;
//! * a command that fails is reported and the loop goes on to the next;
//! * end of input exits successfully — which is why `tclsh < script` exits 0
//!   where `tclsh script` exits 1 — and text left half-typed is discarded.
//!
//! Reading one line at a time is not enough: a command can span lines. The loop
//! keeps reading while the text so far leaves a brace, quote or bracket open,
//! which is what a `{` at the end of a line does.

use std::io::{self, BufRead, IsTerminal, Write};
use std::process::ExitCode;

use tclrs::Interp;

/// What `tclsh` writes when it wants a command. Its continuation prompt is
/// empty, so a command being typed across lines is not prefixed at all.
const PROMPT: &str = "% ";

/// Read commands from stdin until end of input, evaluating each against
/// `interp`. Prompts and echoes results only when `interactive`.
pub fn run(interp: &mut Interp, interactive: bool) -> ExitCode {
    let stdin = io::stdin();
    let mut pending = String::new();

    loop {
        if interactive && pending.is_empty() {
            print!("{PROMPT}");
            if io::stdout().flush().is_err() {
                return ExitCode::FAILURE;
            }
        }

        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            // End of input. Anything typed but not finished is dropped.
            Ok(0) => return ExitCode::SUCCESS,
            Ok(_) => {}
            Err(e) => {
                eprintln!("tclrs: {e}");
                return ExitCode::FAILURE;
            }
        }

        pending.push_str(&line);
        if incomplete(&pending) {
            continue;
        }

        let script = std::mem::take(&mut pending);
        match interp.eval(&script) {
            // An empty result is not echoed, so `set x {}` and `puts hi` add
            // nothing beyond what they did themselves.
            Ok(result) => {
                if interactive && !result.is_empty() {
                    println!("{result}");
                }
            }
            // No `(file …)` line here: the reference interpreter adds one only
            // for a script it read from a file.
            Err(e) => eprintln!("{}", e.msg),
        }
    }
}

/// Whether `src` needs more input before it is a script.
///
/// The answer lives in [`tclrs::parser::incomplete`], because `info complete`
/// asks the same question of the same parser and the two must not be able to
/// disagree — a REPL that kept reading where `info complete` said 1 would be
/// answering from a second reading of the same text.
pub fn incomplete(src: &str) -> bool {
    tclrs::parser::incomplete(src)
}

/// True when stdin is a terminal, and the loop should prompt and echo.
pub fn stdin_is_terminal() -> bool {
    io::stdin().is_terminal()
}
