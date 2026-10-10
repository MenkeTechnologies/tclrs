//! `exit`, `time` and `exec`: the commands that deal with the process the
//! interpreter lives in.
//!
//! * `exit ?returnCode?` is `Tcl_ExitObjCmd` (`generic/tclCmdAH.c`): it ends the
//!   process, and before it does it hands everything buffered — the interpreter's
//!   standard output and every open channel — to the operating system, which is
//!   what `Tcl_Exit` → `Tcl_Finalize` closing the channels amounts to. It is
//!   not an error and nothing a script has open (`catch`, `try … finally`)
//!   intercepts it.
//! * `time command ?count?` is `Tcl_TimeObjCmd` (`generic/tclCmdMZ.c`): run the
//!   script `count` times at the calling level and report the mean in
//!   microseconds, as an integer for a count of one or less and as a double
//!   otherwise.
//! * `exec ?-ignorestderr? ?-keepnewline? ?--? arg …` is `Tcl_ExecObjCmd` over
//!   `TclCreatePipeline` (`unix/tclUnixPipe.c`): the argument words are split
//!   into the commands of a pipeline and their redirections, the pipeline is
//!   run, and standard output is the command's value.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Instant;

use fusevm::{Op, Value, VM};

use crate::compiler::{CompileError, Compiler};
use crate::parser::Word;
use crate::runtime::{to_tcl_string, Output, Shared, TclError};

/// Extension opcode ids owned by this module.
pub mod ext {
    pub use crate::compiler::ext::PROCESS_BASE as BASE;
    /// `[code?]` → never returns.
    pub const EXIT: u16 = BASE;
    /// `[declared?, script, count?]` → `N microseconds per iteration`. The inline
    /// operand is the number of words after `time`, plus `IN_FRAME`
    /// when `declared` leads the stack.
    pub const TIME: u16 = BASE + 1;
    /// `[arg …]` → what the pipeline wrote to standard output.
    pub const EXEC: u16 = BASE + 2;
    /// `[name, arg …]` → the command's value, having replaced the running
    /// procedure activation with the call where the callee is a procedure of
    /// the same chunk.
    pub const TAILCALL: u16 = BASE + 3;
    /// [`TAILCALL`] from inside a `catch` or `try` region: the call is made from
    /// the activation, which stays.
    pub const TAILCALL_CALL: u16 = BASE + 4;
}

/// The command names this module claims.
pub const COMMANDS: &[&str] = &["exec", "exit", "tailcall", "time"];

/// Set in `time`'s inline operand when the command was written inside a
/// procedure body, so the script runs against that body's frame.
const IN_FRAME: u8 = 4;

/// Whether an id belongs to this module's block.
pub(crate) fn is_op(id: u16) -> bool {
    (ext::BASE..crate::compiler::ext::PROCESS_END).contains(&id)
}

// ── compiling ────────────────────────────────────────────────────────────

pub(crate) fn compile(c: &mut Compiler, name: &str, args: &[Word]) -> Result<(), CompileError> {
    match name {
        "exit" => {
            if args.len() > 1 {
                return c.error("wrong # args: should be \"exit ?returnCode?\"");
            }
            for w in args {
                c.word(w)?;
            }
            c.emit(
                Op::Extended(ext::EXIT, args.len() as u8),
                1 - args.len() as i32,
            );
            Ok(())
        }
        "time" => {
            if args.is_empty() || args.len() > 2 {
                return c.error("wrong # args: should be \"time command ?count?\"");
            }
            let mut operand = args.len() as u8;
            let mut pushed = args.len() as i32;
            if let Some(declared) = c.declared_globals() {
                c.push_str(&declared);
                operand |= IN_FRAME;
                pushed += 1;
            }
            for w in args {
                c.word(w)?;
            }
            c.emit(Op::Extended(ext::TIME, operand), 1 - pushed);
            Ok(())
        }
        "tailcall" => compile_tailcall(c, args),
        _ => {
            let Ok(argc) = u8::try_from(args.len()) else {
                return c.error("too many arguments for \"exec\"");
            };
            for w in args {
                c.word(w)?;
            }
            c.emit(Op::Extended(ext::EXEC, argc), 1 - args.len() as i32);
            Ok(())
        }
    }
}

