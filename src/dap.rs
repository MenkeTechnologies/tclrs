//! `tclrs --dap` — a Debug Adapter Protocol server (stdio) for Tcl.
//!
//! Stopping granularity is one command: the compiler emits a
//! [`ext_wide::DBG_LINE`] marker before each one when a script is lowered by
//! [`crate::compiler::compile_debug`], and the marker's handler calls
//! [`at_line`], which stops when a breakpoint matches the line, when the client
//! is stepping, or when a pause was asked for. Markers are emitted inside
//! procedure bodies as well as at the top level, so a breakpoint works below
//! the top level and stepping walks into a procedure.
//!
//! The debuggee is not a separate process, and there is no second thread: the
//! run happens on this thread and, when a marker stops it, requests are served
//! from inside the stop ([`Session::wait`]) until the client resumes. That is
//! what makes `scopes` and `variables` read the live VM rather than a copy —
//! the paused VM is the one being asked. The cost is that an asynchronous
//! `pause` is honored at the next command rather than in the middle of one.
//!
//! Everything the debuggee prints is redirected while the run is in progress
//! and re-emitted as `output` events, because stdout is the protocol here; DAP
//! JSON goes to the terminal's own stdout, saved before the redirect.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};

use fusevm::{Value, VM};
use serde_json::{json, Value as Json};

use crate::runtime::to_tcl_string;

thread_local! {
    /// The session, for the length of a run. `None` everywhere else, which is
    /// what makes [`at_line`] free when no debugger is attached.
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
}

/// Serve a debug session on stdin/stdout. The return is the process's exit
/// status.
pub fn run_stdio() -> i32 {
    let session = match Session::new() {
        Ok(session) => session,
        Err(e) => {
            eprintln!("tclrs --dap: {e}");
            return 1;
        }
    };
    SESSION.with(|s| *s.borrow_mut() = Some(session));

    // Requests are served until the client has both launched a program and
    // finished configuring, at which point the program runs on this thread.
    // Nothing holds the session borrow across the run, so `at_line` can take it.
    while let Some(request) = with_session(|s| s.read()).flatten() {
        let start = with_session(|s| s.dispatch(&request)).unwrap_or(false);
        if with_session(|s| s.finished).unwrap_or(true) {
            break;
        }
        if start {
            run_program();
            with_session(|s| {
                s.capture.finish(&mut s.out);
                s.event("terminated", json!({}));
                s.event("exited", json!({"exitCode": 0}));
            });
            break;
        }
    }

    SESSION.with(|s| *s.borrow_mut() = None);
    0
}

/// Called from the VM at every command marker of a debug-compiled chunk.
pub fn at_line(vm: &mut VM, line: usize) {
    with_session(|s| s.reached(vm, line as u32));
}

fn with_session<R>(f: impl FnOnce(&mut Session) -> R) -> Option<R> {
    SESSION.with(|s| s.borrow_mut().as_mut().map(f))
}

/// Read, compile with markers, and run the launched program.
fn run_program() {
    let Some(path) = with_session(|s| s.program.clone()).flatten() else {
        with_session(|s| s.stderr("no program to launch\n"));
        return;
    };
    let src = match std::fs::read_to_string(&path) {
        Ok(src) => src,
        Err(e) => {
            with_session(|s| s.stderr(&format!("couldn't read file \"{path}\": {e}\n")));
            return;
        }
    };
    let compiled = crate::parse(&crate::rust_ffi::desugar(&src))
        .map_err(|e| e.to_string())
        .and_then(|script| crate::compiler::compile_debug(&script).map_err(|e| e.to_string()));
    let chunk = match compiled {
        Ok(chunk) => chunk,
        Err(e) => {
            with_session(|s| s.stderr(&format!("{e}\n")));
            return;
        }
    };
    let mut interp = crate::Interp::new();
    if let Err(e) = interp.run_chunk(chunk) {
        with_session(|s| s.stderr(&format!("{}\n", e.msg)));
    }
}

/// What a stopped run was told to do next.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Resume {
    /// Run until the next breakpoint.
    Continue,
    /// Stop again at the next command.
    Step,
}

pub(crate) struct Session {
    reader: BufReader<std::io::Stdin>,
    /// The terminal's stdout, saved before the debuggee's was redirected.
    out: std::fs::File,
    capture: Capture,
    seq: i64,
    /// Breakpoint lines, by the source path the client named them for.
    breakpoints: HashMap<String, HashSet<u32>>,
    program: Option<String>,
    configured: bool,
    launched: bool,
    stop_at_entry: bool,
    /// Stop at the next command, whatever the breakpoints say.
    stepping: bool,
    /// The line of the command a stopped run is about to execute.
    line: u32,
    /// The variables of the stopped VM, as name/value pairs.
    variables: Vec<(String, String)>,
    finished: bool,
}

