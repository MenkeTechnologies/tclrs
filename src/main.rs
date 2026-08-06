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
//! end of input exits 0. Both write errors to stderr and nothing else — no
//! banner, no prompt, and no output this binary produces that the script did
//! not ask for.
//!
//! A terminal is the exception, and only a terminal: there the session is a
//! REPL, which is a thing to sit in rather than a thing to pipe through, and
//! [`repl_line`] gives it a line editor, a prompt, completion and a greeting.
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

#[cfg(feature = "tk")]
mod main_thread;
mod repl;
mod repl_line;

const USAGE: &str = "\
usage: tclrs [options] FILE ?arg ...?
       tclrs [options] -c SCRIPT ?arg ...?
       tclrs [options]                     read the script from stdin

options:
  -c SCRIPT       run SCRIPT instead of a file
  --aot OUT       compile the script to a standalone native executable at OUT
  --aot-object O  emit the relocatable AOT object only (link it yourself)
  --lsp           speak the Language Server Protocol on stdin/stdout
  --dap           speak the Debug Adapter Protocol on stdin/stdout
  --tiers         run the script, then report which fusevm tiers took its chunk
  --dump-tokens   print the parser's lexical output instead of running it
  --dump-ast      print the parse tree instead of running it
  --disasm        print the compiled bytecode instead of running it
  -h, --help      this message
  --version, -V   version";

/// The extra line `--help` prints in a build that can host Tk.
#[cfg(feature = "tk")]
const TK_USAGE: &str =
    "\n  --tk            run on the main thread, with the Tk event loop available";
#[cfg(not(feature = "tk"))]
const TK_USAGE: &str = "";

fn main() -> ExitCode {
    // Two ways to run the interpreter, and which one is chosen is the whole of
    // this binary's Tk restructuring.
    //
    // Ordinarily it runs on a thread of its own. Nested `eval` costs native
    // stack — each level runs a VM of its own — and the interpreter refuses to
    // nest deeper than `tclsh` does, so the thread is sized to reach that limit
    // before the stack runs out and the process reports a script error rather
    // than dying on a signal. Measured against this tree: 1000 levels of nested
    // `eval` need 99 MiB unoptimized and 14 MiB optimized, and
    // `RECOMMENDED_STACK` is 256 MiB.
    //
    // Hosting Tk on macOS makes that impossible as it stands, because Tk has to
    // be initialised on the *main* thread: `Tk_MacOSXSetupTkNotifier` installs
    // the Aqua event source only when the current run loop is the main run loop
    // (`tk9.0.4/macosx/tkMacOSXNotify.c:258-272`), and panics outright if that
    // holds on a thread AppKit does not consider the main one (`:259-266`). A
    // Tk session therefore runs on the main thread, and gets its stack from
    // [`main_thread`] instead of from a thread builder.
    //
    // Nothing else changes. Without `--tk`, and in a build without the `tk`
    // feature at all, this is the same spawn it has always been.
    match tk_session() {
        false => std::thread::Builder::new()
            .stack_size(tclrs::runtime::RECOMMENDED_STACK)
            .spawn(drive)
            .expect("spawn interpreter thread")
            .join()
            .unwrap_or(ExitCode::FAILURE),
        #[cfg(feature = "tk")]
        true => main_thread::run(drive),
        #[cfg(not(feature = "tk"))]
        true => unreachable!("--tk is not a recognized option in this build"),
    }
}

/// Whether the command line asked for a Tk session.
///
/// Scanned here rather than in [`drive`] because it decides which thread
/// [`drive`] runs on. A build without the `tk` feature never says yes, and
/// `--tk` falls through to the unknown-option error the same as any other
/// unrecognized flag.
#[cfg(feature = "tk")]
fn tk_session() -> bool {
    // Only before the script: everything after a file name or `-c` belongs to
    // the script, which is the same rule the option loop in `drive` follows.
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--tk" => return true,
            "-c" => return false,
            a if a.starts_with('-') => continue,
            _ => return false,
        }
    }
    false
}

#[cfg(not(feature = "tk"))]
fn tk_session() -> bool {
    false
}