// ── running ──────────────────────────────────────────────────────────────

/// Run one of this module's ops. `out` is the interpreter's standard output,
/// which `exit` and `exec` have to flush before the process does anything that
/// could write behind its back.
pub(crate) fn run(
    interp: &Shared,
    vm: &mut VM,
    id: u16,
    arg: u8,
    out: &Output,
) -> Result<(), TclError> {
    if let Some(done) = overridden(interp, vm, id, arg) {
        return done;
    }
    match id {
        ext::EXIT => {
            let code = match arg {
                0 => 0,
                _ => {
                    let text = to_tcl_string(&vm.pop());
                    let wide = crate::list::wide(&text).map_err(TclError::plain)?;
                    // `Tcl_GetIntFromObj`: a value outside `int` is as much a
                    // non-integer as a word.
                    i32::try_from(wide).map_err(|_| {
                        TclError::plain("integer value too large to represent as non-long")
                    })?
                }
            };
            terminate(out, code)
        }
        ext::TIME => time_op(interp, vm, arg),
        ext::TAILCALL => crate::procs::tailcall_op(interp, vm, arg, true),
        ext::TAILCALL_CALL => crate::procs::tailcall_op(interp, vm, arg, false),
        _ => {
            let mut words = Vec::with_capacity(arg as usize);
            for _ in 0..arg {
                words.push(to_tcl_string(&vm.pop()));
            }
            words.reverse();
            out.flush();
            let value = exec(&words, out)?;
            vm.push(Value::Str(Arc::new(value)));
            Ok(())
        }
    }
}

/// A procedure of the command's name, defined in some chunk the compiler of
/// this call did not see, takes the call: tclsh resolves a name when the command
/// runs, so `proc exit {} {…}` replaces `exit` for every script that runs after
/// it. `None` when the builtin is what was meant.
fn overridden(interp: &Shared, vm: &mut VM, id: u16, arg: u8) -> Option<Result<(), TclError>> {
    let name = match id {
        ext::EXIT => "exit",
        ext::TIME => "time",
        ext::TAILCALL | ext::TAILCALL_CALL => "tailcall",
        _ => "exec",
    };
    crate::procs::defined_proc(interp, name)?;
    let argc = match id {
        ext::TIME => (arg & !IN_FRAME) as usize,
        _ => arg as usize,
    };
    let mut args = Vec::with_capacity(argc);
    for _ in 0..argc {
        args.push(vm.pop());
    }
    args.reverse();
    // `time` pushed the body's declarations ahead of its words; the procedure
    // is called with the words alone.
    if id == ext::TIME && arg & IN_FRAME != 0 {
        vm.pop();
    }
    Some(crate::procs::invoke(interp, vm, name, &args, 0))
}

/// Flush everything buffered and end the process.
pub(crate) fn terminate(out: &Output, code: i32) -> ! {
    out.flush();
    crate::cmd_channel::flush_all();
    std::process::exit(code)
}