impl Session {
    fn new() -> std::io::Result<Session> {
        let capture = Capture::start()?;
        Ok(Session {
            reader: BufReader::new(std::io::stdin()),
            out: capture.saved_stdout()?,
            capture,
            seq: 0,
            breakpoints: HashMap::new(),
            program: None,
            configured: false,
            launched: false,
            stop_at_entry: false,
            stepping: false,
            line: 0,
            variables: Vec::new(),
            finished: false,
        })
    }

    // ── the wire ─────────────────────────────────────────────────────────

    /// One `Content-Length`-framed message, or `None` at end of input.
    fn read(&mut self) -> Option<Json> {
        let mut length = 0;
        loop {
            let mut header = String::new();
            if self.reader.read_line(&mut header).ok()? == 0 {
                return None;
            }
            if header == "\r\n" || header == "\n" {
                break;
            }
            if let Some(value) = header.trim().strip_prefix("Content-Length:") {
                length = value.trim().parse().ok()?;
            }
        }
        let mut body = vec![0; length];
        self.reader.read_exact(&mut body).ok()?;
        serde_json::from_slice(&body).ok()
    }

    fn write(&mut self, message: &Json) {
        let body = message.to_string();
        let _ = write!(self.out, "Content-Length: {}\r\n\r\n{body}", body.len());
        let _ = self.out.flush();
    }

    fn event(&mut self, event: &str, body: Json) {
        self.seq += 1;
        let seq = self.seq;
        self.write(&json!({"seq": seq, "type": "event", "event": event, "body": body}));
    }

    fn respond(&mut self, request: &Json, body: Json) {
        self.seq += 1;
        let seq = self.seq;
        self.write(&json!({
            "seq": seq,
            "type": "response",
            "request_seq": request["seq"],
            "success": true,
            "command": request["command"],
            "body": body,
        }));
    }

    fn stderr(&mut self, text: &str) {
        self.event("output", json!({"category": "stderr", "output": text}));
    }

    // ── requests ─────────────────────────────────────────────────────────

    /// Answer one request. True when it is time to run the program: both
    /// `launch` and `configurationDone` have arrived, in whichever order this
    /// client sends them.
    fn dispatch(&mut self, request: &Json) -> bool {
        match request["command"].as_str().unwrap_or_default() {
            "initialize" => {
                self.respond(
                    request,
                    json!({
                        "supportsConfigurationDoneRequest": true,
                        "supportsTerminateRequest": true,
                        "supportsEvaluateForHovers": false,
                    }),
                );
                self.event("initialized", json!({}));
            }
            "setBreakpoints" => self.set_breakpoints(request),
            "setExceptionBreakpoints" => self.respond(request, json!({})),
            "launch" => {
                self.program = request["arguments"]["program"].as_str().map(str::to_string);
                self.stop_at_entry = request["arguments"]["stopOnEntry"]
                    .as_bool()
                    .unwrap_or(false);
                self.stepping = self.stop_at_entry;
                self.launched = true;
                self.respond(request, json!({}));
            }
            "configurationDone" => {
                self.configured = true;
                self.respond(request, json!({}));
            }
            "threads" => self.respond(request, json!({"threads": [{"id": 1, "name": "tclrs"}]})),
            "disconnect" | "terminate" => {
                self.respond(request, json!({}));
                self.finished = true;
            }
            // Everything else is only meaningful while stopped; answering it
            // here keeps a client from waiting on a response that never comes.
            other => self.respond(request, json!({"unsupported": other})),
        }
        self.launched && self.configured && !self.finished
    }

