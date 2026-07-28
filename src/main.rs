//! The `tclrs` binary.
//!
//! ```text
//! tclrs FILE ?arg ...?   run a script file
//! tclrs -c SCRIPT        run SCRIPT
//! tclrs                  read a script from stdin; a REPL when stdin is a terminal
//! tclrs --version        print the version
//! ```
//!
//! `tclsh` is the specification for what this prints and what it exits with.
//! A script read from a file is one script: it stops at the first failure and
//! exits 1. A script read from stdin is a sequence of commands: each is
//! evaluated as it completes, a failure is reported and the next one runs, and
//! end of input exits 0. Both write errors to stderr and nothing else — there
//! is no banner, no prompt outside a terminal, and no output this binary
//! produces that the script did not ask for.
//!
//! `-c` and `--version` have no `tclsh` equivalent. `tclsh` reads stdin for any
//! argument starting with `-`; tclrs recognizes those two and rejects any other
//! option rather than silently doing something else with it.

use std::process::ExitCode;

use tclrs::Interp;

mod repl;

fn main() -> ExitCode {
    // Nested `eval` costs native stack: each level runs a VM of its own. The
    // interpreter refuses to nest deeper than `tclsh` does, and this thread is
    // sized so that limit is reached before the stack is — the process reports
    // a script error rather than dying on a signal.
    std::thread::Builder::new()
        .stack_size(tclrs::runtime::RECOMMENDED_STACK)
        .spawn(drive)
        .expect("spawn interpreter thread")
        .join()
        .unwrap_or(ExitCode::FAILURE)
}

fn drive() -> ExitCode {
    let mut argv = std::env::args();
    let program = argv.next().unwrap_or_else(|| "tclrs".to_string());
    let args: Vec<String> = argv.collect();

    match args.first().map(String::as_str) {
        Some("--version") => {
            println!("tclrs {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("-c") => match args.get(1) {
            Some(script) => run_command(script, &program, &args[2..]),
            None => fail("-c requires a script"),
        },
        Some(option) if option.starts_with('-') => fail(&format!("unknown option \"{option}\"")),
        Some(file) => run_file(file, &args[1..]),
        None => run_stdin(&program, &[]),
    }
}

/// Report a usage problem the way every other tclrs error is reported — on
/// stderr, terse, prefixed with the program name — and exit non-zero.
fn fail(reason: &str) -> ExitCode {
    eprintln!("tclrs: {reason}");
    ExitCode::FAILURE
}

/// An interpreter with `argv0`, `argc` and `argv` set, as `tclsh` sets them.
fn interp_for(argv0: &str, args: &[String]) -> Interp {
    let mut interp = Interp::new();
    interp.set_global("argv0", argv0);
    interp.set_global("argc", args.len().to_string());
    interp.set_global("argv", tclrs::list::join(args));
    interp
}

/// A whole script, evaluated as one. Its first failure ends it.
fn run_source(interp: &mut Interp, src: &str, file: Option<&str>) -> ExitCode {
    match interp.eval(src) {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            // The reference interpreter follows the message with the stack of
            // commands that raised it and then the source location. tclrs
            // resolves command dispatch while compiling, so it has no such
            // stack to print and does not invent one; the location is printed
            // when the failure was located, in the same spelling.
            eprintln!("{}", e.msg);
            if let (Some(file), Some(line)) = (file, e.line) {
                eprintln!("    (file \"{file}\" line {line})");
            }
            ExitCode::FAILURE
        }
    }
}

fn run_file(file: &str, args: &[String]) -> ExitCode {
    let src = match std::fs::read_to_string(file) {
        Ok(src) => src,
        Err(e) => {
            eprintln!("couldn't read file \"{file}\": {}", read_failure(&e));
            return ExitCode::FAILURE;
        }
    };
    let mut interp = interp_for(file, args);
    run_source(&mut interp, &src, Some(file))
}

fn run_command(script: &str, program: &str, args: &[String]) -> ExitCode {
    let mut interp = interp_for(program, args);
    run_source(&mut interp, script, None)
}

/// Stdin: the REPL when it is a terminal, the same loop without prompts or
/// result echo when it is not.
fn run_stdin(program: &str, args: &[String]) -> ExitCode {
    let mut interp = interp_for(program, args);
    repl::run(&mut interp, repl::stdin_is_terminal())
}

/// Why a script could not be read, in the reference interpreter's wording:
/// `couldn't read file "x": no such file or directory`. Anything with no
/// established spelling falls back to the operating system's own message
/// rather than being forced into one.
fn read_failure(e: &std::io::Error) -> String {
    use std::io::ErrorKind;
    match e.kind() {
        ErrorKind::NotFound => "no such file or directory".to_string(),
        ErrorKind::PermissionDenied => "permission denied".to_string(),
        ErrorKind::IsADirectory => "is a directory".to_string(),
        _ => e.to_string(),
    }
}