/// `Tcl_TimeObjCmd`.
fn time_op(interp: &Shared, vm: &mut VM, arg: u8) -> Result<(), TclError> {
    let argc = (arg & !IN_FRAME) as usize;
    let mut words = Vec::with_capacity(argc);
    for _ in 0..argc {
        words.push(to_tcl_string(&vm.pop()));
    }
    words.reverse();
    let declared = (arg & IN_FRAME != 0).then(|| to_tcl_string(&vm.pop()));
    let script = words.remove(0);
    let count = match words.first() {
        Some(text) => crate::list::wide(text).map_err(TclError::plain)?,
        None => 1,
    };

    let started = Instant::now();
    let body = |interp: &Shared| -> Result<(), TclError> {
        for _ in 0..count.max(0) {
            crate::runtime::run_source(interp, &script)?;
        }
        Ok(())
    };
    match declared {
        // The frame of the innermost procedure activation, as `eval` uses.
        Some(declared) => {
            let up = crate::runtime::levels(vm).first().copied().unwrap_or(0);
            crate::runtime::in_frame(interp, vm, up, &declared, body)?;
        }
        None => {
            crate::runtime::with_written_back(interp, vm, body)?;
        }
    }
    let micros = started.elapsed().as_micros() as f64;
    // `if (count <= 1)`: an integer, since the time is not fractional.
    let text = if count <= 1 {
        format!("{}", if count <= 0 { 0 } else { micros as i64 })
    } else {
        crate::runtime::format_double(micros / count as f64)
    };
    vm.push(Value::Str(Arc::new(format!(
        "{text} microseconds per iteration"
    ))));
    Ok(())
}

// ── exec ─────────────────────────────────────────────────────────────────

/// Where one standard stream of one pipeline command goes.
enum Sink {
    /// Whatever the interpreter's own stream is.
    Inherit,
    /// `>file`, `>>file`: opened when the pipeline is built.
    File(File),
    /// `>@stdout`-style redirection to a Tcl channel. The command's output is
    /// collected and written to the channel after the pipeline finishes.
    Channel(String),
    /// The stream is merged into standard output (`2>@1`, `>&`).
    ToStdout,
    /// Captured: standard output becomes the command's value, standard error
    /// becomes its error.
    Capture,
}

/// Where a pipeline's standard input comes from.
enum Source {
    Inherit,
    File(File),
    Text(Vec<u8>),
}

struct Stage {
    argv: Vec<String>,
    /// `|&`: this stage's standard error goes down the pipe too.
    stderr_to_pipe: bool,
}

struct Plan {
    stages: Vec<Stage>,
    stdin: Source,
    stdout: Sink,
    stderr: Sink,
    background: bool,
    keep_newline: bool,
}

const WRONG_ARGS: &str = "wrong # args: should be \"exec ?-option ...? arg ?arg ...?\"";

fn io_message(error: &std::io::Error) -> String {
    match error.raw_os_error() {
        // SAFETY: `strerror` returns a pointer to a static string for every
        // errno value, and the result is copied before returning.
        Some(code) => unsafe {
            let text = libc::strerror(code);
            if text.is_null() {
                return "unknown error".to_string();
            }
            std::ffi::CStr::from_ptr(text)
                .to_string_lossy()
                .to_lowercase()
        },
        None => error.to_string(),
    }
}

