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
//! argument starting with `-`; tclrs recognizes its own options and rejects any
//! other rather than silently doing something else with it.
//!
//! The remaining options do not run the script the ordinary way at all:
//! `--aot` and `--aot-object` send it through fusevm's closed-world compiler,
//! `--tiers` runs it and then reports which JIT tiers took its bytecode, and
//! `--disasm` prints the bytecode instead of running it. Each of them wants a
//! whole script, so they read a file, a `-c` argument, or all of stdin, and
//! never open a REPL.

use std::path::PathBuf;
use std::process::ExitCode;

use tclrs::Interp;

mod repl;

const USAGE: &str = "\
usage: tclrs [options] FILE ?arg ...?
       tclrs [options] -c SCRIPT ?arg ...?
       tclrs [options]                     read the script from stdin

options:
  -c SCRIPT       run SCRIPT instead of a file
  --aot OUT       compile the script to a standalone native executable at OUT
  --aot-object O  emit the relocatable AOT object only (link it yourself)
  --tiers         run the script, then report which fusevm tiers took its chunk
  --disasm        print the compiled bytecode instead of running it
  -h, --help      this message
  --version, -V   version";

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

/// What the command line asked for, once the options are read.
enum Action {
    Run,
    Aot(PathBuf),
    AotObject(PathBuf),
    Tiers,
    Disasm,
}

/// Where the script comes from, which also decides how failures are reported.
enum Source {
    /// A script file.
    File(String),
    /// A `-c` argument.
    Command(String),
    /// Standard input.
    Stdin,
}

fn drive() -> ExitCode {
    let mut argv = std::env::args();
    let program = argv.next().unwrap_or_else(|| "tclrs".to_string());
    let args: Vec<String> = argv.collect();

    let mut action = Action::Run;
    let mut source = Source::Stdin;
    let mut script_args: &[String] = &[];

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--version" | "-V" => {
                println!("tclrs {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "--tiers" => action = Action::Tiers,
            "--disasm" => action = Action::Disasm,
            flag @ ("--aot" | "--aot-object") => {
                let Some(out) = args.get(i + 1) else {
                    return fail(&format!("{flag} requires a path"));
                };
                action = match flag {
                    "--aot" => Action::Aot(PathBuf::from(out)),
                    _ => Action::AotObject(PathBuf::from(out)),
                };
                i += 1;
            }
            "-c" => {
                let Some(script) = args.get(i + 1) else {
                    return fail("-c requires a script");
                };
                source = Source::Command(script.clone());
                script_args = &args[(i + 2).min(args.len())..];
                break;
            }
            option if option.starts_with('-') => {
                return fail(&format!("unknown option \"{option}\""))
            }
            file => {
                source = Source::File(file.to_string());
                script_args = &args[i + 1..];
                break;
            }
        }
        i += 1;
    }

    // Only the ordinary run reads stdin a command at a time; every other action
    // wants the whole script before it does anything.
    if let (Action::Run, Source::Stdin) = (&action, &source) {
        let mut interp = interp_for(&program, script_args);
        return repl::run(&mut interp, repl::stdin_is_terminal());
    }

    let (src, file) = match &source {
        Source::File(path) => match std::fs::read_to_string(path) {
            Ok(src) => (src, Some(path.as_str())),
            Err(e) => {
                eprintln!("couldn't read file \"{path}\": {}", read_failure(&e));
                return ExitCode::FAILURE;
            }
        },
        Source::Command(script) => (script.clone(), None),
        Source::Stdin => match std::io::read_to_string(std::io::stdin()) {
            Ok(src) => (src, None),
            Err(e) => return fail(&format!("stdin: {e}")),
        },
    };

    match action {
        Action::Run => {
            let mut interp = interp_for(file.unwrap_or(&program), script_args);
            run_source(&mut interp, &src, file)
        }
        Action::Aot(out) => report(tclrs::aot::compile_executable(&src, &out)),
        Action::AotObject(out) => report(tclrs::aot::compile_object(&src, &out)),
        Action::Tiers => match tclrs::tiers::report(&src) {
            Ok(r) => {
                println!("{r}");
                ExitCode::SUCCESS
            }
            Err(e) => fail(&e),
        },
        Action::Disasm => match tclrs::runtime::compile(&src) {
            Ok(chunk) => {
                print!("{}", chunk.disassemble());
                ExitCode::SUCCESS
            }
            Err(e) => fail(&e),
        },
    }
}

/// Report a usage problem the way every other tclrs error is reported — on
/// stderr, terse, prefixed with the program name — and exit non-zero.
fn fail(reason: &str) -> ExitCode {
    eprintln!("tclrs: {reason}");
    ExitCode::FAILURE
}

/// The exit status of an action that either worked or explained itself.
fn report(outcome: Result<(), String>) -> ExitCode {
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(&e),
    }
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