    fn set_breakpoints(&mut self, request: &Json) {
        let path = request["arguments"]["source"]["path"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let asked = request["arguments"]["breakpoints"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let lines: HashSet<u32> = asked
            .iter()
            .filter_map(|b| b["line"].as_u64())
            .map(|line| line as u32)
            .collect();
        let verified: Vec<Json> = asked
            .iter()
            .filter_map(|b| b["line"].as_u64())
            .map(|line| json!({"verified": true, "line": line}))
            .collect();
        self.breakpoints.insert(canonical(&path), lines);
        self.respond(request, json!({"breakpoints": verified}));
    }

    // ── stopping ─────────────────────────────────────────────────────────

    /// A command is about to run. Stop when something asked for it.
    fn reached(&mut self, vm: &mut VM, line: u32) {
        if self.finished {
            return;
        }
        let breakpoint = self
            .program
            .as_deref()
            .map(canonical)
            .and_then(|path| self.breakpoints.get(&path).cloned())
            .is_some_and(|lines| lines.contains(&line));
        if !breakpoint && !self.stepping {
            return;
        }
        self.line = line;
        self.variables = visible(vm);
        let reason = if breakpoint { "breakpoint" } else { "step" };
        self.event(
            "stopped",
            json!({"reason": reason, "threadId": 1, "allThreadsStopped": true}),
        );
        // Program output produced up to the stop belongs before it.
        self.capture.drain(&mut self.out, &mut self.seq);
        self.wait();
    }

    /// Serve requests from inside a stop, until the client resumes the run.
    fn wait(&mut self) {
        while let Some(request) = self.read() {
            let resume = match request["command"].as_str().unwrap_or_default() {
                "continue" => {
                    self.respond(&request, json!({"allThreadsContinued": true}));
                    Some(Resume::Continue)
                }
                // Frame-depth-aware stepping is not modelled: `stepIn` and
                // `next` both stop at the next command, and `stepOut` runs on
                // to the next breakpoint.
                "next" | "stepIn" => {
                    self.respond(&request, json!({}));
                    Some(Resume::Step)
                }
                "stepOut" => {
                    self.respond(&request, json!({}));
                    Some(Resume::Continue)
                }
                "stackTrace" => {
                    let frame = json!({
                        "id": 1,
                        "name": "script",
                        "line": self.line,
                        "column": 1,
                        "source": {"path": self.program.clone().unwrap_or_default()},
                    });
                    self.respond(&request, json!({"stackFrames": [frame], "totalFrames": 1}));
                    None
                }
                "scopes" => {
                    let scope = json!({
                        "name": "Variables",
                        "variablesReference": 1,
                        "expensive": false,
                    });
                    self.respond(&request, json!({"scopes": [scope]}));
                    None
                }
                "variables" => {
                    let variables: Vec<Json> = self
                        .variables
                        .iter()
                        .map(|(name, value)| {
                            json!({"name": name, "value": value, "variablesReference": 0})
                        })
                        .collect();
                    self.respond(&request, json!({"variables": variables}));
                    None
                }
                "threads" => {
                    self.respond(&request, json!({"threads": [{"id": 1, "name": "tclrs"}]}));
                    None
                }
                "setBreakpoints" => {
                    self.set_breakpoints(&request);
                    None
                }
                "disconnect" | "terminate" => {
                    self.respond(&request, json!({}));
                    self.finished = true;
                    Some(Resume::Continue)
                }
                other => {
                    self.respond(&request, json!({"unsupported": other}));
                    None
                }
            };
            match resume {
                Some(Resume::Continue) => {
                    self.stepping = false;
                    return;
                }
                Some(Resume::Step) => {
                    self.stepping = true;
                    return;
                }
                None => {}
            }
        }
        // End of input while stopped: the client is gone, so stop stopping.
        self.finished = true;
        self.stepping = false;
    }
}

/// The variables of a stopped VM, in the chunk's own order.
fn visible(vm: &mut VM) -> Vec<(String, String)> {
    vm.chunk
        .names
        .iter()
        .enumerate()
        // The compiler's own loop state is named with a leading NUL, and is not
        // anything the script wrote.
        .filter(|(_, name)| !name.starts_with('\u{0}'))
        .filter_map(|(slot, name)| {
            let value = vm.globals.get(slot)?;
            match value {
                Value::Undef => None,
                value => Some((name.clone(), to_tcl_string(value))),
            }
        })
        .collect()
}

/// A path as the breakpoints and the launch argument are compared: resolved
/// when the filesystem can, and left alone when it cannot.
fn canonical(path: &str) -> String {
    std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string())
}

// ── the debuggee's output ────────────────────────────────────────────────

/// stdout is the protocol, so the debuggee's own writes are redirected into a
/// pipe and re-emitted as `output` events. The terminal's stdout is duplicated
/// first, and that duplicate is what the adapter writes to.
struct Capture {
    /// The read end of the pipe the debuggee writes into.
    read: std::fs::File,
    /// The duplicated original stdout, handed to the session.
    saved: libc::c_int,
    restored: bool,
}

impl Capture {
    fn start() -> std::io::Result<Capture> {
        use std::os::fd::FromRawFd;
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: `pipe` fills two ints and reports failure through its return.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: dup/dup2 on a valid descriptor; failure is reported by -1.
        let saved = unsafe { libc::dup(libc::STDOUT_FILENO) };
        if saved < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if unsafe { libc::dup2(fds[1], libc::STDOUT_FILENO) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: the write end is now duplicated onto fd 1; this copy is done.
        unsafe { libc::close(fds[1]) };
        // SAFETY: `fds[0]` is an owned descriptor this struct takes over.
        let read = unsafe { std::fs::File::from_raw_fd(fds[0]) };
        set_nonblocking(fds[0])?;
        Ok(Capture {
            read,
            saved,
            restored: false,
        })
    }

    /// A `File` on the saved stdout, for the adapter's own writes.
    fn saved_stdout(&self) -> std::io::Result<std::fs::File> {
        use std::os::fd::FromRawFd;
        // SAFETY: `dup` returns a fresh owned descriptor, or -1.
        let fd = unsafe { libc::dup(self.saved) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `fd` is owned by the returned File.
        Ok(unsafe { std::fs::File::from_raw_fd(fd) })
    }

    /// Emit whatever the debuggee has printed so far. Non-blocking: nothing
    /// waiting means nothing to emit.
    fn drain(&mut self, out: &mut std::fs::File, seq: &mut i64) {
        let mut buffer = [0u8; 4096];
        loop {
            match self.read.read(&mut buffer) {
                Ok(0) => return,
                Ok(n) => {
                    let text = String::from_utf8_lossy(&buffer[..n]).into_owned();
                    *seq += 1;
                    let message = json!({
                        "seq": *seq,
                        "type": "event",
                        "event": "output",
                        "body": {"category": "stdout", "output": text},
                    })
                    .to_string();
                    let _ = write!(out, "Content-Length: {}\r\n\r\n{message}", message.len());
                    let _ = out.flush();
                }
                Err(_) => return,
            }
        }
    }

    /// Put the real stdout back and emit what is left in the pipe.
    fn finish(&mut self, out: &mut std::fs::File) {
        if self.restored {
            return;
        }
        // SAFETY: restoring fd 1 from the descriptor saved in `start`.
        unsafe {
            libc::dup2(self.saved, libc::STDOUT_FILENO);
        }
        self.restored = true;
        let mut seq = i64::MAX / 2;
        self.drain(out, &mut seq);
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        if !self.restored {
            // SAFETY: restoring fd 1, as `finish` would have.
            unsafe {
                libc::dup2(self.saved, libc::STDOUT_FILENO);
            }
        }
        // SAFETY: the saved descriptor is owned by this struct.
        unsafe {
            libc::close(self.saved);
        }
    }
}

fn set_nonblocking(fd: libc::c_int) -> std::io::Result<()> {
    // SAFETY: fcntl with F_GETFL/F_SETFL on an owned descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::compiler::ext_wide;
    use fusevm::Op;

    /// The markers are what a debugger stops at, and an ordinary compilation
    /// must not carry them — they would cost a dispatch per command in every
    /// script anyone runs.
    #[test]
    fn markers_are_emitted_only_for_a_debug_compilation() {
        let script = crate::parse("set x 1\nputs $x\n").expect("parses");
        let plain = crate::compiler::compile(&script).expect("compiles");
        let debug = crate::compiler::compile_debug(&script).expect("compiles");

        assert_eq!(markers(&plain), Vec::<usize>::new());
        assert_eq!(markers(&debug), vec![1, 2]);
    }

    /// A marker before every command of a procedure body too, which is what
    /// makes a breakpoint inside a procedure reachable.
    #[test]
    fn a_procedure_body_carries_markers() {
        let script = crate::parse("proc f {} {\n  set a 1\n  set b 2\n}\nf\n").expect("parses");
        let debug = crate::compiler::compile_debug(&script).expect("compiles");
        let lines = markers(&debug);
        assert!(lines.contains(&2), "{lines:?}");
        assert!(lines.contains(&3), "{lines:?}");
    }

    /// The instrumented chunk still runs, and runs the same: the marker's
    /// handler does nothing at all without a session.
    #[test]
    fn a_debug_chunk_runs_like_an_ordinary_one() {
        let script = crate::parse("set x 21\nputs [expr {$x * 2}]\n").expect("parses");
        let chunk = crate::compiler::compile_debug(&script).expect("compiles");
        let mut interp = crate::Interp::capturing();
        interp.run_chunk(chunk).expect("runs");
        assert_eq!(interp.take_output(), "42\n");
    }

    fn markers(chunk: &fusevm::Chunk) -> Vec<usize> {
        chunk
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::ExtendedWide(id, line) if *id == ext_wide::DBG_LINE => Some(*line),
                _ => None,
            })
            .collect()
    }
}