/// Split the argument words into stages and redirections, as
/// `TclCreatePipeline` does: an operator may carry its operand in the same
/// word (`>out.txt`) or take the next one.
fn parse(words: &[String]) -> Result<Plan, TclError> {
    let mut ignore_stderr = false;
    let mut keep_newline = false;
    let mut at = 0;
    while let Some(word) = words.get(at) {
        if !word.starts_with('-') {
            break;
        }
        match word.as_str() {
            "-ignorestderr" => ignore_stderr = true,
            "-keepnewline" => keep_newline = true,
            "--" => {
                at += 1;
                break;
            }
            other => {
                return Err(TclError::plain(format!(
                    "bad option \"{other}\": must be -ignorestderr, -keepnewline, or --"
                )))
            }
        }
        at += 1;
    }
    let mut args = &words[at..];
    if args.is_empty() {
        return Err(TclError::plain(WRONG_ARGS));
    }
    let mut background = false;
    if args.last().map(String::as_str) == Some("&") {
        background = true;
        args = &args[..args.len() - 1];
    }

    let mut stages: Vec<Stage> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut stdin = Source::Inherit;
    let mut stdout = Sink::Capture;
    let mut stderr = Sink::Capture;
    let mut stderr_set = false;

    let mut i = 0;
    while i < args.len() {
        let word = args[i].as_str();
        i += 1;
        // The operator is the longest prefix that is one; what follows it in
        // the same word is its operand.
        let operator = [
            "|&", "|", "<<", "<@", "<", ">>&@", ">>&", ">&@", ">&", ">>", ">@", ">",
        ]
        .iter()
        .chain(["2>>", "2>@1", "2>@", "2>"].iter())
        .find(|op| word.starts_with(**op))
        .copied();
        let Some(op) = operator else {
            current.push(word.to_string());
            continue;
        };
        if op == "|" || op == "|&" {
            if !word[op.len()..].is_empty() {
                // `|cat` is the word `|cat`, not an operator, only when
                // nothing follows the bar: Tcl reads the bar alone.
                current.push(word.to_string());
                continue;
            }
            if current.is_empty() {
                return Err(TclError::plain("illegal use of | or |& in command"));
            }
            stages.push(Stage {
                argv: std::mem::take(&mut current),
                stderr_to_pipe: op == "|&",
            });
            continue;
        }
        // `2>@1` carries no operand: it is the whole of the redirection.
        let mut operand = word[op.len()..].to_string();
        if op == "2>@1" {
            if !operand.is_empty() {
                // `2>@1x` is `2>@` with the channel `1x`.
                operand = format!("1{operand}");
            } else {
                if stderr_set {
                    return Err(TclError::plain(
                        "can't specify \"2>@1\" as last word in command",
                    ));
                }
                stderr = Sink::ToStdout;
                stderr_set = true;
                continue;
            }
        }
        let op = if op == "2>@1" { "2>@" } else { op };
        if operand.is_empty() {
            match args.get(i) {
                Some(next) => {
                    operand = next.clone();
                    i += 1;
                }
                None => {
                    return Err(TclError::plain(format!(
                        "can't specify \"{op}\" as last word in command"
                    )))
                }
            }
        }
        let file_for = |append: bool, path: &str| -> Result<File, TclError> {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true);
            if append {
                options.append(true);
            } else {
                options.truncate(true);
            }
            options.open(path).map_err(|e| {
                TclError::plain(format!(
                    "couldn't write file \"{path}\": {}",
                    io_message(&e)
                ))
            })
        };
        let channel_sink = |name: &str| -> Sink {
            match name {
                "stdout" => Sink::Inherit,
                _ => Sink::Channel(name.to_string()),
            }
        };
        match op {
            "<" => {
                let file = File::open(&operand).map_err(|e| {
                    TclError::plain(format!(
                        "couldn't read file \"{operand}\": {}",
                        io_message(&e)
                    ))
                })?;
                stdin = Source::File(file);
            }
            "<<" => {
                stdin = Source::Text(operand.into_bytes());
            }
            "<@" => {
                let text = match operand.as_str() {
                    "stdin" => None,
                    other => Some(read_channel(other)?),
                };
                stdin = match text {
                    Some(t) => Source::Text(t.into_bytes()),
                    None => Source::Inherit,
                };
            }
            ">" | ">>" => {
                stdout = Sink::File(file_for(op == ">>", &operand)?);
            }
            ">&" | ">>&" => {
                stdout = Sink::File(file_for(op == ">>&", &operand)?);
                stderr = Sink::ToStdout;
                stderr_set = true;
            }
            ">@" => {
                stdout = match operand.as_str() {
                    "stderr" => Sink::Channel("stderr".to_string()),
                    other => channel_sink(other),
                };
            }
            ">&@" | ">>&@" => {
                stdout = match operand.as_str() {
                    "stderr" => Sink::Channel("stderr".to_string()),
                    other => channel_sink(other),
                };
                stderr = Sink::ToStdout;
                stderr_set = true;
            }
            "2>" | "2>>" => {
                stderr = Sink::File(file_for(op == "2>>", &operand)?);
                stderr_set = true;
            }
            _ => {
                // `2>@name`
                stderr = match operand.as_str() {
                    "stderr" => Sink::Inherit,
                    "stdout" => Sink::Channel("stdout".to_string()),
                    other => Sink::Channel(other.to_string()),
                };
                stderr_set = true;
            }
        }
    }
    if current.is_empty() {
        return Err(TclError::plain(if stages.is_empty() {
            WRONG_ARGS.to_string()
        } else {
            "illegal use of | or |& in command".to_string()
        }));
    }
    stages.push(Stage {
        argv: current,
        stderr_to_pipe: false,
    });
    if ignore_stderr && matches!(stderr, Sink::Capture) {
        stderr = Sink::Inherit;
    }
    Ok(Plan {
        stages,
        stdin,
        stdout,
        stderr,
        background,
        keep_newline,
    })
}