/// What the command line asked for, once the options are read.
enum Action {
    Run,
    Aot(PathBuf),
    AotObject(PathBuf),
    Tiers,
    DumpTokens,
    DumpAst,
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
                println!("{USAGE}{TK_USAGE}");
                return ExitCode::SUCCESS;
            }
            // Read by `tk_session` before this loop ever runs; it decides which
            // thread this function is running on, not what it does.
            #[cfg(feature = "tk")]
            "--tk" => {}
            "--lsp" => return ExitCode::from(!tclrs::lsp::run_stdio() as u8),
            "--dap" => return ExitCode::from(tclrs::dap::run_stdio() as u8),
            "--tiers" => action = Action::Tiers,
            "--dump-tokens" => action = Action::DumpTokens,
            "--dump-ast" => action = Action::DumpAst,
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
        let mut interp = interp_for(&program, script_args, None);
        // A terminal gets the line editor — history, completion, multi-line
        // editing. A pipe gets the plain loop, which prints nothing of its own
        // and is what `tclsh < script` is compared against.
        let status = match repl::stdin_is_terminal() {
            true => repl_line::run(&mut interp),
            false => repl::run(&mut interp, false),
        };
        if status == ExitCode::SUCCESS {
            tk_main_loop();
        }
        return status;
    }

    let (src, file) = match &source {
        Source::File(path) => match std::fs::read_to_string(path) {
            Ok(src) => {
                // `info script` answers with the file being run, and with the
                // empty string for `-c` or stdin, as tclsh does for those.
                tclrs::runtime::note_script(path);
                (src, Some(path.as_str()))
            }
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
            let mut interp = interp_for(file.unwrap_or(&program), script_args, file);
            let status = run_source(&mut interp, &src, file);
            // Only a script that succeeded gets the main loop:
            // `Tcl_Main` guards the call with `exitCode == 0`
            // (`generic/tclMain.c:589-598`), so a `wish` script that fails
            // reports the failure and exits rather than leaving its
            // half-built windows on the screen with nothing to close them.
            if status == ExitCode::SUCCESS {
                tk_main_loop();
            }
            status
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
        Action::DumpTokens => match tclrs::dump::tokens(&src) {
            Ok(listing) => {
                print!("{listing}");
                ExitCode::SUCCESS
            }
            Err(e) => fail(&e),
        },
        Action::DumpAst => match tclrs::dump::ast(&src) {
            Ok(tree) => {
                print!("{tree}");
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
///
/// A `--tk` run also opens a Tk session on it before it is handed back, which
/// is the last moment that can happen: the session is what makes a name Tk has
/// not registered yet compile to a run-time lookup, and the script is compiled
/// whole before its first `package require Tk` runs. See
/// [`tclrs::tk::session`]. Opening one loads nothing — the toolkit is opened by
/// `package require Tk` and by nothing else.
fn interp_for(argv0: &str, args: &[String], file: Option<&str>) -> Interp {
    let mut interp = Interp::new();
    interp.set_global("argv0", argv0);
    interp.set_global("argc", args.len().to_string());
    interp.set_global("argv", tclrs::list::join(args));
    #[cfg(feature = "tk")]
    if tk_session() {
        tclrs::tk::session::open(&interp, file);
    }
    // The name of the script is a Tk session's business and nothing else's.
    #[cfg(not(feature = "tk"))]
    let _ = file;
    interp
}

/// Sit in Tk's main loop, if the script asked for Tk and Tk is still up.
///
/// `wish` does the same thing at the same point: `Tcl_MainLoopProc` is set by
/// `Tk_Init` (`tk9.0.4/generic/tkWindow.c:3477`) and `Tcl_Main` calls it once
/// the script has been evaluated and only if the script succeeded
/// (`generic/tclMain.c:589-598`). It returns when the last main window is
/// gone, which is what closing the window does — so a Tk application ends the
/// way a Tk application ends, and a script that never mentioned Tk is
/// unaffected because nothing registered a main loop.
#[cfg(feature = "tk")]
fn tk_main_loop() {
    if tk_session() {
        tclrs::tk::session::main_loop();
    }
}

#[cfg(not(feature = "tk"))]
fn tk_main_loop() {}

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