/// The whole of a channel's remaining input, for `<@channel`.
fn read_channel(name: &str) -> Result<String, TclError> {
    let id = crate::cmd_channel::resolve_readable(name).map_err(TclError::plain)?;
    crate::cmd_channel::read_chars(id, None).map_err(TclError::plain)
}

/// A pipe's two ends as owned files.
fn pipe() -> Result<(File, File), TclError> {
    let mut fds = [0i32; 2];
    // SAFETY: `fds` is a two-element array, as `pipe(2)` requires.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(TclError::plain(format!(
            "couldn't create pipe: {}",
            io_message(&std::io::Error::last_os_error())
        )));
    }
    // SAFETY: both descriptors were just created and are owned by nothing else.
    unsafe {
        libc::fcntl(fds[0], libc::F_SETFD, libc::FD_CLOEXEC);
        libc::fcntl(fds[1], libc::F_SETFD, libc::FD_CLOEXEC);
        Ok((File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])))
    }
}

/// `SIGNAME` and the message `Tcl_SignalMsg` words for it.
fn signal_names(signal: i32) -> (String, String) {
    let (name, msg) = match signal {
        libc::SIGABRT => ("SIGABRT", "SIGABRT"),
        libc::SIGALRM => ("SIGALRM", "alarm clock"),
        libc::SIGBUS => ("SIGBUS", "bus error"),
        libc::SIGCHLD => ("SIGCHLD", "child status changed"),
        libc::SIGCONT => ("SIGCONT", "continue after stop"),
        libc::SIGFPE => ("SIGFPE", "floating-point exception"),
        libc::SIGHUP => ("SIGHUP", "hangup"),
        libc::SIGILL => ("SIGILL", "illegal instruction"),
        libc::SIGINT => ("SIGINT", "interrupt"),
        libc::SIGKILL => ("SIGKILL", "kill signal"),
        libc::SIGPIPE => ("SIGPIPE", "write on pipe with no readers"),
        libc::SIGQUIT => ("SIGQUIT", "quit signal"),
        libc::SIGSEGV => ("SIGSEGV", "segmentation violation"),
        libc::SIGSTOP => ("SIGSTOP", "stop"),
        libc::SIGTERM => ("SIGTERM", "software termination signal"),
        libc::SIGTSTP => ("SIGTSTP", "stop signal from tty"),
        libc::SIGTTIN => ("SIGTTIN", "background tty read"),
        libc::SIGTTOU => ("SIGTTOU", "background tty write"),
        libc::SIGUSR1 => ("SIGUSR1", "user-defined signal 1"),
        libc::SIGUSR2 => ("SIGUSR2", "user-defined signal 2"),
        libc::SIGPROF => ("SIGPROF", "profiling alarm"),
        libc::SIGTRAP => ("SIGTRAP", "trace trap"),
        libc::SIGURG => ("SIGURG", "urgent I/O condition"),
        libc::SIGVTALRM => ("SIGVTALRM", "virtual time alarm"),
        libc::SIGXCPU => ("SIGXCPU", "exceeded CPU time limit"),
        libc::SIGXFSZ => ("SIGXFSZ", "exceeded file size limit"),
        libc::SIGWINCH => ("SIGWINCH", "window size changes"),
        other => return (format!("{other}"), format!("unknown signal {other}")),
    };
    (name.to_string(), msg.to_string())
}

/// Run `words` as `exec` does and answer the command's value.
fn exec(words: &[String], out: &Output) -> Result<String, TclError> {
    let plan = parse(words)?;
    crate::cmd_channel::flush_all();

    // Standard input of the first stage.
    let mut stdin_writer: Option<(File, Vec<u8>)> = None;
    let first_stdin = match plan.stdin {
        Source::Inherit => Stdio::inherit(),
        Source::File(f) => Stdio::from(f),
        Source::Text(bytes) => {
            let (read, write) = pipe()?;
            stdin_writer = Some((write, bytes));
            Stdio::from(read)
        }
    };

    // The pipe standard output is captured through, and the one standard error
    // is: both are read concurrently, because either filling would stall a
    // child that the other is waiting on.
    let capture_out = matches!(plan.stdout, Sink::Capture) && !plan.background;
    let collect_channel_out = matches!(plan.stdout, Sink::Channel(_));
    let (out_read, out_write) = if capture_out || collect_channel_out {
        let (r, w) = pipe()?;
        (Some(r), Some(w))
    } else {
        (None, None)
    };
    let capture_err = matches!(plan.stderr, Sink::Capture) && !plan.background;
    let collect_channel_err = matches!(plan.stderr, Sink::Channel(_));
    let (err_read, err_write) = if capture_err || collect_channel_err {
        let (r, w) = pipe()?;
        (Some(r), Some(w))
    } else {
        (None, None)
    };

    let dup = |file: &File| -> Result<File, TclError> {
        file.try_clone().map_err(|e| {
            TclError::plain(format!("couldn't duplicate descriptor: {}", io_message(&e)))
        })
    };
    let mut children: Vec<Child> = Vec::new();
    let mut previous_out: Option<File> = None;
    let last = plan.stages.len() - 1;
    let mut first_in = Some(first_stdin);
    let mut spawn_error: Option<TclError> = None;
    let mut stage_stdout_for_err: Option<File> = None;

    for (n, stage) in plan.stages.iter().enumerate() {
        let mut cmd = Command::new(&stage.argv[0]);
        cmd.args(&stage.argv[1..]);
        // Input: the previous stage's pipe, or the pipeline's own source.
        match (n, previous_out.take()) {
            (0, _) => {
                cmd.stdin(first_in.take().unwrap_or_else(Stdio::inherit));
            }
            (_, Some(pipe_read)) => {
                cmd.stdin(Stdio::from(pipe_read));
            }
            (_, None) => {
                cmd.stdin(Stdio::null());
            }
        }
        // Output.
        let mut merged_to_pipe: Option<File> = None;
        if n == last {
            match &plan.stdout {
                Sink::Capture if capture_out => {
                    let w = out_write.as_ref().expect("capture pipe");
                    cmd.stdout(Stdio::from(dup(w)?));
                    stage_stdout_for_err = Some(dup(w)?);
                }
                Sink::Channel(_) => {
                    let w = out_write.as_ref().expect("channel pipe");
                    cmd.stdout(Stdio::from(dup(w)?));
                    stage_stdout_for_err = Some(dup(w)?);
                }
                Sink::File(f) => {
                    cmd.stdout(Stdio::from(dup(f)?));
                    stage_stdout_for_err = Some(dup(f)?);
                }
                _ => {
                    cmd.stdout(Stdio::inherit());
                }
            }
        } else {
            let (r, w) = pipe()?;
            if stage.stderr_to_pipe {
                merged_to_pipe = Some(dup(&w)?);
            }
            cmd.stdout(Stdio::from(w));
            previous_out = Some(r);
        }
        // Error.
        if let Some(w) = merged_to_pipe {
            cmd.stderr(Stdio::from(w));
        } else {
            match &plan.stderr {
                Sink::Capture if capture_err => {
                    let w = err_write.as_ref().expect("capture pipe");
                    cmd.stderr(Stdio::from(dup(w)?));
                }
                Sink::Channel(_) => {
                    let w = err_write.as_ref().expect("channel pipe");
                    cmd.stderr(Stdio::from(dup(w)?));
                }
                Sink::File(f) => {
                    cmd.stderr(Stdio::from(dup(f)?));
                }
                Sink::ToStdout => match &stage_stdout_for_err {
                    Some(w) => {
                        cmd.stderr(Stdio::from(dup(w)?));
                    }
                    None => {
                        cmd.stderr(Stdio::inherit());
                    }
                },
                Sink::Inherit | Sink::Capture => {
                    cmd.stderr(Stdio::inherit());
                }
            }
        }
        match cmd.spawn() {
            Ok(child) => children.push(child),
            Err(e) => {
                spawn_error = Some(TclError::plain(format!(
                    "couldn't execute \"{}\": {}",
                    stage.argv[0],
                    io_message(&e)
                )));
                break;
            }
        }
    }
    // The parent's copies of the write ends must go, or the readers never see
    // end of file.
    drop(out_write);
    drop(err_write);
    drop(stage_stdout_for_err);
    drop(previous_out);
    drop(first_in);

    if let Some(e) = spawn_error {
        for child in &mut children {
            let _ = child.kill();
            let _ = child.wait();
        }
        return Err(e);
    }

    if plan.background {
        let pids: Vec<String> = children.iter().map(|c| c.id().to_string()).collect();
        // Detached: nothing waits for them.
        std::mem::forget(children);
        return Ok(pids.join(" "));
    }

    let writer = stdin_writer.map(|(mut pipe, bytes)| {
        std::thread::spawn(move || {
            let _ = pipe.write_all(&bytes);
        })
    });
    let reader = |source: Option<File>| {
        source.map(|mut f| {
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let _ = f.read_to_end(&mut bytes);
                bytes
            })
        })
    };
    let out_thread = reader(out_read);
    let err_thread = reader(err_read);

    // Statuses in pipeline order.
    let mut statuses = Vec::with_capacity(children.len());
    for child in &mut children {
        let pid = child.id();
        match child.wait() {
            Ok(status) => statuses.push((pid, status)),
            Err(e) => {
                return Err(TclError::plain(format!(
                    "error waiting for process to exit: {}",
                    io_message(&e)
                )))
            }
        }
    }
    if let Some(w) = writer {
        let _ = w.join();
    }
    let stdout_bytes = out_thread
        .map(|t| t.join().unwrap_or_default())
        .unwrap_or_default();
    let stderr_bytes = err_thread
        .map(|t| t.join().unwrap_or_default())
        .unwrap_or_default();
    let mut result = String::from_utf8_lossy(&stdout_bytes).into_owned();
    let stderr_text = String::from_utf8_lossy(&stderr_bytes).into_owned();

    // Redirections to a channel: written once the pipeline has finished.
    if let Sink::Channel(name) = &plan.stdout {
        write_channel(name, &result, out)?;
        result.clear();
    }
    let mut error_text = String::new();
    if let Sink::Channel(name) = &plan.stderr {
        write_channel(name, &stderr_text, out)?;
    } else {
        error_text = stderr_text;
    }

    use std::os::unix::process::ExitStatusExt;
    let mut errorcode: Option<String> = None;
    let mut message: Option<String> = None;
    for (pid, status) in &statuses {
        if let Some(signal) = status.signal() {
            let (name, text) = signal_names(signal);
            errorcode = Some(crate::list::join(&[
                "CHILDKILLED".to_string(),
                pid.to_string(),
                name,
                text.clone(),
            ]));
            message = Some(format!("child killed: {text}"));
        } else if let Some(code) = status.code() {
            if code != 0 {
                errorcode = Some(format!("CHILDSTATUS {pid} {code}"));
                message = Some("child process exited abnormally".to_string());
            }
        }
    }

    if !plan.keep_newline {
        if result.ends_with('\n') {
            result.pop();
        }
        if error_text.ends_with('\n') {
            error_text.pop();
        }
    }

    let had_stderr = !error_text.is_empty();
    if had_stderr || message.is_some() {
        // Standard error ends the result, which is what makes the message of a
        // failing command the text it printed; the generic wording is only for
        // a child that said nothing.
        let mut text = result;
        if had_stderr {
            if !text.is_empty() && !text.ends_with('\n') && !plan.keep_newline {
                text.push('\n');
            }
            text.push_str(&error_text);
        } else if let Some(m) = &message {
            text = m.clone();
        }
        let mut e = TclError::plain(text);
        e.errorcode = Some(errorcode.unwrap_or_else(|| "NONE".to_string()));
        return Err(e);
    }
    Ok(result)
}

/// Write what a redirection collected to the channel it names.
fn write_channel(name: &str, text: &str, out: &Output) -> Result<(), TclError> {
    match name {
        "stdout" => {
            out.write(text);
            Ok(())
        }
        "stderr" => {
            let _ = std::io::stderr().write_all(text.as_bytes());
            Ok(())
        }
        other => {
            let id = crate::cmd_channel::resolve_writable(other).map_err(TclError::plain)?;
            crate::cmd_channel::write_id(id, text, Some(out)).map_err(TclError::plain)
        }
    }
}

// ── tailcall ─────────────────────────────────────────────────────────────

/// `tailcall command ?arg …?` (`TclNRTailcallObjCmd`).
///
/// Only a procedure body (or a lambda's, which is compiled as one) has an
/// activation to replace. Elsewhere it raises when it runs, as tclsh does.
fn compile_tailcall(c: &mut Compiler, args: &[Word]) -> Result<(), CompileError> {
    if c.scope.is_none() {
        return Err(c.deferrable_err("tailcall can only be called from a proc, lambda or method"));
    }
    // No command: nothing to call, and the procedure carries on.
    if args.is_empty() {
        c.push_empty();
        return Ok(());
    }
    let Ok(argc) = u8::try_from(args.len() + 1) else {
        return c.error("too many arguments for \"tailcall\"");
    };
    // The body's `global` declarations ride in front, for a builtin that has to
    // run against the activation (`TAILCALL_CALL`).
    let declared = c.declared_globals().unwrap_or_default();
    c.push_str(&declared);
    // The name is resolved where the call is written, so an unqualified name
    // inside a namespace reaches that namespace's procedure.
    match args[0].as_literal().and_then(|n| c.ns_resolves(n)) {
        Some(key) => c.push_str(&key),
        None => c.word(&args[0])?,
    }
    for w in &args[1..] {
        c.word(w)?;
    }
    // Inside a `catch` or `try` the unwind has to go through the region's
    // handler, which a replaced frame would skip: the call is made from the
    // activation and its value raised as the activation's `return`.
    if c.catch_depth > 0 {
        c.emit(Op::Extended(ext::TAILCALL_CALL, argc), 1 - i32::from(argc));
        c.emit(Op::LoadInt(0), 1);
        c.emit(Op::LoadInt(1), 1);
        c.emit(Op::Extended(crate::compiler::ext::RAISE, 0), -3);
        c.push_empty();
        return Ok(());
    }
    // Every loop this call leaves has a region open at run time, and its
    // `LOOP_LEAVE` is never reached — the same bookkeeping `return` does.
    for _ in 0..c.loops.len() {
        c.emit(Op::Extended(crate::compiler::ext::LOOP_LEAVE, 0), 0);
    }
    c.emit(Op::Extended(ext::TAILCALL, argc), 1 - i32::from(argc));
    c.emit(Op::ReturnValue, -1);
    c.push_empty();
    Ok(())
}
