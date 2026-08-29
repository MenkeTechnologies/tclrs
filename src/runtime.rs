//! Running a compiled chunk: the numeric hook and extension ops that give
//! fusevm Tcl's arithmetic, the driver that owns interpreter state, error
//! unwinding and coroutine switching, and Tcl's number formatting.
//!
//! Two hooks carry all of the language-specific behavior:
//!
//! * the **numeric hook** catches operands the VM cannot compute on natively —
//!   strings, mostly — and applies Tcl's rules: an operand that parses as a
//!   number is one, comparisons fall back to string order when it does not, and
//!   arithmetic on a non-number is an error. It also catches the one pair the
//!   VM *could* compute on but must not: an integer past 2^53 compared against
//!   a double, which Tcl orders exactly and a machine `f64` cannot;
//! * the **extension handler** implements the operators whose Tcl meaning
//!   differs from the VM's generic one: `/` and `%` floor toward negative
//!   infinity, `**` stays integral for integral operands.
//!
//! Everything else runs as native ops, so the arithmetic the JIT cares about
//! stays visible to it.
//!
//! Three layers sit on top of that, and they are one mechanism rather than
//! three:
//!
//! * an [`Interp`] owns the variables and holds them between evaluations, which
//!   is what makes a REPL a REPL and what lets the `eval` command run a script
//!   built at run time and see the same state the script that built it sees;
//! * a `Machine` drives one evaluation. A script that uses no coroutine is
//!   one VM run in a loop that only ever restarts it at a `catch` handler; a
//!   script that creates coroutines has one VM per context, and the same loop
//!   also services the requests their ops raise. Both paths share one
//!   mechanism: an op stashes something in a cell and halts, and the driver
//!   reads the cell after `run()` returns — the pattern fusevm's scheduler is
//!   built on;
//! * `Hooks::install` is the only place a hook is put on a VM, so the main
//!   VM, every coroutine's VM and every nested `eval`'s VM behave alike.
//!
//! The two ways of holding variables meet at `seed` and `flush`. Within one
//! evaluation every VM runs the same chunk, so the global table is a `Vec` the
//! driver moves into whichever VM is about to run. Across evaluations the chunk
//! differs — a chunk interns its own name table — so the interpreter keeps the
//! variables keyed by name and the vector is projected out of that map on entry
//! and read back into it on exit.
//!
//! The VM is asked for its highest tier: `Hooks::install` also calls
//! `enable_tracing_jit`, which makes `VM::run` consult fusevm's block JIT for a
//! wholly-eligible chunk and arm the trace recorder at every backward branch
//! otherwise. [`crate::tiers`] reports which of those tiers a given script
//! actually reaches — see the JIT section of the README for what that measures
//! on Tcl today.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use fusevm::{Chunk, Frame, NumOp, VMResult, Value, VM};
use num_bigint::BigInt;
use num_traits::{FromPrimitive, Signed, ToPrimitive, Zero};

use crate::cache::ChunkCache;
use crate::compiler::{ext, ext_wide, Place};
use crate::coro::{self, Request};
use crate::list;

/// The outcome of running a script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The value of the script's last command.
    pub result: String,
    /// Everything the script wrote to stdout.
    pub output: String,
}

/// Tcl's five standard return codes. Any other integer is legal too — `return
/// -code 42` — so a code travels as an `i32` rather than an enum.
pub(crate) const TCL_OK: i32 = 0;
pub(crate) const TCL_ERROR: i32 = 1;
pub(crate) const TCL_RETURN: i32 = 2;
pub(crate) const TCL_BREAK: i32 = 3;
pub(crate) const TCL_CONTINUE: i32 = 4;

/// A script that would not compile, or that finished with a non-`ok` return
/// code — which in Tcl is one mechanism, not two: `break`, `continue`, `return`
/// and an error all leave a command the same way, differing only in the code
/// they carry and in what is prepared to absorb it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TclError {
    /// The message, in the reference interpreter's wording. For a non-error
    /// code this is the *result* — `return 7` carries `7` — not a diagnostic.
    pub msg: String,
    /// The 1-based script line, when the failure was located while compiling.
    /// A failure raised by a running chunk carries no line, as the reference
    /// interpreter's does not either.
    pub line: Option<usize>,
    /// The return code: `TCL_ERROR` for an error, and one of the others for
    /// a `break`, a `continue` or a `return`.
    pub code: i32,
    /// The number of call levels still to unwind before `code` takes effect.
    /// `return -level 1 -code break` (which is what a bare `return -code break`
    /// means) is code `TCL_RETURN` to the procedure it is written in and code
    /// `TCL_BREAK` to that procedure's caller — the mechanism a script builds
    /// its own control structures out of. Zero for an error and for `break` /
    /// `continue`, which act where they are written.
    pub level: i32,
}

impl TclError {
    pub(crate) fn plain(msg: impl Into<String>) -> Self {
        TclError {
            msg: msg.into(),
            line: None,
            code: TCL_ERROR,
            level: 0,
        }
    }

    /// A non-error return code leaving a command: `break`, `continue`, or a
    /// `return` with its `-level` still to be spent.
    pub(crate) fn coded(code: i32, level: i32, msg: impl Into<String>) -> Self {
        TclError {
            msg: msg.into(),
            line: None,
            code,
            level,
        }
    }

    /// The code as whatever is about to absorb it sees. A `return` with levels
    /// left to unwind presents as [`TCL_RETURN`]; its own code applies only
    /// once the levels are spent.
    pub(crate) fn visible_code(&self) -> i32 {
        if self.level > 0 {
            TCL_RETURN
        } else {
            self.code
        }
    }

    /// Cross one procedure-call boundary: spend a level, and once none are
    /// left the carried code becomes the one the caller sees.
    pub(crate) fn descend(mut self) -> Self {
        if self.level > 0 {
            self.level -= 1;
        }
        self
    }

    /// Tcl's `-errorcode`-style option dictionary for `catch`'s options
    /// variable. Only the two options this frontend models are present.
    pub(crate) fn options(&self) -> String {
        format!("-code {} -level {}", self.code, self.level)
    }

    /// The inverse of [`TclError::options`]: rebuild the error a `catch`
    /// region's handler was resumed with, so that a handler acting as a
    /// `finally` can hand it back on unchanged ([`crate::compiler::ext::RERAISE`]).
    ///
    /// Beside `options` rather than anywhere else, because the two are one
    /// format: an option this frontend learns to carry has to be written and
    /// read in the same place or a re-raise silently drops it.
    pub(crate) fn from_options(options: &str, msg: String) -> Self {
        let mut error = TclError {
            msg,
            line: None,
            code: TCL_ERROR,
            level: 0,
        };
        let mut words = options.split_whitespace();
        while let (Some(key), Some(value)) = (words.next(), words.next()) {
            match key {
                "-code" => error.code = value.parse().unwrap_or(TCL_ERROR),
                "-level" => error.level = value.parse().unwrap_or(0),
                _ => {}
            }
        }
        error
    }

    /// The message a code that reached the outermost level is reported with.
    /// A `break` nothing absorbed is not "the script returned 3"; it is the
    /// reference interpreter's `invoked "break" outside of a loop`.
    pub(crate) fn escaped(self) -> Self {
        let word = match self.visible_code() {
            TCL_BREAK => "break",
            TCL_CONTINUE => "continue",
            _ => return self,
        };
        TclError {
            msg: format!("invoked \"{word}\" outside of a loop"),
            ..self
        }
    }
}

impl fmt::Display for TclError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(line) => write!(f, "{} (line {line})", self.msg),
            None => f.write_str(&self.msg),
        }
    }
}

impl std::error::Error for TclError {}

/// Parse and lower a script, with both failures reported the same way.
///
/// The interpreter reaches the same lowering through [`crate::cache`], which
/// keeps what it compiled; this is the entry for the callers that want the
/// chunk itself — the ahead-of-time compiler, the tier report, and `--disasm`.
pub fn compile(src: &str) -> Result<Chunk, String> {
    // As in [`crate::cache::ChunkCache::compile`]: an inline `rust { ... }`
    // block is rewritten into a command before the parser sees the script.
    let rewritten = crate::rust_ffi::desugar(src);
    let script = crate::parser::parse(&rewritten).map_err(|e| e.to_string())?;
    crate::compiler::compile(&script).map_err(|e| e.to_string())
}

/// Compile and run a script in a fresh interpreter, capturing its output.
///
/// A one-shot convenience over [`Interp`]: the state it builds is discarded
/// when it returns.
pub fn eval(src: &str) -> Result<Outcome, String> {
    let (result, output) = eval_captured(src);
    result.map(|result| Outcome { result, output })
}

/// Compile and run a script, reporting what it wrote even when it fails.
///
/// [`eval`] drops the output of a failing script, which is the convenient
/// shape for a caller that only wants the value. A conformance harness needs
/// both halves: a Tcl program that prints and then fails has an observable
/// outcome on stdout as well as an error, and comparing only the error would
/// let a divergence in the printed part go unnoticed.
pub fn eval_captured(src: &str) -> (Result<String, String>, String) {
    let mut interp = Interp::capturing();
    let result = interp.eval(src).map_err(|e| e.to_string());
    (result, interp.take_output())
}

// ── the interpreter ──────────────────────────────────────────────────────

/// How deep `eval` may nest before the interpreter refuses to go further —
/// `interp recursionlimit`'s default in the reference interpreter, and the same
/// message when it is reached.
///
/// A nested script runs on a VM of its own, so nesting costs native stack.
/// Refusing at a fixed depth turns what would be a stack overflow — a signal,
/// not an error — into a script error the script can be blamed for. A host
/// running on a small stack should lower it with
/// [`Interp::set_recursion_limit`]; [`RECOMMENDED_STACK`] is what this default
/// needs.
pub const DEFAULT_RECURSION_LIMIT: usize = 1000;

/// The thread stack [`DEFAULT_RECURSION_LIMIT`] levels of nesting need, with
/// room to spare for an unoptimized build. The `tclrs` binary runs on a thread
/// this size; a host embedding the library and keeping the default limit needs
/// as much.
pub const RECOMMENDED_STACK: usize = 256 * 1024 * 1024;

/// Where an interpreter's scripts write.
///
/// One of these per interpreter, cloned into every VM's sink, so a coroutine's
/// output and a nested `eval`'s output land in the same place and in the order
/// they were produced. The stdout form buffers: a script that prints in a loop
/// should not be measuring one syscall per line.
#[derive(Clone)]
pub(crate) enum Output {
    Capture(Arc<Mutex<String>>),
    Stdout(Arc<Mutex<std::io::BufWriter<std::io::Stdout>>>),
}

impl Output {
    fn stdout() -> Output {
        Output::Stdout(Arc::new(Mutex::new(std::io::BufWriter::new(
            std::io::stdout(),
        ))))
    }

    pub(crate) fn write(&self, s: &str) {
        match self {
            Output::Capture(buf) => buf.lock().expect("output lock").push_str(s),
            Output::Stdout(out) => {
                let _ = out.lock().expect("output lock").write_all(s.as_bytes());
            }
        }
    }

    /// Push what is buffered out to the operating system. Called at the end of
    /// every evaluation, so an error the caller prints afterwards cannot
    /// overtake the output of the script that raised it.
    pub(crate) fn flush(&self) {
        if let Output::Stdout(out) = self {
            let _ = out.lock().expect("output lock").flush();
        }
    }
}

/// Everything an interpreter carries between evaluations.
///
/// It lives behind an `Arc<Mutex<…>>` because a running chunk reaches back into
/// it: the `eval` command compiles and runs a nested script from inside the
/// extension handler of the chunk that invoked it. No lock is ever held across
/// a `VM::run`, so that nesting can go as deep as `limit` allows.
pub(crate) struct State {
    /// The variables, keyed by name. This is the authority, not the VM's slot
    /// vector — see `seed`. A namespace variable is one of these under its
    /// qualified name; `crate::cmd_namespace::store_key` is the spelling.
    pub(crate) globals: HashMap<String, Value>,
    /// Procedures whose name was bound while a script was running, which is
    /// every `proc` that is not at its script's top level. A name here answers
    /// from the moment the defining code ran and not before, which is what
    /// separates it from the procedures the compiler resolves — see
    /// [`crate::procs`]. Held on the interpreter rather than per evaluation, so
    /// a definition survives into the next `eval` the way a variable does.
    pub(crate) commands: HashMap<String, crate::procs::RuntimeProc>,
    /// The namespaces and commands the running script has created, which is
    /// what `namespace exists`, `children`, `which` and `origin` answer from.
    /// Per interpreter rather than per process, because two interpreters have
    /// separate namespaces. See `crate::cmd_namespace::Registry`.
    pub(crate) ns: crate::cmd_namespace::Registry,
    /// The chunks whose runs are in progress, outermost first.
    ///
    /// A procedure's body is an op index, which means nothing outside the chunk
    /// it was compiled into — so the chunk has to be recorded with it, and it
    /// has to be recorded as something a later call can *run*. The running one
    /// is only reachable from the VM as a `Chunk` by value, and copying one per
    /// `proc` would copy the whole program; this is where the `Arc` the chunk
    /// arrived as is kept so that [`crate::procs::define_op`] can take a handle
    /// to it instead. Pushed and popped by [`Machine::start`], so the last entry
    /// is the chunk of the innermost run — which is the one whose extension
    /// handler is executing.
    pub(crate) running: Vec<Arc<Chunk>>,
    /// Global names an `upvar` outside a procedure made one variable: the alias,
    /// and the name it stands for. Empty for almost every script, and one hash
    /// lookup per name at a chunk's entry and exit when it is not — see
    /// [`alias_global`].
    aliases: HashMap<String, String>,
    /// The frame projections in effect, outermost first — see [`Projection`].
    /// Empty except while a nested script is running against a procedure
    /// activation's variables.
    projections: Vec<Projection>,
    cache: ChunkCache,
    /// Where the scripts of this interpreter write.
    output: Output,
    /// How many scripts are running, counting the outermost.
    depth: usize,
    limit: usize,
    /// The contexts whose VMs are running, outermost first: the coroutine's
    /// name, or `None` for a script's own main context.
    ///
    /// A nested script runs a machine of its own, which cannot see the machine
    /// that started it. This is what lets a `yield` in an `eval`'d script tell
    /// that it is inside a coroutine, and say so, rather than report the
    /// reference interpreter's message for a `yield` that is in no coroutine.
    ///
    /// Parallel to [`Interpreter::running`], which is the same stack of runs seen
    /// as chunks rather than as coroutine contexts.
    contexts: Vec<Option<String>>,
    /// The `after` scripts this interpreter has registered and not yet run.
    /// Tcl keeps them on the interpreter too (`Tcl_SetAssocData(interp,
    /// "tclAfter", …)`, `generic/tclTimer.c:801-807`); see
    /// [`crate::cmd_after`].
    pub(crate) afters: crate::cmd_after::Afters,
}

/// One frame projection in progress: what a nested script running against a
/// procedure activation's variables needs that the variable table cannot say.
///
/// Two things live here, for two questions the script asks.
///
/// A `::`-qualified name is the *interpreter's* variable, not the frame's, so it
/// has to reach past the view the projection installed — [`State::root_global`]
/// walks these outermost-first to find the table the view displaced.
///
/// `info locals` asked inside the script has to answer with the frame's names.
/// The script is a chunk of its own, compiled at the script's own level, so its
/// compiler has no scope to list; the frame it is projected into is only known
/// here.
struct Projection {
    /// The variable table this projection displaced. The outermost one is the
    /// interpreter's own, which is what makes the walk in [`State::root_global`]
    /// terminate at the right table.
    outer: HashMap<String, Value>,
    /// The names the body declared `global`, which are in the view but are not
    /// locals of the frame. Everything else the view holds is, which is what
    /// [`State::frame_locals`] answers `info locals` with — read from the view
    /// as it stands rather than from a snapshot, so a name the script itself
    /// creates is listed the moment it exists.
    declared: Vec<String>,
}

pub(crate) type Shared = Arc<Mutex<State>>;

/// A Tcl interpreter: the variables of a session, and the chunks compiled for
/// it.
///
/// Every evaluation runs against the same state, so a variable set by one
/// survives into the next. That is what a REPL needs, and what the `eval`
/// command needs, and the two are the same mechanism.
pub struct Interp {
    shared: Shared,
}

impl Interp {
    /// An interpreter whose scripts write to the process's stdout.
    pub fn new() -> Self {
        Interp::with_output(Output::stdout())
    }

    /// An interpreter that collects what its scripts write, for
    /// [`Interp::take_output`].
    pub fn capturing() -> Self {
        Interp::with_output(Output::Capture(Arc::new(Mutex::new(String::new()))))
    }

    fn with_output(output: Output) -> Self {
        Interp {
            shared: Arc::new(Mutex::new(State {
                globals: HashMap::new(),
                commands: HashMap::new(),
                ns: crate::cmd_namespace::Registry::default(),
                running: Vec::new(),
                aliases: HashMap::new(),
                projections: Vec::new(),
                cache: ChunkCache::new(),
                output,
                depth: 0,
                limit: DEFAULT_RECURSION_LIMIT,
                contexts: Vec::new(),
                afters: crate::cmd_after::Afters::default(),
            })),
        }
    }

    /// How deep `eval` may nest. Lower it when the interpreter runs on a stack
    /// smaller than [`RECOMMENDED_STACK`], because the depth is the only thing
    /// standing between a runaway script and a stack overflow.
    pub fn set_recursion_limit(&mut self, limit: usize) {
        self.lock().limit = limit.max(1);
    }

    /// Finish a run at the outermost level, where a return code has nowhere left
    /// to go.
    ///
    /// The script itself is a level, so a `return` that reached here spends its
    /// last one — and a `return` whose code is then `ok` is not a failure at all:
    /// the script finished with that result. A `break` or a `continue` nothing
    /// absorbed is reported the way the reference interpreter reports it, by what
    /// it was rather than by the number it carried.
    fn outermost(outcome: Result<Value, TclError>) -> Result<String, TclError> {
        match outcome {
            Ok(v) => Ok(to_tcl_string(&v)),
            Err(e) => {
                let e = e.descend();
                if e.visible_code() == TCL_OK {
                    Ok(e.msg)
                } else {
                    Err(e.escaped())
                }
            }
        }
    }

    /// Compile and run a script, returning the value of its last command.
    pub fn eval(&mut self, src: &str) -> Result<String, TclError> {
        Self::outermost(run_source(&self.shared, src))
    }

    /// Run a chunk this interpreter did not compile, against its variables.
    ///
    /// [`Interp::eval`] is the ordinary way in, and it compiles through the
    /// cache. This is for a caller holding a chunk that was lowered
    /// differently — the debug adapter runs
    /// [`crate::compiler::compile_debug`]'s output, which is the same script
    /// with a line marker before every command.
    pub fn run_chunk(&mut self, chunk: fusevm::Chunk) -> Result<String, TclError> {
        Self::outermost(Machine::run(&self.shared, Arc::new(chunk)))
    }

    /// Set a variable from the host — how the binary supplies `argv0`, `argc`
    /// and `argv`.
    pub fn set_global(&mut self, name: &str, value: impl Into<String>) {
        let value = Value::Str(Arc::new(value.into()));
        self.lock().globals.insert(name.to_string(), value);
    }

    /// Read a variable's string form, or `None` when it is not set.
    pub fn global(&self, name: &str) -> Option<String> {
        self.lock().globals.get(name).map(to_tcl_string)
    }

    /// Every variable this interpreter holds, sorted. The REPL completes `$`
    /// from it; nothing about evaluation reads it. An array is one variable
    /// here, under its own name — its elements are inside its value, and
    /// `array names` is what lists those.
    pub fn global_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.lock().globals.keys().cloned().collect();
        names.sort();
        names
    }

    /// The handle a foreign caller re-enters this interpreter through.
    ///
    /// [`Interp`] takes `&mut self` to evaluate, which a C callback re-entering
    /// through a stub table cannot produce: the same interpreter may already be
    /// part-way through an evaluation further down the stack. The state behind
    /// it is an `Arc<Mutex<…>>` and no lock is held while a script runs, which
    /// is what makes that re-entry sound — the `eval` command relies on the
    /// same property. Handing out the handle is how `crate::tk::interp` keeps
    /// one interpreter per `Tcl_Interp *` without owning it.
    #[cfg(feature = "tk")]
    pub(crate) fn into_shared(self) -> Shared {
        self.shared
    }

    /// The same handle, without giving up ownership: a Tk session hands it to
    /// the host it builds and then goes on running scripts through this
    /// interpreter, which is the point — the two are one interpreter.
    #[cfg(feature = "tk")]
    pub(crate) fn shared_handle(&self) -> Shared {
        Arc::clone(&self.shared)
    }

    /// Take everything captured so far, leaving the buffer empty. Always empty
    /// for an interpreter built by [`Interp::new`], which does not capture.
    pub fn take_output(&mut self) -> String {
        match &self.lock().output {
            Output::Capture(buf) => std::mem::take(&mut buf.lock().expect("output lock")),
            Output::Stdout(_) => String::new(),
        }
    }

    /// `(hits, misses)` from the chunk cache — one miss per compilation.
    pub fn cache_stats(&self) -> (u64, u64) {
        self.lock().cache.stats()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.shared.lock().expect("interpreter lock")
    }
}

impl Default for Interp {
    fn default() -> Self {
        Interp::new()
    }
}

/// Compile `src` — reusing the cached chunk when the same text has been
/// evaluated before — and run it against `shared`.
pub(crate) fn run_source(shared: &Shared, src: &str) -> Result<Value, TclError> {
    let compiled = {
        let mut state = shared.lock().expect("interpreter lock");
        // The limit counts nested evaluations, so the outermost script — the
        // one that is not nested in anything — does not spend a level, as it
        // does not in the reference interpreter.
        if state.depth > state.limit {
            return Err(TclError::plain(
                "too many nested evaluations (infinite loop?)",
            ));
        }
        state.depth += 1;
        // A script running inside a frame projection lowers a `::`-qualified
        // name apart from a bare one, because there the two are different
        // variables. See [`crate::compiler::Compiler::projected`].
        let projected = state.projected();
        state.cache.compile_in(src, projected)
    };
    // The depth is given back however this returns, including the compile
    // failure above, which is why it is not a `?` in the block.
    //
    // The cached `Arc` is handed over rather than cloned out of: `VM::new` takes
    // a chunk by value and copies it either way, and keeping the handle is what
    // lets a `proc` this run defines record the chunk it belongs to without a
    // second copy of the program. See [`State::running`].
    let result = match compiled {
        Ok(chunk) => Machine::run(shared, chunk),
        // The whole script would not parse. The reference interpreter never
        // saw the bad command: it parses one command, runs it, and parses the
        // next, so everything before the failure has already run and produced
        // its output — and if one of those commands failed, THAT is the error
        // reported, not the syntax error further down. See `run_prefix`.
        Err(e) => run_prefix(shared, src, e),
    };
    shared.lock().expect("interpreter lock").depth -= 1;
    result
}

/// Run the commands a script's failed parse left intact, then report the
/// failure — `Tcl_EvalEx`'s command-at-a-time model, reached only when the
/// whole-script parse said no.
///
/// `puts hi` followed by `puts {` writes `hi` and then reports
/// `missing close-brace` at the line the failing command STARTS on. A lowering
/// failure (the parse succeeded, the compile did not) has no prefix and is
/// returned unchanged.
fn run_prefix(shared: &Shared, src: &str, err: TclError) -> Result<Value, TclError> {
    let Some((end, line, _)) = crate::parser::valid_prefix(src) else {
        return Err(err);
    };
    let err = TclError { line: Some(line), ..err };
    if end == 0 {
        return Err(err);
    }
    let compiled = {
        let mut state = shared.lock().expect("interpreter lock");
        let projected = state.projected();
        state.cache.compile_in(&src[..end], projected)
    };
    // The prefix parsed, so it can only fail while running — and a command
    // that failed did so BEFORE the text the syntax error is in was reached,
    // which is the error the reference interpreter reports.
    match compiled.and_then(|chunk| Machine::run(shared, chunk)) {
        Ok(_) => Err(err),
        Err(e) => Err(e),
    }
}

/// Enter a procedure body compiled into `chunk`, which is not the chunk the
/// caller is running.
///
/// A procedure is an op index into one chunk's op stream, so a call from
/// anywhere else cannot be a jump: the body runs on a VM of its own over the
/// chunk it was compiled into, positioned the way [`Machine::create`] positions
/// a coroutine's. The interpreter's variables are the same ones either way —
/// every chunk projects them through [`seed`] and writes them back through
/// [`flush`] — which is what makes the two runs one interpreter.
///
/// Counted against the recursion limit for the same reason [`run_source`] is: a
/// procedure that calls itself across a chunk boundary spends native stack per
/// call, and the limit is what turns a runaway one into a Tcl error rather than
/// a signal.
pub(crate) fn call_in_chunk(
    shared: &Shared,
    chunk: &Arc<Chunk>,
    entry: usize,
    actuals: Vec<Value>,
) -> Result<Value, TclError> {
    {
        let mut state = shared.lock().expect("interpreter lock");
        if state.depth > state.limit {
            return Err(TclError::plain(
                "too many nested evaluations (infinite loop?)",
            ));
        }
        state.depth += 1;
    }
    let result = Machine::start(shared, Arc::clone(chunk), Some((entry, actuals)));
    shared.lock().expect("interpreter lock").depth -= 1;
    // This is a procedure-call boundary, and a `return` spends one level
    // crossing it: `return -code break` written in a procedure is code 2 to
    // the procedure and code 3 to whoever called it. A `return` whose levels
    // are then spent and whose code is `ok` is the procedure's ordinary
    // result — which is what `catch {return $x}` in a procedure body leaves.
    match result {
        Err(e) if e.level > 0 => {
            let e = e.descend();
            if e.visible_code() == TCL_OK {
                Ok(Value::Str(Arc::new(e.msg)))
            } else {
                Err(e)
            }
        }
        other => other,
    }
}

thread_local! {
    /// A VM per chunk, kept between runs of it.
    ///
    /// `fusevm::VM::new` takes a `Chunk` by value, so entering a chunk copies
    /// its whole program — the op vector, the constant pool and the name table.
    /// That copy is paid per *call* for a procedure whose body was compiled
    /// into another chunk, which is every procedure defined inside `eval`,
    /// `namespace eval` or a `source`d file: 200,000 calls of a two-line
    /// procedure defined in an `eval` cost 2.009 s of CPU where the same
    /// procedure written at the top level cost 0.009 s, and a profile of the
    /// first had `Op::clone` and the copy out of the op slice at the top of it.
    ///
    /// Keeping the VM lets the program stay where it is. [`VM::reset`] clears
    /// every other part of the machine and takes the chunk by value, so the
    /// chunk is moved out of the VM and straight back into it and nothing is
    /// copied. The entry holds the `Arc` it was built from, so the pointer it
    /// is found by cannot be reused by a different chunk while it is held.
    ///
    /// A VM is taken *out* while it runs and put back afterwards, so a
    /// recursive or re-entrant call builds one of its own rather than
    /// disturbing the run above it.
    static VMS: std::cell::RefCell<Vec<(Arc<Chunk>, VM)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// How many chunks keep a VM. Each entry holds a copy of that chunk's program,
/// so this is a memory bound as much as a hit-rate one; a script alternating
/// between more chunks than this pays the copy on the ones that fall out.
const POOLED_VMS: usize = 8;

/// A VM positioned to run `chunk`, reusing the one this chunk last ran on.
fn acquire_vm(chunk: &Arc<Chunk>) -> VM {
    let pooled = VMS.with(|pool| {
        let mut pool = pool.borrow_mut();
        let at = pool.iter().position(|(key, _)| Arc::ptr_eq(key, chunk))?;
        Some(pool.remove(at).1)
    });
    match pooled {
        Some(mut vm) => {
            // Out and straight back in: `VM::reset` takes a chunk by value, and
            // handing it the VM's own program is what makes the reset free.
            let program = std::mem::take(&mut vm.chunk);
            vm.reset(program);
            vm
        }
        None => VM::new((**chunk).clone()),
    }
}

/// Give a finished VM back, so the next run of `chunk` need not copy it.
fn release_vm(chunk: &Arc<Chunk>, vm: VM) {
    VMS.with(|pool| {
        let mut pool = pool.borrow_mut();
        if pool.len() >= POOLED_VMS {
            pool.remove(0);
        }
        pool.push((Arc::clone(chunk), vm));
    });
}

/// A chunk interns its own name table, so the slot holding a given variable
/// differs from chunk to chunk and a slot vector cannot be carried from one run
/// to the next. The interpreter's map is the authority; a chunk's slots are a
/// projection of it, built here on entry and read back by `flush` on exit.
fn seed(chunk: &Chunk, shared: &Shared) -> Vec<Value> {
    let traced = TracedIn::of(chunk);
    // A read trace exists to let its owner put the current value in place
    // before anything reads it, so it runs before the projection is taken
    // rather than after.
    let mut values: Vec<Value> = {
        let state = shared.lock().expect("interpreter lock");
        chunk
            .names
            .iter()
            .map(|name| {
                let key = state.alias_of(name);
                // A name a projection cannot answer — the interpreter's, said so
                // by the `::` the chunk kept, or a namespace's, which no frame
                // slot can be named. Outside a projection the two paths reach
                // the same table.
                let value = if crate::cmd_namespace::is_namespaced(name) {
                    state.root_global(key)
                } else {
                    state.globals.get(key)
                };
                value.cloned().unwrap_or(Value::Undef)
            })
            .collect()
    };
    traced.blank_reads(&mut values);
    values
}

/// Write a finished chunk's slots back into the interpreter's variables. A slot
/// left `Undef` — never assigned, or unset — removes the variable rather than
/// storing an empty value, so `unset` survives into the next evaluation.
fn flush(chunk: &Chunk, shared: &Shared, globals: &[Value]) {
    let traced = TracedIn::of(chunk);
    let fired = write_back(chunk, shared, globals, &traced, Boundary::End);
    // Whatever a trace on the way out wants to do it does after the value is
    // stored, exactly as `TclCallVarTraces` runs a write trace after the write
    // (`generic/tclTrace.c:2616-2655`). Its refusal has nowhere to go here —
    // the command that wrote is already over — so it is dropped; the sync
    // points inside a run report it, see [`sync_traced`].
    for (name, op) in fired {
        let _ = TracedIn::fire_one(&name, op);
    }
}

/// Store a chunk's slots into the interpreter's variables, and say which traces
/// that write should fire.
///
/// Split out from [`flush`] because a trace runs foreign code that can write
/// variables of its own, and this holds the interpreter lock. Nothing is called
/// back into while it is held.
fn write_back(
    chunk: &Chunk,
    shared: &Shared,
    globals: &[Value],
    traced: &TracedIn,
    boundary: Boundary,
) -> Vec<(String, TraceOp)> {
    let mut fired = Vec::new();
    let mut state = shared.lock().expect("interpreter lock");
    for (slot, name) in chunk.names.iter().enumerate() {
        // The compiler's own loop state is named with a leading NUL so that no
        // Tcl variable can collide with it. It is rebuilt on every entry to the
        // loop that owns it, so it is not interpreter state.
        if name.starts_with('\u{0}') {
            continue;
        }
        // An aliased name is not its own variable: `upvar` outside a procedure
        // made it another spelling of one, and the write goes where that one is.
        // `alias_of` also strips the `::` a chunk keeps on an explicitly
        // qualified name, which the table does not carry, so it is consulted for
        // every such name whether or not any alias exists.
        // The write of a namespace variable goes past whatever projection is in
        // effect, to the same table [`seed`] read it from.
        let past = crate::cmd_namespace::is_namespaced(name);
        let aliased = (!state.aliases.is_empty() || name.starts_with("::"))
            .then(|| state.alias_of(name).to_string());
        let name: &str = aliased.as_deref().unwrap_or(name);
        let value = globals.get(slot).unwrap_or(&Value::Undef);
        let watched = traced.at(slot);
        if past && state.projected() {
            match value {
                Value::Undef if watched.reads || boundary == Boundary::Sync => {}
                Value::Undef => {
                    if state.root_global(name).is_some() {
                        state.set_root_global(name, None);
                        if watched.unsets {
                            fired.push((name.to_string(), TraceOp::Unset));
                        }
                    }
                }
                value => {
                    let changed = state.root_global(name) != Some(value);
                    state.set_root_global(name, Some(value.clone()));
                    if changed && watched.writes {
                        fired.push((name.to_string(), TraceOp::Write));
                    }
                }
            }
            continue;
        }
        match value {
            // A slot [`TracedIn::blank_reads`] emptied so that reads of it
            // would reach the hook is not an `unset`, and neither is a slot a
            // partly-run chunk has not written yet — see [`Boundary`]. The
            // interpreter's copy stands in both cases.
            Value::Undef if watched.reads || boundary == Boundary::Sync => {}
            Value::Undef => {
                if state.globals.remove(name).is_some() && watched.unsets {
                    fired.push((name.to_string(), TraceOp::Unset));
                }
            }
            value => {
                // The comparison decides whether a write trace fires, so it is
                // paid either way — and once it says the table already holds
                // this value, storing it again is a name allocated, a value
                // cloned and a hash insert for no change at all. Most of a
                // chunk's projection is unchanged on most exits from it: a
                // procedure body reads the globals it names far more often
                // than it writes them.
                //
                // Measured over 200,000 cross-chunk calls of a body that reads
                // five globals and writes none, twelve interleaved runs of
                // each build: 3.66s of user time before and 3.02s after by the
                // fastest run, 4.16s and 3.52s by the mean. The same workload
                // with the callee writing what it reads is 1.56s and 1.58s —
                // unchanged, as it should be. The two builds are compared
                // interleaved and by their fastest run because this machine
                // carries a heavy and varying load; an absolute figure from it
                // means nothing on its own.
                if state.globals.get(name) != Some(value) {
                    state.globals.insert(name.to_string(), value.clone());
                    if watched.writes {
                        fired.push((name.to_string(), TraceOp::Write));
                    }
                }
            }
        }
    }
    // The names the chunk's own table does not carry, which a run-time `upvar`
    // interned past the end of it. They are ordinary globals in every other
    // respect, so they are written back on the same terms — including an empty
    // slot at the end of a chunk being an `unset`. No trace is consulted: a
    // trace is registered against a name the chunk *names*, and these are
    // exactly the names it does not.
    for (offset, name) in overflow_names(chunk, globals).iter().enumerate() {
        let value = globals.get(overflow_value_index(chunk, offset));
        match value {
            Some(Value::Undef) | None if boundary == Boundary::End => {
                state.globals.remove(name);
            }
            Some(Value::Undef) | None => {}
            Some(value) => {
                state.globals.insert(name.clone(), value.clone());
            }
        }
    }
    fired
}

/// The overflow directory of a running chunk: the names a run-time `upvar`
/// reached that the chunk's own name table does not carry.
///
/// It lives in the projection it describes — at the index one past the chunk's
/// last name — rather than beside the interpreter, and that is deliberate. A
/// projection travels with the VM: it is moved in and out of a parked coroutine,
/// handed across an `eval`, and flushed by whichever code holds it. A directory
/// held anywhere else would have to be kept in step with all of that; held here
/// it cannot fall out of step, because it *is* part of the thing being moved.
fn overflow_names(chunk: &Chunk, globals: &[Value]) -> Vec<String> {
    match globals.get(chunk.names.len()) {
        Some(Value::Array(names)) => names.iter().map(to_tcl_string).collect(),
        _ => Vec::new(),
    }
}

/// Where the value of the `offset`th overflow name sits: past the directory.
fn overflow_value_index(chunk: &Chunk, offset: usize) -> usize {
    chunk.names.len() + 1 + offset
}

/// One variable of the interpreter's table, by the name a *running* command
/// spells — an alias `upvar` made is followed, as every other read of the table
/// follows it.
///
/// The compiler reaches variables by slot or by name index, both settled while
/// the script is read. `subst` cannot: the value it substitutes is only a value
/// when the command runs, so the name it names is too. See [`crate::cmd_subst`].
pub(crate) fn read_global(interp: &Shared, name: &str) -> Option<Value> {
    let state = interp.lock().expect("interpreter lock");
    let key = state.alias_of(name);
    // A namespace variable is not a frame's, so a projection in effect does not
    // answer for it — the same rule [`seed`] applies to a chunk's names. Outside
    // one both reach the same table.
    if crate::cmd_namespace::is_namespaced(name) {
        return state.root_global(key).cloned();
    }
    state.globals.get(key).cloned()
}

impl State {
    /// The name an alias stands for, or the name itself. One step is enough for
    /// every alias this frontend makes — a chain would need `upvar` to an alias,
    /// which registers the target it resolved to — and a bounded walk is what
    /// keeps a cycle from being a hang.
    fn alias_of<'a>(&'a self, name: &'a str) -> &'a str {
        // A chunk spells an explicitly `::`-qualified name with its prefix, so
        // that a frame projection can tell `$::g` from `$g`
        // (`crate::cmd_namespace::chunk_key`). The variable table knows only the
        // one variable, under the prefixless key.
        let mut at = crate::cmd_namespace::store_key(name);
        if self.aliases.is_empty() {
            return at;
        }
        for _ in 0..8 {
            match self.aliases.get(at) {
                Some(target) if target != at => at = target,
                _ => break,
            }
        }
        at
    }
}

impl State {
    /// Whether a nested script compiled now would run inside a frame projection.
    /// The compiler needs it to key a `::`-qualified name apart from a bare one;
    /// see [`crate::compiler::Compiler::projected`].
    pub(crate) fn projected(&self) -> bool {
        !self.projections.is_empty()
    }

    /// The interpreter's own value for the variable `key`, reached past whatever
    /// frame projections are in effect.
    ///
    /// `::g` names the root namespace's variable wherever it is written, so a
    /// projection must not answer it from the frame's view. Projections nest, and
    /// each one displaced the table the one outside it installed, so the
    /// interpreter's own table is the *outermost* one — and the table in
    /// `globals` when nothing is projected.
    fn root_global(&self, key: &str) -> Option<&Value> {
        match self.projections.first() {
            Some(p) => p.outer.get(key),
            None => self.globals.get(key),
        }
    }

    /// Store the interpreter's value for `key` past the projections in effect,
    /// the write matching [`State::root_global`]'s read. `None` removes it.
    fn set_root_global(&mut self, key: &str, value: Option<Value>) {
        let table = match self.projections.first_mut() {
            Some(p) => &mut p.outer,
            None => &mut self.globals,
        };
        match value {
            Some(v) => table.insert(key.to_string(), v),
            None => table.remove(key),
        };
    }

    /// What separates the frame's locals from the rest of the variables visible
    /// to a nested script, or `None` when no projection is in effect.
    ///
    /// The script is a chunk of its own, compiled at the script's own level,
    /// where the compiler has no scope to list — so `info locals` cannot be
    /// answered from the lowering the way a body's is. It is answered from the
    /// projection instead: while one is in effect the visible variables *are*
    /// the frame's, so all of them are its locals except the ones the body
    /// declared `global`. At the script's own level there is no projection and
    /// no local, and the answer is empty, as tclsh's is.
    pub(crate) fn frame_declared(&self) -> Option<Vec<String>> {
        self.projections.last().map(|p| p.declared.clone())
    }
}

/// Make the global `alias` another spelling of the global `target`, which is
/// what `upvar` outside a procedure does.
///
/// The pair lives on the interpreter rather than in a chunk, because that is the
/// lifetime it has: `tk.tcl` binds `::tk::Priv` once and every later script sees
/// one variable. `seed` and `write_back` resolve through it, so a chunk that
/// mentions either name reaches the same storage without any op knowing.
pub(crate) fn alias_global(interp: &Shared, alias: &str, target: &str) -> Result<(), String> {
    let alias = crate::cmd_namespace::store_key(alias).to_string();
    let target = crate::cmd_namespace::store_key(target).to_string();
    if alias == target {
        // `TclPtrObjMakeUpvarIdx` refuses this outright
        // (`generic/tclVar.c`: "can't upvar from variable to itself").
        return Err("can't upvar from variable to itself".to_string());
    }
    let mut state = interp.lock().expect("interpreter lock");
    // The alias takes the target's value: they are one variable from here on, and
    // the target's is the one that survives — which is what tclsh does, since the
    // link is to the target's storage.
    if let Some(value) = state.globals.get(&target).cloned() {
        state.globals.insert(alias.clone(), value);
    } else {
        state.globals.remove(&alias);
    }
    state.aliases.insert(alias, target);
    Ok(())
}

/// The index in the running chunk's projection that holds the global `key`,
/// adding it to the overflow area when the chunk's name table has no entry for
/// it. The value is seeded from the interpreter, as `seed` seeds a named one.
///
/// This is what lets `upvar #0 ::tk::Priv.$disp priv` reach a variable whose
/// name no op in the chunk could have mentioned: after this the link is an
/// ordinary [`Place::Global`], and every element, `array`, `lappend` and `unset`
/// op reaches it the way it reaches any other global.
pub(crate) fn intern_overflow(interp: &Shared, vm: &mut VM, key: &str) -> Result<u16, String> {
    let base = vm.chunk.names.len();
    let mut names = overflow_names(&vm.chunk, &vm.globals);
    if let Some(offset) = names.iter().position(|n| n == key) {
        return index_of(base + 1 + offset);
    }
    names.push(key.to_string());
    let offset = names.len() - 1;
    let value_at = base + 1 + offset;
    if vm.globals.len() <= value_at {
        vm.globals.resize(value_at + 1, Value::Undef);
    }
    vm.globals[base] = Value::array(
        names
            .iter()
            .map(|n| Value::Str(Arc::new(n.clone())))
            .collect(),
    );
    vm.globals[value_at] = interp
        .lock()
        .expect("interpreter lock")
        .globals
        .get(key)
        .cloned()
        .unwrap_or(Value::Undef);
    index_of(value_at)
}

fn index_of(index: usize) -> Result<u16, String> {
    u16::try_from(index).map_err(|_| "too many variables in one chunk".to_string())
}

/// Take the projection again, keeping whatever overflow names the old one had.
///
/// Every place a chunk's projection is rebuilt part-way through a run goes
/// through this rather than through `seed` directly: an `upvar` made before the
/// rebuild must still point somewhere after it.
fn reproject(chunk: &Chunk, shared: &Shared, old: &[Value]) -> Vec<Value> {
    let mut values = seed(chunk, shared);
    let names = overflow_names(chunk, old);
    if names.is_empty() {
        return values;
    }
    let base = chunk.names.len();
    values.resize(base + 1 + names.len(), Value::Undef);
    values[base] = Value::array(
        names
            .iter()
            .map(|n| Value::Str(Arc::new(n.clone())))
            .collect(),
    );
    let state = shared.lock().expect("interpreter lock");
    for (offset, name) in names.iter().enumerate() {
        values[base + 1 + offset] = state.globals.get(name).cloned().unwrap_or(Value::Undef);
    }
    values
}

/// Which kind of write-back this is, which is the whole of what an empty slot
/// means.
///
/// At the **end** of a chunk every global it names has been through `seed`, so
/// an empty slot is a variable the chunk unset — that is the rule `flush` has
/// always had, and removing the entry is what makes `unset` survive into the
/// next evaluation.
///
/// **Part-way through** a chunk it is not. A global the chunk has not reached
/// yet is empty too, and so is one whose value only arrived while a Tk command
/// was running: a widget created with `-textvariable v` sets `v` from C, and
/// the slot for `v` in the calling chunk is still empty because nothing has
/// assigned it. Treating that as an unset fired an unset trace at the widget
/// that had just been told about the variable, which is how this distinction
/// was found. An unset the chunk really did make is seen at the end instead —
/// later than Tcl would, never lost.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Boundary {
    Sync,
    End,
}

// ── variable traces ──────────────────────────────────────────────────────────

/// Which operations on a variable something is watching for.
///
/// Tcl keeps this as bits on the variable itself (`VAR_TRACED_READ` and its
/// neighbours, `generic/tclInt.h`), consulted before the trace list is walked
/// at all (`generic/tclTrace.c:2624`). The same shape, for the same reason: the
/// common answer is "nothing", and it has to be cheap.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Traced {
    pub reads: bool,
    pub writes: bool,
    pub unsets: bool,
}

impl Traced {
    pub fn any(self) -> bool {
        self.reads || self.writes || self.unsets
    }
}

/// What happened to a variable, in the three kinds Tcl's `Tcl_VarTraceProc`
/// distinguishes: `TCL_TRACE_READS`, `TCL_TRACE_WRITES` and `TCL_TRACE_UNSETS`
/// (`generic/tcl.h`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TraceOp {
    Read,
    Write,
    Unset,
}

/// Where a variable trace registered outside this module is answered.
///
/// tclrs has no `trace` command of its own, so the only implementation is
/// `crate::tk::linkvar`, which holds the traces `Tcl_TraceVar2` created and
/// calls the C procedures behind them. Keeping the interface here rather than
/// reaching into `crate::tk` is what lets the whole mechanism compile away in a
/// build without the feature: nothing ever installs a sink, so
/// [`traces_armed`] is false and every call site below is one relaxed atomic
/// load.
pub trait VarTraceSink: Send + Sync {
    /// Which operations on `name` are traced. Called for every global a chunk
    /// names, so it must be cheap for the overwhelmingly common "none".
    fn traced(&self, name: &str) -> Traced;

    /// Run the traces on `name` for `op`. The error is the message a trace
    /// procedure returned, which Tcl turns into the failure of the access
    /// (`generic/tclTrace.c:2663-2700`).
    fn fire(&self, name: &str, op: TraceOp) -> Result<(), String>;
}

/// False until something installs a sink *and* that sink holds at least one
/// trace, so the cost of the whole mechanism on a script that traces nothing is
/// this load.
static TRACES_ARMED: AtomicBool = AtomicBool::new(false);
static TRACE_SINK: Mutex<Option<Arc<dyn VarTraceSink>>> = Mutex::new(None);

/// Install the sink variable traces are answered through. Replacing one is
/// allowed and is what a test does; there is one per process, as there is one
/// Tk host per process.
pub fn set_var_trace_sink(sink: Arc<dyn VarTraceSink>) {
    *TRACE_SINK.lock().expect("trace sink lock") = Some(sink);
}

/// Tell the runtime whether the sink currently holds any trace at all. The sink
/// calls this as its registry fills and empties, so a script that runs after
/// every trace has been removed pays nothing.
pub fn arm_var_traces(armed: bool) {
    TRACES_ARMED.store(armed, Ordering::Relaxed);
}

/// Whether any variable in this process is traced.
pub fn traces_armed() -> bool {
    TRACES_ARMED.load(Ordering::Relaxed)
}

fn trace_sink() -> Option<Arc<dyn VarTraceSink>> {
    if !traces_armed() {
        return None;
    }
    TRACE_SINK.lock().expect("trace sink lock").clone()
}

/// The traced globals of one chunk: which of its name-table entries carry a
/// trace, and which operations that trace watches.
///
/// Recomputed wherever it is needed rather than carried, because the trace list
/// changes while a chunk runs — a Tk command that creates a widget with a
/// `-textvariable` adds one — and a copy taken at entry would miss it. The
/// recomputation is skipped entirely when nothing is traced.
struct TracedIn {
    entries: Vec<(usize, String, Traced)>,
}

impl TracedIn {
    fn of(chunk: &Chunk) -> TracedIn {
        let Some(sink) = trace_sink() else {
            return TracedIn {
                entries: Vec::new(),
            };
        };
        let entries = chunk
            .names
            .iter()
            .enumerate()
            .filter(|(_, name)| !name.starts_with('\u{0}'))
            .filter_map(|(slot, name)| {
                // A trace is registered against the variable, which is the
                // prefixless name; the chunk may spell it `::x`.
                let name = crate::cmd_namespace::store_key(name);
                let watched = sink.traced(name);
                watched.any().then(|| (slot, name.to_string(), watched))
            })
            .collect();
        TracedIn { entries }
    }

    #[cfg(feature = "tk")]
    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// What is watched at a chunk name-table index.
    fn at(&self, slot: usize) -> Traced {
        self.entries
            .iter()
            .find(|(i, _, _)| *i == slot)
            .map_or(Traced::default(), |(_, _, w)| *w)
    }

    fn fire_one(name: &str, op: TraceOp) -> Result<(), String> {
        match trace_sink() {
            Some(sink) => sink.fire(name, op),
            None => Ok(()),
        }
    }

    /// Empty the slot of every read-traced global, so that each read of one
    /// reaches the undef hook and fires its trace there.
    ///
    /// fusevm calls the hook for a `GetVar` that finds `Undef` and pushes what
    /// the hook answers *without storing it*
    /// (`fusevm-0.16.0/src/vm.rs:1749-1768`), so the slot stays empty and every
    /// read goes the same way. That is what makes a read trace fire at the read
    /// rather than at a boundary near it.
    ///
    /// The cost is that the slot no longer distinguishes "blanked" from
    /// "unset", which [`write_back`] resolves by leaving the interpreter's copy
    /// alone. An `unset` of a read-traced variable inside a chunk that never
    /// otherwise touches it is therefore not seen as an unset — the one place
    /// this projection is lossy, and the reason `Tcl_UnsetVar2` fires its
    /// traces itself.
    fn blank_reads(&self, values: &mut [Value]) {
        for (slot, _, watched) in &self.entries {
            if watched.reads {
                if let Some(cell) = values.get_mut(*slot) {
                    *cell = Value::Undef;
                }
            }
        }
    }
}

/// Store what the running chunk has written into traced variables, and fire the
/// traces that costs. Called *before* control leaves the chunk.
///
/// This is the boundary a Tk command crosses. Tcl fires a trace at the access
/// itself; a chunk here reaches its globals through native `GetVar`/`SetVar`
/// ops that fusevm gives no hook for, so a write inside a chunk is seen when
/// the chunk next hands control to something that could observe it. Calling a
/// registered Tk command is that moment, and it is the only one: no C code runs
/// between two ops of a chunk otherwise, so nothing can tell the difference.
///
/// The chunk's copy is read back afterwards, because a trace may have rewritten
/// the variable it was told about — a read-only linked variable does exactly
/// that — and the command about to run must see one value, not two.
#[cfg(feature = "tk")]
pub(crate) fn sync_out(shared: &Shared, vm: &mut VM) -> Result<(), String> {
    if !traces_armed() {
        return Ok(());
    }
    let traced = TracedIn::of(&vm.chunk);
    if traced.is_empty() {
        return Ok(());
    }
    let fired = write_back(&vm.chunk, shared, &vm.globals, &traced, Boundary::Sync);
    for (name, op) in fired {
        TracedIn::fire_one(&name, op)?;
    }
    project(shared, vm, &traced);
    Ok(())
}

/// Take up whatever the interpreter's variables now hold. Called *after*
/// control comes back.
///
/// Strictly one-way: the chunk's slot for a variable a Tk command just wrote is
/// stale by definition, and writing it back would undo the write. `.c select`
/// on a checkbutton sets its `-variable` from C, and a two-way sync here put the
/// slot's older value back over it — which is how the direction was found.
#[cfg(feature = "tk")]
pub(crate) fn sync_in(shared: &Shared, vm: &mut VM) {
    if !traces_armed() {
        return;
    }
    let traced = TracedIn::of(&vm.chunk);
    if traced.is_empty() {
        return;
    }
    project(shared, vm, &traced);
}

/// Copy every traced variable from the interpreter into the chunk's slots, and
/// re-empty the read-traced ones so the next read of one fires its trace.
#[cfg(feature = "tk")]
fn project(shared: &Shared, vm: &mut VM, traced: &TracedIn) {
    let state = shared.lock().expect("interpreter lock");
    for (slot, name, _) in &traced.entries {
        if let Some(cell) = vm.globals.get_mut(*slot) {
            *cell = state.globals.get(name).cloned().unwrap_or(Value::Undef);
        }
    }
    drop(state);
    traced.blank_reads(&mut vm.globals);
}

// ── reaching the variables through a handle ──────────────────────────────────
//
// [`Interp`] takes `&mut self` to set a variable and `&self` to read one, which
// a C stub re-entering through the host cannot produce: the same interpreter
// may already be part-way through an evaluation further down the stack. These
// take the handle instead, which is the same thing `eval` and the Tk evaluator
// already do. The lock is taken and released inside each, so nothing is held
// while a trace runs.

/// A global's value, or `None` when it is unset. Read traces are *not* fired:
/// the caller says whether this read is one a script made.
#[cfg(feature = "tk")]
pub(crate) fn global_of(shared: &Shared, name: &str) -> Option<Value> {
    shared
        .lock()
        .expect("interpreter lock")
        .globals
        .get(name)
        .cloned()
}

/// Store a global and fire its write traces. Returns the trace's refusal, which
/// is what `Tcl_ObjSetVar2` reports by answering NULL.
#[cfg(feature = "tk")]
pub(crate) fn set_global_of(shared: &Shared, name: &str, value: Value) -> Result<(), String> {
    let changed = {
        let mut state = shared.lock().expect("interpreter lock");
        let changed = state.globals.get(name) != Some(&value);
        state.globals.insert(name.to_string(), value);
        changed
    };
    if changed && traces_armed() {
        TracedIn::fire_one(name, TraceOp::Write)?;
    }
    Ok(())
}

/// Remove a global and fire its unset traces. Answers whether it was set.
///
/// This is the one unset the projection in [`TracedIn::blank_reads`] cannot
/// see, so it fires its own traces rather than waiting for a boundary.
#[cfg(feature = "tk")]
pub(crate) fn unset_global_of(shared: &Shared, name: &str) -> bool {
    let had = shared
        .lock()
        .expect("interpreter lock")
        .globals
        .remove(name)
        .is_some();
    if had && traces_armed() {
        let _ = TracedIn::fire_one(name, TraceOp::Unset);
    }
    had
}

/// The value a read of `name` should answer with, after its read traces have
/// run. `None` when the variable is genuinely unset, which is the error the
/// undef hook reports.
fn traced_read(shared: &Shared, name: &str) -> Option<Value> {
    let sink = trace_sink()?;
    if !sink.traced(name).reads {
        return None;
    }
    sink.fire(name, TraceOp::Read).ok()?;
    let state = shared.lock().expect("interpreter lock");
    state.globals.get(name).cloned()
}

// ── the hooks ────────────────────────────────────────────────────────────

/// A `catch` region the VM has entered and not yet left.
///
/// The two depths are what makes resuming possible: an error can be raised
/// anywhere below, including inside a procedure the guarded script called, and
/// restoring them puts the VM back exactly where the handler was compiled to
/// expect it.
#[derive(Clone, Copy)]
struct CatchFrame {
    /// What the region absorbs, and where it resumes.
    kind: FrameKind,
    /// Value-stack length when the region was entered.
    stack: usize,
    /// Call-frame count when the region was entered.
    frames: usize,
}

/// The two kinds of region a raised return code can land in.
#[derive(Clone, Copy)]
enum FrameKind {
    /// A `catch`, which absorbs every code — including `break` and `continue`,
    /// which is why `while {1} {catch {break}}` does not end the loop.
    /// The payload is the op index of the handler block.
    Catch(usize),
    /// A loop, which absorbs only `break` and `continue` — arriving from a
    /// nested script, from a procedure that returned one, or from anywhere
    /// else the direct jump the compiler emits could not reach. `brk` and
    /// `cont` are the op indices those two resume at.
    ///
    /// `step_start`..`step_end` is the half-open op range of the loop's
    /// *step*, over which the
    /// region stops absorbing `continue`: `for`'s `next` script gets an
    /// exception range of its own in tclsh with `supportsContinue = 0`
    /// (`generic/tclCompCmds.c:2617`), so a `continue` raised there belongs to
    /// an enclosing loop and not to this one. Sending it to `cont` would land
    /// it back on the step that raised it — `for {set i 0} {$i < 5} {incr i;
    /// continue} {}` ran for ever before this was recorded, where tclsh
    /// reports `invoked "continue" outside of a loop`.
    ///
    /// A loop with no step — `while`, `foreach` — leaves the range empty, and
    /// then nothing can be inside it.
    Loop {
        brk: usize,
        cont: usize,
        step_start: usize,
        step_end: usize,
    },
}

/// Install everything a Tcl chunk needs on a VM — the output sink, the numeric
/// hook, the extension dispatch and fusevm's tracing JIT — for a caller that
/// drives the VM itself rather than through an [`Interp`].
///
/// That caller is fusevm's ahead-of-time entry ([`crate::aot_runtime`]), which
/// owns the run and never hands control back mid-way. `catch` and coroutines
/// need a driver that does, so [`crate::aot`] refuses a script using either
/// before it compiles one.
pub fn install_hooks(vm: &mut VM) -> Hooks {
    let hooks = Hooks::new(Interp::new().shared);
    hooks.install(vm);
    hooks
}

/// The same, with the script's output collected into `buf` rather than written
/// to stdout — what the in-process ahead-of-time run reads back.
///
/// It has to be installed here rather than by replacing the VM's output sink
/// afterwards, because `puts` is a frontend op and writes through these hooks
/// rather than through the VM (see [`ext::PUTS`]); a sink swapped in after the
/// fact would catch only what fusevm's own ops print.
pub fn install_hooks_capturing(vm: &mut VM, buf: Arc<Mutex<String>>) -> Hooks {
    let hooks = Hooks::new(Interp::with_output(Output::Capture(buf)).shared);
    hooks.install(vm);
    hooks
}

/// The cells every VM's hooks write into. One set is shared by the main VM and
/// every coroutine's; the driver swaps the per-context ones (`catches`,
/// `current`) around each `run()`, so a hook never has to know which VM it is
/// running inside.
pub struct Hooks {
    /// Where the script's writes go.
    output: Output,
    error: Arc<Mutex<Option<TclError>>>,
    /// `catch` regions the *running* VM has entered and not yet left.
    catches: Arc<Mutex<Vec<CatchFrame>>>,
    /// The coroutine request an op raised, for the driver to service.
    pending: Arc<Mutex<Option<Request>>>,
    /// The name of the coroutine whose VM is running, for `info coroutine`.
    current: Arc<Mutex<Option<String>>>,
    /// The interpreter a nested `eval` runs against.
    interp: Shared,
}

impl Hooks {
    fn new(interp: Shared) -> Hooks {
        let output = interp.lock().expect("interpreter lock").output.clone();
        Hooks {
            output,
            error: Arc::new(Mutex::new(None)),
            catches: Arc::new(Mutex::new(Vec::new())),
            pending: Arc::new(Mutex::new(None)),
            current: Arc::new(Mutex::new(None)),
            interp,
        }
    }

    /// The message an extension op parked, if one did. What an ahead-of-time
    /// run reports: fusevm's AOT entry maps the VM's result to an exit code and
    /// cannot see an error the frontend raised.
    pub fn take_error(&self) -> Option<String> {
        self.error.lock().expect("error lock").take().map(|e| e.msg)
    }

    /// Give `vm` the frontend's hooks. This is the only place a hook is
    /// installed, so a coroutine prints to the same stdout, raises errors
    /// through the same cell, evaluates nested scripts against the same
    /// variables and reaches the same JIT tiers as the script that created it.
    fn install(&self, vm: &mut VM) {
        let sink = self.output.clone();
        vm.set_output_sink(Box::new(move |s: &str| sink.write(s)));
        // Sited rather than plain, so `incr`'s operand refusal can be worded
        // as `incr`'s: it and `expr {$x + 1}` lower to the same `Op::Add` on
        // the same value, and only the site tells them apart. Keeping the
        // arithmetic a native op is what keeps a counted loop traced, so the
        // distinction cannot live in an extension op here.
        vm.set_sited_numeric_hook(Arc::new(|call: fusevm::NumericCall<'_>| {
            // At an `incr` site the operands are held to `incr`'s rule before
            // the arithmetic runs, not after it fails: `incr` takes an integer,
            // so `set x 1.5; incr x` is refused where the addition would
            // happily have answered 2.5. An operand that *is* an integer falls
            // through, so promotion to a bignum still works here.
            if is_incr_site(call.chunk, call.ip) {
                if let Some(e) = incr_operand_error(call.a, call.b) {
                    return Err(e);
                }
            }
            numeric(call.op, call.a, call.b)
        }));
        // Reading a variable that was never assigned is an error in Tcl, not
        // the empty string. fusevm calls this for every variable read that
        // finds `Undef` (`VM::set_undef_hook`); answering here rather than
        // through a frontend op is what keeps the read a native op, and a
        // counted loop traced — an extension op in a loop body costs that loop
        // its JIT trace.
        let undef_interp = Arc::clone(&self.interp);
        vm.set_undef_hook(Arc::new(move |read: fusevm::UndefRead<'_>| {
            // A read-traced global is left empty by `TracedIn::blank_reads` so
            // that every read of it arrives here; its traces run and the value
            // they leave is the answer. Checked before the tolerant-site rule
            // below, because such a variable is not absent — it was emptied on
            // purpose, and `incr` on one must see what the trace put there.
            if traces_armed() {
                if let Some(name) = read.name {
                    if let Some(value) = traced_read(&undef_interp, name) {
                        return Ok(value);
                    }
                }
            }
            if tolerates_undef(read.chunk, read.ip) {
                // `incr x` on a variable that does not exist creates it at
                // zero. It is the same read op on the same name as `$x`, so
                // only the site tells them apart.
                return Ok(Value::Undef);
            }
            match read.name {
                // The compiler generates hidden globals for its own loop
                // state, named so that no script can spell them. One reaching
                // here is not a script's variable and must not be reported as
                // one.
                Some(name) if !name.starts_with('\u{0}') => {
                    Err(format!("can't read \"{name}\": no such variable"))
                }
                // fusevm builds this read's `UndefRead` with `name: None` for a
                // frame slot, so a procedure's local keeps the old reading:
                // `Undef` is exactly that reading. The chunk *does* carry the
                // names now — `src/procs.rs` publishes them and `uplevel` and
                // `apply` run against them — so what is left is for fusevm to
                // resolve one at its `Op::GetSlot` arm. See BUGS.md.
                _ => Ok(Value::Undef),
            }
        }));

        let err_cell = Arc::clone(&self.error);
        let open = Arc::clone(&self.catches);
        let pending = Arc::clone(&self.pending);
        let current = Arc::clone(&self.current);
        let interp = Arc::clone(&self.interp);
        // `puts` writes here rather than through fusevm's `PrintLn`, so that
        // what reaches the channel is Tcl's string form of the value; see
        // [`ext::PUTS`]. It is the same sink the VM's own output goes to, so
        // the two interleave in the order the script wrote them.
        let out = self.output.clone();
        vm.set_extension_handler(Box::new(move |vm: &mut VM, id: u16, arg: u8| {
            if id == ext::CATCH_END || id == ext::LOOP_LEAVE {
                open.lock().expect("catch lock").pop();
                return;
            }
            if id == ext::LOOP_ENTER {
                // Pushed in the order the compiler emits them, so popped in
                // reverse: the two trampolines, then the mark whose target is
                // where the loop's step ends. The step *begins* where the
                // continue trampoline sends control, so both bounds are read
                // out of the jump targets the compiler patched into them.
                // `Op::LoadInt` put these here, so they are read as the
                // integers they are rather than formatted and parsed back.
                // That is what pays for the third operand: over a procedure
                // holding a loop, called 200,000 times, twelve interleaved
                // runs of each build measured 0.78s of user time before this
                // and 0.75s after by the fastest run, 0.90s and 0.86s by the
                // mean.
                let index = |v: Value| match v {
                    Value::Int(i) => i as usize,
                    other => to_tcl_string(&other).parse().unwrap_or(0),
                };
                let mark = index(vm.pop());
                let cont = index(vm.pop());
                let brk = index(vm.pop());
                let target = |at: usize| match vm.chunk.ops.get(at) {
                    Some(fusevm::Op::Jump(to)) => *to,
                    _ => 0,
                };
                let (step_start, step_end) = (target(cont), target(mark));
                open.lock().expect("catch lock").push(CatchFrame {
                    kind: FrameKind::Loop {
                        brk,
                        cont,
                        step_start,
                        step_end,
                    },
                    stack: vm.stack.len(),
                    frames: vm.frames.len(),
                });
                return;
            }
            if id == ext::PUTS {
                let mut text = to_tcl_string(&vm.pop());
                if arg == 1 {
                    text.push('\n');
                }
                out.write(&text);
                vm.push(Value::Str(Arc::new(String::new())));
                return;
            }
            if coro::is_op(id) {
                let name = current.lock().expect("coroutine lock").clone();
                if let Some(request) = coro::extension(vm, id, arg, name.as_deref()) {
                    *pending.lock().expect("request lock") = Some(request);
                    vm.request_halt();
                }
                return;
            }
            let outcome = match id {
                // The ops that reach back out of the chunk: a nested script, a
                // function an inline `rust` block exported, and the two halves
                // of a procedure whose name is bound at run time. One grouped
                // pattern rather than four arms, so the ops that do not — every
                // operator and every command module, which is what a hot loop
                // is made of — fall through on one test.
                //
                // A Tk command is one of these too: `runtime-proc` folded the
                // old `TK_DISPATCH` into `DYN_CALL`, so `crate::procs::call_op`
                // resolves a script's procedure and a command Tk registered
                // through the same table, with `crate::tk::dispatch` as the
                // fallback half.
                //
                // A command with a `{*}` word is one of these as well: its
                // words are only a list of arguments once they have been
                // spliced, so which command is being called — and whether it is
                // a procedure, a Tk command or a builtin — is decided here.
                ext::EVAL | ext::FFI_CALL | ext::PROC_DEFINE | ext::DYN_CALL | ext::EXPAND_CALL => {
                    interpreter_op(&interp, vm, id, arg)
                }
                // ── the namespace block's runtime ops ────────────────────
                // Their handlers need the interpreter itself — a registry to
                // query, a file to evaluate — which the plain `extension`
                // below is not given.
                id if crate::cmd_namespace::is_op(id) => {
                    crate::cmd_namespace::extension(&interp, vm, id, arg)
                }
                id if crate::cmd_source::is_op(id) => {
                    crate::cmd_source::extension(&interp, vm, id, arg)
                }
                // ── end of the namespace block ───────────────────────────
                // ── the event loop and the scope commands ────────────────
                // Their own arms rather than the plain one below because each
                // needs the interpreter the running chunk belongs to: an
                // `after` script, a `vwait`'s variable and an `uplevel`'s
                // script are all state that lives across chunks, not inside
                // this one.
                ext::AFTER => crate::cmd_after::after_op(&interp, vm, arg),
                ext::UPDATE => crate::cmd_after::update_op(&interp, vm, arg),
                ext::VWAIT => crate::cmd_after::vwait_op(&interp, vm, arg),
                // `upvar` with a computed target needs the interpreter for the
                // same reason `uplevel` does: a name the chunk's table does not
                // carry is interned against the interpreter's variables.
                ext::UPVAR => crate::cmd_scope::upvar_op(&interp, vm, arg),
                // `dict with` binds variables named by the dictionary's own
                // keys, so it needs the interpreter for exactly the reason
                // `upvar` with a computed target does: a name the chunk's table
                // does not carry is interned against the interpreter's
                // variables. Its write-back is dispatched beside it so the two
                // halves raise the same located error type.
                ext::DICT_WITH_BIND => crate::assoc::dict_with_bind(&interp, vm),
                ext::DICT_WITH_END => crate::assoc::dict_with_end(vm, arg),
                // Following a link needs nothing but the VM, but it raises the
                // located `TclError` the arms above raise rather than the plain
                // string the module below returns, so it is dispatched here.
                ext::LINK_GET => crate::cmd_scope::link_get(vm, arg == 1),
                ext::LINK_SET => crate::cmd_scope::link_set(vm),
                // A variable whose *name* the script computed — `set $n 1`,
                // `unset $n`, `incr $n`, `info exists $n`. Here for exactly the
                // reason `upvar` with a computed target is: the name is a value,
                // so it may be one the chunk's table does not carry, and
                // reaching it means interning it against the interpreter's own
                // variables. See `crate::cmd_scope::dynamic_link`.
                ext::DYN_GET => crate::cmd_scope::dyn_get_op(&interp, vm, arg),
                ext::DYN_SET => crate::cmd_scope::dyn_set_op(&interp, vm),
                ext::DYN_UNSET => crate::cmd_scope::dyn_unset_op(&interp, vm, arg == 1),
                ext::DYN_EXISTS => crate::cmd_scope::dyn_exists_op(vm, &interp),
                // ── end of the event block ───────────────────────────────
                // ── the ops that run a script in another frame ───────────
                // `eval` inside a body, `uplevel` and `apply`. Each needs the
                // interpreter, and each is the same exchange: the running
                // chunk's variables out, a projection of the target frame in,
                // the script, and the frame's slots written back.
                ext::EVAL_FRAME => eval_frame_op(&interp, vm, arg),
                ext::UPLEVEL => uplevel_op(&interp, vm, arg),
                ext::APPLY => apply_op(&interp, vm, arg),
                // `subst` reads the calling frame's variables and runs the
                // commands its value spells against that frame, so it is the
                // fourth op of this kind rather than one more entry in the
                // plain `extension` below.
                ext::SUBST => crate::cmd_subst::subst_op(&interp, vm, arg),
                // `lsort` is dispatched here rather than with the rest of the
                // list block, because `-command` calls back into the
                // interpreter once per compared pair — and whether a given call
                // says `-command` is only known when it runs.
                ext::LSORT => crate::cmd_list::lsort_op(&interp, vm, arg).map_err(TclError::plain),
                // `regsub` for the same reason: `-command` calls back into the
                // interpreter once per match, and whether a given call says
                // `-command` is only known when it runs.
                crate::regexp::ext::REGSUB => {
                    crate::regexp::regsub_op(&interp, vm, arg).map_err(TclError::plain)
                }
                // `info globals` / `vars` answer from the interpreter's own
                // table, not the chunk's name pool: `argc`, `argv` and `argv0`
                // are set by the host and are interned only if the script
                // happens to mention them, so a chunk-only answer omits exactly
                // the variables every script starts with.
                crate::cmd_info::ext::NAMES => info_names_op(&interp, vm, arg),
                // ── end of the frame ops ─────────────────────────────────
                // The channel ops write through the running interpreter's own
                // output, so that `puts stdout x` reaches wherever `puts x`
                // does — including an `Output::Capture`. That sink is only in
                // scope here, which is why they are dispatched from the closure
                // rather than from `extension` below.
                //
                // A bounded range rather than `id >= CHANNEL_BASE`: the blocks
                // above it — namespaces, the event loop, `package` — are all
                // higher ids, and an open-ended test here would claim every one
                // of them. Their arms happen to stand earlier, but that would
                // make arm order the only thing keeping `after` out of the
                // channel handler, and arm order is not what the block map
                // promises.
                id if (ext::CHANNEL_BASE..ext::CHANNEL_END).contains(&id) => {
                    crate::cmd_channel::run(vm, id, arg, &out).map_err(TclError::plain)
                }
                // Its own arm because `package require` may run a script — the
                // `ifneeded` one, or `package unknown` — and a script needs
                // the interpreter, which only this closure holds.
                ext::PACKAGE => package_op(&interp, vm, arg),
                // Its own arm because it is the one op that produces a
                // `TclError` carrying something other than an error: the code
                // and the level are the point of it, and `extension` below can
                // only answer with a message.
                ext::RAISE => {
                    let level = to_tcl_string(&vm.pop()).parse().unwrap_or(0);
                    let code = to_tcl_string(&vm.pop()).parse().unwrap_or(TCL_ERROR);
                    Err(TclError::coded(code, level, to_tcl_string(&vm.pop())))
                }
                // Its own arm for the same reason [`ext::RAISE`] is: it is the
                // *other* op that produces a `TclError` carrying a code, and it
                // reconstructs it from the option dictionary the handler was
                // resumed with rather than from a code the compiler chose.
                ext::RERAISE => {
                    let msg = to_tcl_string(&vm.pop());
                    let options = to_tcl_string(&vm.pop());
                    vm.pop(); // the visible code, superseded by `options`
                    Err(TclError::from_options(&options, msg))
                }
                _ => extension(vm, id, arg).map_err(TclError::plain),
            };
            if let Err(e) = outcome {
                *err_cell.lock().expect("error lock") = Some(e);
                // `VM::run` pops one value when it stops, so leave it one to
                // pop: the stack then still holds what the failing op left, and
                // the catch driver's depth arithmetic stays exact.
                vm.push(Value::Undef);
                vm.request_halt();
            }
        }));

        let entered = Arc::clone(&self.catches);
        let wide_err = Arc::clone(&self.error);
        vm.set_extension_wide_handler(Box::new(move |vm: &mut VM, id: u16, payload: usize| {
            if id == ext_wide::DBG_LINE {
                // Only a chunk compiled by `compile_debug` carries these, and
                // only `--dap` answers them; without a session attached this is
                // one `Option` check.
                crate::dap::at_line(vm, payload);
                return;
            }
            if id == ext_wide::ERROR_AT {
                // A failure the compiler found and lowered as code (see
                // `Compiler::defer`). It carries the line the refusal would have
                // been reported at, so deferring it costs the diagnostic
                // nothing but its timing.
                let msg = to_tcl_string(&vm.pop());
                *wide_err.lock().expect("error lock") = Some(TclError {
                    msg,
                    line: Some(payload),
                    code: TCL_ERROR,
                    level: 0,
                });
                vm.push(Value::Undef);
                vm.request_halt();
                return;
            }
            if id == ext_wide::CATCH {
                entered.lock().expect("catch lock").push(CatchFrame {
                    kind: FrameKind::Catch(payload),
                    stack: vm.stack.len(),
                    frames: vm.frames.len(),
                });
            }
        }));

        // Hot loops trace-compile through fusevm's Cranelift JIT, and a chunk
        // the block tier can take whole runs in native code with no dispatch
        // loop at all. With `jit-disk-cache` the compiled code outlives the
        // process.
        if jit_enabled() {
            vm.enable_tracing_jit();
        }
    }
}

/// Whether to arm the JIT — off when `TCLRS_JIT` is `off`, `0` or `no`.
///
/// The switch exists so the benchmark can measure the interpreter and the
/// JIT-armed VM as separate rows on the same binary — which cuts both ways. A
/// loop inside a procedure trace-compiles and the JIT row is a large win; a loop
/// at a script's top level cannot, and then arming the tier is pure cost: the
/// dispatch loop checks the recorder at every op and consults the block tier once
/// per run.
/// Read once. This is consulted on every entry into a chunk — which is once
/// per call of a procedure whose body was compiled elsewhere — and `getenv`
/// walks the whole environment under a lock every time it is asked. Measured
/// on 2,000,000 cross-chunk calls: `__findenv_locked` was 124 of 3796 samples,
/// 3.3% of the run, reached only from here.
fn jit_enabled() -> bool {
    static ARMED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ARMED.get_or_init(|| {
        !matches!(
            std::env::var("TCLRS_JIT").as_deref(),
            Ok("off") | Ok("0") | Ok("no")
        )
    })
}

/// The five extension ops that need something the chunk does not carry: the
/// interpreter itself, or a table living beside it.
///
/// They are dispatched together, behind one test in the extension handler,
/// because every other op — the operators, the list, string, `dict` and array
/// modules — is answerable from the stack alone and is what a loop is made of.
/// Three of the five are *located*: their failures stand in for refusals the
/// compiler would otherwise have deferred with the command's line attached,
/// which is why they carry a [`TclError`] and not a bare message.
fn interpreter_op(interp: &Shared, vm: &mut VM, id: u16, arg: u8) -> Result<(), TclError> {
    match id {
        ext::EVAL => eval_op(interp, vm, arg),
        ext::FFI_CALL => ffi_op(vm, arg).map_err(TclError::plain),
        // A `proc` outside its script's top level binds its name here, when the
        // command runs, rather than while it is compiled.
        ext::PROC_DEFINE => crate::procs::define_op(interp, vm),
        // A call whose name only a run-time table can resolve: such a procedure,
        // or a command Tk registered.
        ext::DYN_CALL => crate::procs::call_op(interp, vm, arg),
        // A call whose *argument list* only exists at run time, because one of
        // its words was written `{*}…`.
        ext::EXPAND_CALL => crate::procs::expand_call_op(interp, vm, arg),
        // The caller's pattern sends only the five ids above here. Answering
        // the rest the way the caller's other arm would is what keeps a sixth
        // id added there and forgotten here a wrong *answer* rather than a call
        // to whichever of these happened to be last.
        _ => extension(vm, id, arg).map_err(TclError::plain),
    }
}

/// A call to a function an inline `rust { ... }` block exported: the name was
/// pushed first, then the arguments. The library is already loaded — the
/// compiler registered it while lowering the block — so this only marshals.
fn ffi_op(vm: &mut VM, argc: u8) -> Result<(), String> {
    let mut values = Vec::with_capacity(argc as usize);
    for _ in 0..argc {
        values.push(vm.pop());
    }
    values.reverse();
    let (name, args) = values.split_first().expect("the name is pushed first");
    let result = crate::rust_ffi::call(&to_tcl_string(name), args)?;
    vm.push(result);
    Ok(())
}

/// The `eval` command: concatenate the arguments and run the result as a
/// script, against the state of the interpreter that reached this op.
///
/// The running chunk's slots are written back before the nested script runs and
/// re-read after it, so the two see one set of variables in both directions —
/// including when the nested script fails, since what it did set before failing
/// is set.
/// `info commands` / `procs` / `globals` / `vars`.
///
/// Lives here rather than in `cmd_info` because two of the four can only be
/// answered from [`State::globals`], which the extension handler reaches and a
/// bare `&mut VM` does not.
fn info_names_op(interp: &Shared, vm: &mut VM, which: u8) -> Result<(), TclError> {
    // `info locals` and in-body `info vars` push a candidate list and a place per
    // candidate ahead of the pattern; every other kind pushes the pattern and a
    // flag saying whether the command gave one. The kind says which shape is on
    // the stack, so the two are read apart here rather than made uniform — a
    // pattern-only kind must not pay for two pushes it has no use for.
    let (filter, candidates) = if which == crate::cmd_info::SET_OF {
        let pattern = to_tcl_string(&vm.pop());
        let places = crate::list::split(&to_tcl_string(&vm.pop())).map_err(TclError::plain)?;
        let names = crate::list::split(&to_tcl_string(&vm.pop())).map_err(TclError::plain)?;
        (Some(pattern), Some((names, places)))
    } else {
        let given = matches!(vm.pop(), Value::Int(1));
        let pattern = to_tcl_string(&vm.pop());
        (given.then_some(pattern), None)
    };

    let mut names: Vec<String> = match which {
        // commands: every command name the frontend answers to, plus this
        // chunk's procedures. `crate::names::commands` is the same assembly the
        // REPL's completer uses, built from the modules' own tables — a command
        // module added to the tree is listed here without a second list to keep.
        crate::cmd_info::COMMANDS => crate::names::commands()
            .into_iter()
            .map(|s| s.to_string())
            .chain(chunk_procs(vm))
            .collect(),
        crate::cmd_info::PROCS => chunk_procs(vm).collect(),
        // `info locals` from a script that is not a body: the frame it is
        // running in is a run-time fact, so the names come from the projection.
        // Through `global_names_of` for the same reason `info vars` goes through
        // it — a name *this* chunk has assigned is in its slots and not yet in
        // the interpreter's table, and `eval {set v 1; info locals}` has to list
        // it.
        crate::cmd_info::FRAME_LOCALS => {
            let declared = interp.lock().expect("interpreter lock").frame_declared();
            match declared {
                None => Vec::new(),
                Some(declared) => global_names_of(interp, vm)
                    .into_iter()
                    .filter(|n| !declared.iter().any(|d| d == n))
                    .collect(),
            }
        }
        // The candidates the compiler pushed, kept where the variable each names
        // is set — or unconditionally for a name `global`, `variable` or `upvar`
        // bound into the frame, whose visibility is not a question about a slot.
        crate::cmd_info::SET_OF => {
            let (names, places) = candidates.expect("SET_OF pushed its candidates");
            names
                .iter()
                .zip(places.iter())
                .filter(|(_, place)| match place.parse::<i64>() {
                    Ok(crate::cmd_info::ALWAYS) => true,
                    Ok(raw) => var_is_set(vm, Place::decode(raw)),
                    Err(_) => false,
                })
                .map(|(name, _)| name.clone())
                // The compiler can only offer the names the body mentions. A
                // local this activation grew afterwards — an `eval` that
                // assigned a name, a `dict with` key the body never wrote — is
                // just as much one of its variables, and it is only knowable
                // from the frame.
                .chain(
                    frame_of_current_level(vm)
                        .map(|frame| {
                            crate::cmd_scope::runtime_locals(vm, frame)
                                .into_iter()
                                .map(|(name, _)| name)
                                .collect::<Vec<String>>()
                        })
                        .unwrap_or_default(),
                )
                .collect()
        }
        // globals and vars: both halves of where a global lives mid-run — the
        // interpreter's table, which holds what the host set and what an earlier
        // evaluation left, and the running chunk's slot vector, which holds what
        // this one has assigned and not yet written back.
        _ => global_names_of(interp, vm),
    };
    // A pattern that names a namespace is matched — and answered — in fully
    // qualified form: `info vars n::*` is `::n::x` in tclsh, where a pattern
    // with no separator in it answers the bare names it matched. Both sides are
    // normalised to the absolute spelling so that `n::*` and `::n::*` are the
    // same pattern, which is what tclsh resolves them to.
    let qualified = filter.as_deref().is_some_and(|p| p.contains("::"));
    if let Some(p) = filter.as_deref() {
        if qualified {
            let p = absolute(p);
            names.retain(|name| crate::list::glob_match(&p, &absolute(name)));
        } else {
            names.retain(|name| crate::list::glob_match(p, name));
        }
    }
    if qualified {
        for name in &mut names {
            *name = absolute(name);
        }
    }
    names.sort();
    names.dedup();
    vm.push(Value::Str(Arc::new(crate::list::join(&names))));
    Ok(())
}

/// A name in its absolute spelling: rooted at the global namespace.
fn absolute(name: &str) -> String {
    if name.starts_with("::") {
        name.to_string()
    } else {
        format!("::{name}")
    }
}

/// Run `body` against the interpreter's own variable table, with the running
/// chunk's variables written out first and read back afterwards.
///
/// What an `eval` at a script's top level does, and what an op that calls a
/// *command* needs — `lsort -command`'s comparison, which `Tcl_EvalObjv`
/// invokes at the current level rather than as a script in the caller's frame.
/// Without the write-out a command it calls would not see what the running
/// chunk has assigned and not yet flushed.
pub(crate) fn at_global<T, E>(
    interp: &Shared,
    vm: &mut VM,
    body: impl FnOnce(&Shared) -> Result<T, E>,
) -> Result<T, E> {
    flush(&vm.chunk, interp, &vm.globals);
    let result = body(interp);
    vm.globals = reproject(&vm.chunk, interp, &vm.globals);
    result
}

fn eval_op(interp: &Shared, vm: &mut VM, argc: u8) -> Result<(), TclError> {
    let mut args = Vec::with_capacity(argc as usize);
    for _ in 0..argc {
        args.push(to_tcl_string(&vm.pop()));
    }
    args.reverse();
    // One argument is the script; several are concatenated as `concat` does,
    // which is where `eval $cmd $args` gets its meaning.
    let src = if args.len() == 1 {
        args.remove(0)
    } else {
        crate::cmd_list::concat(&args)
    };

    flush(&vm.chunk, interp, &vm.globals);
    let result = run_source(interp, &src);
    let globals = reproject(&vm.chunk, interp, &vm.globals);
    vm.globals = globals;
    vm.push(result?);
    Ok(())
}

/// `eval` inside a procedure body: `[declared, arg …]`.
///
/// The script runs against the procedure's own frame, which is what tclsh does —
/// see [`run_in_frame`].
fn eval_frame_op(interp: &Shared, vm: &mut VM, argc: u8) -> Result<(), TclError> {
    let mut args = Vec::with_capacity(argc as usize);
    for _ in 0..argc {
        args.push(to_tcl_string(&vm.pop()));
    }
    args.reverse();
    let declared = args.remove(0);
    let src = script_of(args);
    // The current level, which is the innermost procedure frame — not the
    // innermost VM frame, which a scope or a side exit may have pushed inside it.
    let up = levels(vm).first().copied().unwrap_or(0);
    run_in_frame(interp, vm, &src, up, &declared)
}

/// `uplevel ?level? arg …`: `[declared, arg …]`.
///
/// `#0` is the global level, which is what an ordinary `eval` already runs
/// against; any other level is a frame, counted outwards from this one.
///
/// Which word is the level is decided here rather than while compiling, because
/// `uplevel $n {…}` is ordinary Tcl: `Tcl_UplevelObjCmd` hands the *substituted*
/// first word to `TclObjGetFrame`, and takes it as a level when it has the shape
/// of one and a script follows it. Deciding it from the literal text instead
/// answered `invalid command name "1"` for `uplevel $n {set v}` where tclsh
/// answers the script's value, and ran `uplevel 1` with no script where tclsh
/// reports `wrong # args` — both measured against tclsh 9.0.4.
fn uplevel_op(interp: &Shared, vm: &mut VM, argc: u8) -> Result<(), TclError> {
    let mut args = Vec::with_capacity(argc as usize);
    for _ in 0..argc {
        args.push(to_tcl_string(&vm.pop()));
    }
    args.reverse();
    let declared = args.remove(0);
    // A level word is `#n` or a bare unsigned integer, and nothing else: `uplevel
    // 1.5 …` runs `1.5 …` as a script in tclsh, and `uplevel 5 {…}` five levels
    // deeper than the stack goes is `bad level "5"` rather than a script called
    // `5`. So the *shape* selects the word and resolving it is allowed to fail.
    let takes_level = args.len() > 1 && crate::compiler::looks_like_a_level(&args[0]);
    if args.len() == 1 && crate::compiler::looks_like_a_level(&args[0]) {
        return Err(TclError::plain(
            "wrong # args: should be \"uplevel ?level? command ?arg ...?\"".to_string(),
        ));
    }
    let level = if takes_level {
        args.remove(0)
    } else {
        "1".to_string()
    };
    let src = script_of(args);

    // The levels this context has: one per active procedure call, which is what
    // Tcl counts. The global level is not one of them — it is what `#0` names,
    // and what a relative level reaches by counting past the outermost call.
    let ups = levels(vm);
    let up = match parse_level(&level, ups.len()) {
        Some(Level::Global) => {
            flush(&vm.chunk, interp, &vm.globals);
            let result = run_source(interp, &src);
            vm.globals = seed(&vm.chunk, interp);
            vm.push(result?);
            return Ok(());
        }
        // A level counted in calls, resolved to the frame that call pushed.
        Some(Level::Up(out)) => match ups.get(out) {
            Some(&up) => up,
            None => return Err(TclError::plain(format!("bad level \"{level}\""))),
        },
        None => return Err(TclError::plain(format!("bad level \"{level}\""))),
    };
    run_in_frame(interp, vm, &src, up, &declared)
}

/// The `up` distances of the frames that are Tcl levels, innermost first.
///
/// A Tcl level is a procedure call, and fusevm pushes a frame for other reasons
/// too: the base frame a script's top level runs in, the frame a scope opens, and
/// one materialized after a JIT side exit. Only a call to a named subroutine
/// records an `entry_ip`, so that is what tells a level from a frame.
///
/// Counting VM frames instead is what made `uplevel 1` at the top level find the
/// base frame and answer, where tclsh reports `bad level "1"` — there is no level
/// above the global one.
pub(crate) fn levels(vm: &VM) -> Vec<usize> {
    let n = vm.frames.len();
    (0..n)
        .filter(|&up| vm.frames[n - 1 - up].entry_ip.is_some())
        .collect()
}

/// Which level a `level` word names.
enum Level {
    /// `#0`, or a relative level that reaches past the outermost call.
    Global,
    /// This many calls outwards from the running one.
    Up(usize),
}

/// Read `uplevel`'s level word the way `Tcl_GetFrame` reads it: `#n` counts from
/// the global level inwards, a bare number counts outwards from here, and
/// anything else is not a level at all.
fn parse_level(word: &str, depth: usize) -> Option<Level> {
    if let Some(abs) = word.strip_prefix('#') {
        let abs: usize = abs.parse().ok()?;
        // `#0` is the global level; `#1` is the outermost frame, and so on.
        if abs == 0 {
            return Some(Level::Global);
        }
        return depth.checked_sub(abs).map(Level::Up);
    }
    let rel: usize = word.parse().ok()?;
    if rel > depth {
        return None;
    }
    if rel == depth {
        Some(Level::Global)
    } else {
        Some(Level::Up(rel))
    }
}

/// One argument is the script; several are concatenated as `concat` does, which
/// is where `eval $cmd $args` gets its meaning — and why `uplevel 1 set y {a b}`
/// loses the braces and becomes three words, as it does in tclsh.
fn script_of(mut args: Vec<String>) -> String {
    if args.len() == 1 {
        args.remove(0)
    } else {
        crate::cmd_list::concat(&args)
    }
}

/// Run `src` against the variables of the frame `up` levels out.
///
/// tclsh runs an `eval`'s script in *exactly* the calling frame's context: a
/// local is visible and writable, a variable the script creates becomes a local,
/// `unset` removes one, and a bare read of a global **refuses** unless the body
/// linked it with `global`. So the interpreter's variable table is replaced for
/// the duration by a projection of that frame — its named slots, plus the names
/// the body declared global — and read back afterwards. Nothing else is visible,
/// which is the half that a projection merely *added* to the globals would get
/// wrong.
fn run_in_frame(
    interp: &Shared,
    vm: &mut VM,
    src: &str,
    up: usize,
    declared: &str,
) -> Result<(), TclError> {
    let value = in_frame(interp, vm, up, declared, |interp| run_source(interp, src))?;
    vm.push(value);
    Ok(())
}

/// The projection [`run_in_frame`] runs a script inside, as a scope any op can
/// borrow.
///
/// Split out because a script is not the only thing that has to run against the
/// calling frame: `subst` reads that frame's variables and runs the commands its
/// value spells, and `lsort -command` calls a comparison command per pair. Each
/// is several evaluations inside *one* projection, so the exchange — the running
/// chunk's variables out, the frame's in, the slots read back — belongs here and
/// not in each caller. Doing any of them against the globals instead would read
/// and write the wrong variables inside a procedure without ever saying so.
pub(crate) fn in_frame<T>(
    interp: &Shared,
    vm: &mut VM,
    up: usize,
    declared: &str,
    body: impl FnOnce(&Shared) -> Result<T, TclError>,
) -> Result<T, TclError> {
    let names: Vec<String> = vm.slot_names_at(up).to_vec();
    let frame = match vm.frames.len().checked_sub(up + 1) {
        // A frame with no name for its slots — the base frame, a scope frame, or
        // one materialized after a JIT side exit — cannot be projected, so the
        // script runs against the globals, as an ordinary `eval` does.
        //
        // A *procedure activation* whose body happens to declare no local at all
        // is projected like any other, with a view that starts empty. It used to
        // keep the interpreter's table instead, because a `::`-qualified read
        // inside the script had no other way to reach the interpreter's
        // variable; that is now a name of its own
        // (`crate::cmd_namespace::chunk_key`), so the exception is gone — and
        // with it what the exception cost. A script in such a frame was writing
        // the *global* whenever one already wore the name it meant to make a
        // local of: `set g 3; proc p {} {eval {set g 99}}` left `::g` at 99,
        // where tclsh leaves 3 and makes a local.
        Some(index) if names.is_empty() && vm.frames[index].entry_ip.is_none() => {
            flush(&vm.chunk, interp, &vm.globals);
            let result = body(interp);
            vm.globals = seed(&vm.chunk, interp);
            return result;
        }
        Some(index) => index,
        None => return Err(TclError::plain("bad level".to_string())),
    };
    let declared = crate::list::split(declared).unwrap_or_default();

    // The enclosing chunk's globals are the authority for a declared name, so
    // they go into the table before anything is read out of it.
    flush(&vm.chunk, interp, &vm.globals);
    let outer = std::mem::take(&mut interp.lock().expect("interpreter lock").globals);

    let mut view: HashMap<String, Value> = HashMap::new();
    for name in &declared {
        if let Some(v) = outer.get(name) {
            view.insert(name.clone(), v.clone());
        }
    }
    for (slot, name) in names.iter().enumerate() {
        if name.is_empty() {
            continue;
        }
        match vm.frames[frame].slots.get(slot) {
            Some(v) if *v != Value::Undef => {
                view.insert(name.clone(), v.clone());
            }
            // An unset local is absent rather than empty, so a read of it in the
            // nested script refuses exactly as it would in the body.
            _ => {
                view.remove(name);
            }
        }
    }
    // The locals this activation grew after it was compiled — a name an earlier
    // script in the same frame assigned that the body itself never mentions.
    // They are as much this frame's variables as its slots are, so the script
    // sees them by the same rule.
    for (name, value) in crate::cmd_scope::runtime_locals(vm, frame) {
        view.insert(name, value);
    }
    {
        let mut state = interp.lock().expect("interpreter lock");
        state.globals = view;
        // The displaced table goes with the projection rather than staying a
        // local of this function: a `::`-qualified name inside the script names
        // a variable *in it*, and the only way there is through the interpreter.
        state.projections.push(Projection {
            outer,
            declared: declared.clone(),
        });
    }

    let result = body(interp);

    let (after, mut outer) = {
        let mut state = interp.lock().expect("interpreter lock");
        let parked = state.projections.pop().expect("projection was pushed");
        let after = std::mem::take(&mut state.globals);
        (after, parked.outer)
    };
    for (slot, name) in names.iter().enumerate() {
        if name.is_empty() {
            continue;
        }
        let value = after.get(name).cloned().unwrap_or(Value::Undef);
        let slots = &mut vm.frames[frame].slots;
        if slot >= slots.len() {
            slots.resize(slot + 1, Value::Undef);
        }
        slots[slot] = value;
    }
    // A name the script left behind that is neither one of the body's slots nor
    // one of the globals it declared is a *new* local of this activation, which
    // is what `set` inside an `eval` makes in tclsh.
    harvest_locals(vm, frame, &after, |name| {
        !names.iter().any(|n| n == name) && !declared.iter().any(|n| n == name)
    });
    for name in &declared {
        match after.get(name) {
            Some(v) => outer.insert(name.clone(), v.clone()),
            None => outer.remove(name),
        };
    }
    interp.lock().expect("interpreter lock").globals = outer;
    vm.globals = seed(&vm.chunk, interp);
    result
}

/// Move the names a script left behind that belong to the activation in `frame`
/// into that frame's run-time slots, and answer with the names it claimed.
///
/// `grew` decides which of `after`'s names are the frame's rather than the
/// interpreter's; a name the frame *already* grew is always its own, whatever
/// `grew` says about it. An existing run-time local keeps its position — a name
/// must not move between slots — and the ones this script invented are appended
/// in sorted order, because a `HashMap`'s iteration order is not an order
/// anything should inherit. A name that is gone from `after` was unset, and its
/// slot is cleared rather than removed, exactly as a body's own slot records an
/// `unset`.
fn harvest_locals(
    vm: &mut VM,
    frame: usize,
    after: &HashMap<String, Value>,
    grew: impl Fn(&str) -> bool,
) -> Vec<String> {
    let known = crate::cmd_scope::runtime_names(vm, frame);
    let mut fresh: Vec<String> = after
        .keys()
        .filter(|name| !known.iter().any(|n| n == *name) && grew(name))
        .cloned()
        .collect();
    fresh.sort_unstable();
    let taken: Vec<String> = known.into_iter().chain(fresh).collect();
    for name in &taken {
        let Some(slot) = crate::cmd_scope::runtime_slot_alloc(vm, frame, name) else {
            continue;
        };
        let value = after.get(name).cloned().unwrap_or(Value::Undef);
        vm.frames[frame].slots[usize::from(slot)] = value;
    }
    taken
}

/// `apply lambdaExpr ?arg …?`: `[lambda, arg …]`.
///
/// A lambda is a procedure body with its own frame — its parameters are locals,
/// a bare name is a local, a global needs `$::g`, and `return` returns from it —
/// so it is run as one rather than given a second calling convention. The
/// procedure is named with a leading NUL, which no Tcl name can be, and lives
/// only in the chunk built for this call.
fn apply_op(interp: &Shared, vm: &mut VM, argc: u8) -> Result<(), TclError> {
    let mut args = Vec::with_capacity(argc as usize);
    for _ in 0..argc {
        args.push(to_tcl_string(&vm.pop()));
    }
    args.reverse();
    let lambda = args.remove(0);

    let parts = crate::list::split(&lambda).map_err(|_| TclError::plain(bad_lambda(&lambda)))?;
    let (params, body) = match parts.as_slice() {
        [params, body] => (params, body),
        // The third element is a namespace. This frontend has one namespace, so
        // any other is refused rather than silently ignored.
        [params, body, ns] if ns == "::" || ns.is_empty() => (params, body),
        [_, _, ns] => {
            return Err(TclError::plain(format!(
                "the namespace \"{ns}\" of a lambda is not supported yet: this frontend has only \"::\""
            )))
        }
        _ => return Err(TclError::plain(bad_lambda(&lambda))),
    };

    const NAME: &str = "\u{0}apply";
    let mut src = String::with_capacity(body.len() + params.len() + 32);
    src.push_str("proc ");
    src.push_str(NAME);
    src.push(' ');
    src.push_str(&crate::list::quote(params, false));
    src.push(' ');
    src.push_str(&crate::list::quote(body, false));
    src.push('\n');
    src.push_str(NAME);
    for a in &args {
        src.push(' ');
        src.push_str(&crate::list::quote(a, false));
    }

    flush(&vm.chunk, interp, &vm.globals);
    let result = run_source(interp, &src);
    vm.globals = seed(&vm.chunk, interp);
    // The synthesized name must not surface in a diagnostic the script can see:
    // tclsh reports a lambda's arity against `apply lambdaExpr`.
    vm.push(result.map_err(|e| TclError::plain(rename_lambda(&e.msg)))?);
    Ok(())
}

fn bad_lambda(lambda: &str) -> String {
    format!("can't interpret \"{lambda}\" as a lambda expression")
}

/// Replace the synthesized procedure's name in a diagnostic with what tclsh
/// names: `apply lambdaExpr`, followed by the lambda's own parameters.
fn rename_lambda(msg: &str) -> String {
    msg.replace("\u{0}apply", "apply lambdaExpr")
}

// ── reaching the interpreter from a running chunk ────────────────────────
//
// Windows onto the interpreter's variables for an op that has to run a script,
// or wait for one, in the middle of a chunk. `eval` opens and closes the same
// two windows inline above; `after`, `vwait` and `uplevel` need them named,
// because they cross the boundary more than once per command, and `source` and
// the `namespace` queries need both at once — see [`with_written_back`].

/// Write the running chunk's variables back into the interpreter, so a nested
/// script sees what the chunk has done.
pub(crate) fn flush_globals(vm: &VM, interp: &Shared) {
    flush(&vm.chunk, interp, &vm.globals);
}

/// Project the interpreter's variables back into the running chunk, so the
/// chunk sees what a nested script did.
pub(crate) fn reseed_globals(vm: &mut VM, interp: &Shared) {
    vm.globals = reproject(&vm.chunk, interp, &vm.globals);
}

/// Run `body` with the chunk's variables written back to the interpreter and
/// re-read afterwards — both windows above, opened and closed around one call.
///
/// A chunk addresses its variables through a slot vector it owns for the
/// duration of one run, so a command that reaches interpreter state from inside
/// that run — `source`, `namespace which -variable`, `namespace inscope` —
/// would otherwise see the values the *previous* run left. This is the same
/// exchange [`eval_op`] performs, factored out so that every such command makes
/// it the same way.
pub(crate) fn with_written_back<T>(
    interp: &Shared,
    vm: &mut VM,
    body: impl FnOnce(&Shared) -> T,
) -> T {
    flush_globals(vm, interp);
    let out = body(interp);
    reseed_globals(vm, interp);
    out
}

/// What the interpreter holds for `name`, or `None` when it holds nothing.
/// Only correct once [`flush_globals`] has run, which is what `vwait` does
/// before it starts waiting.
///
/// The name arrives as a script wrote it, and the map is keyed the way
/// [`crate::cmd_namespace::store_key`] spells a qualified name — `::done` is
/// stored as `done`. `vwait ::done` is the ordinary spelling, so without the
/// normalisation the wait watches a name nothing ever writes and ends as
/// `would wait forever`.
pub(crate) fn global_value(interp: &Shared, name: &str) -> Option<Value> {
    interp
        .lock()
        .expect("interpreter lock")
        .globals
        .get(crate::cmd_namespace::store_key(name))
        .cloned()
}

/// Every variable that is set, for `info globals` and `info vars`.
///
/// Two sources, because neither is complete on its own: the interpreter's map
/// holds what earlier evaluations left behind, and the running chunk's slot
/// vector holds what *this* one has assigned and not yet written back. A name
/// the compiler generated for its own loop state is not a script's variable and
/// is left out, as it is everywhere else.
pub(crate) fn global_names_of(interp: &Shared, vm: &VM) -> Vec<String> {
    let mut names: Vec<String> = interp
        .lock()
        .expect("interpreter lock")
        .globals
        .keys()
        .filter(|n| !n.starts_with('\u{0}'))
        .cloned()
        .collect();
    let overflow = overflow_names(&vm.chunk, &vm.globals);
    let named = vm.chunk.names.iter().enumerate().chain(
        overflow
            .iter()
            .enumerate()
            .map(|(offset, name)| (overflow_value_index(&vm.chunk, offset), name)),
    );
    for (slot, name) in named {
        if name.starts_with('\u{0}') {
            continue;
        }
        // The chunk's spelling of an explicitly qualified name carries a `::`
        // the variable table does not; `info globals` answers with the table's.
        let name = crate::cmd_namespace::store_key(name);
        if matches!(vm.globals.get(slot), Some(Value::Undef) | None) {
            // Unset in the chunk is unset, whatever an earlier evaluation left
            // in the map: this chunk's `unset x` has to be visible before the
            // write-back happens.
            names.retain(|n| n != name);
            continue;
        }
        names.push(name.to_string());
    }
    names
}

/// Whether the variable at `place` is set, for `info exists`.
pub(crate) fn var_is_set(vm: &VM, place: Place) -> bool {
    if let Place::Link(slot) = place {
        // A link that was never made is not set, and a link to something unset
        // is not set either — which is what tclsh answers for `upvar bogus b`
        // followed by `info exists b`.
        let Some(link) = crate::cmd_scope::link_at(vm, slot) else {
            return false;
        };
        return !matches!(
            crate::cmd_scope::read_link(vm, &link),
            Some(Value::Undef) | None
        );
    }
    let value = match place {
        Place::Global(index) => vm.globals.get(index as usize),
        Place::Slot(slot) | Place::Link(slot) => {
            vm.frames.last().and_then(|f| f.slots.get(slot as usize))
        }
    };
    !matches!(value, Some(Value::Undef) | None)
}

/// The evaluator [`crate::cmd_package`] runs an `ifneeded` or `package unknown`
/// script through: [`eval_op`]'s write-back and re-read, with the concatenation
/// left out because a package script is one string.
struct VmScriptHost<'a> {
    interp: &'a Shared,
    vm: &'a mut VM,
}

impl crate::cmd_package::ScriptHost for VmScriptHost<'_> {
    fn eval(&mut self, src: &str) -> Result<String, TclError> {
        flush(&self.vm.chunk, self.interp, &self.vm.globals);
        let result = run_source(self.interp, src);
        self.vm.globals = reproject(&self.vm.chunk, self.interp, &self.vm.globals);
        result.map(|v| to_tcl_string(&v))
    }
}

/// The `package` command. The arguments come off the stack before the script
/// host is built, because the host borrows the VM for as long as it lives.
fn package_op(interp: &Shared, vm: &mut VM, argc: u8) -> Result<(), TclError> {
    let (line, argv) = crate::cmd_package::take_args(vm, argc);
    let outcome = {
        let mut host = VmScriptHost { interp, vm };
        crate::cmd_package::run(&argv, &mut host)
    };
    match outcome {
        Ok(result) => {
            vm.push(Value::Str(Arc::new(result)));
            Ok(())
        }
        // A failure raised by a script this command ran already carries the
        // line it happened on; only a failure of `package` itself takes the
        // line of the `package` command.
        Err(e) => Err(TclError {
            line: e.line.or(Some(line)),
            ..e
        }),
    }
}

// ── the driver ───────────────────────────────────────────────────────────

/// How a context is suspended, which decides what resuming it may pass in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Park {
    /// Running, or about to be — the main script's context always is.
    Running,
    /// Suspended by `yield`, which takes at most one resumption value.
    AtYield,
    /// Suspended by `yieldto`, whose value is the whole resumption argument
    /// list.
    AtYieldTo,
}

/// One execution context: the main script, or a live coroutine.
struct Context {
    /// `None` once the context has been retired.
    vm: Option<VM>,
    /// The `catch` regions this context has open, parked while another runs.
    catches: Vec<CatchFrame>,
    /// The coroutine's name; `None` for the main script.
    name: Option<String>,
    /// Where control goes when this context yields, finishes or fails.
    resumer: Option<usize>,
    park: Park,
}

/// The driver of one evaluation: every execution context of one script, and
/// the loop that runs them.
struct Machine {
    hooks: Hooks,
    /// The compiled program, from which each coroutine's VM is built. Held as
    /// the handle it arrived as, because a `proc` this run defines records it —
    /// see [`State::running`].
    chunk: Arc<Chunk>,
    contexts: Vec<Context>,
    /// Live coroutines by name. A name leaves as soon as its body ends, which
    /// is what makes a later call report `invalid command name`.
    live: HashMap<String, usize>,
    /// Every name this run has ever made a coroutine of, so a `yieldto` at a
    /// name that is not one can be told apart from one at a coroutine that has
    /// since finished.
    created: HashSet<String>,
    /// The one global variable table, moved into whichever VM runs.
    globals: Vec<Value>,
    /// The context currently running.
    current: usize,
}

impl Machine {
    /// Run one chunk against the interpreter's variables, from the first op to
    /// the end of the main context.
    fn run(shared: &Shared, chunk: Arc<Chunk>) -> Result<Value, TclError> {
        Machine::start(shared, chunk, None)
    }

    /// The same, optionally entering a procedure body inside the chunk instead
    /// of running it from the top.
    ///
    /// `at` is the body's entry point and its actual arguments, arranged the way
    /// the prologue expects them. They sit below a frame that returns past the
    /// end of the program, so the body's `Op::ReturnValue` ends this run with
    /// the procedure's result — which is how [`Machine::create`] enters a
    /// coroutine's body, and the reason both go through one function.
    fn start(
        shared: &Shared,
        chunk: Arc<Chunk>,
        at: Option<(usize, Vec<Value>)>,
    ) -> Result<Value, TclError> {
        let hooks = Hooks::new(Arc::clone(shared));
        let mut main = acquire_vm(&chunk);
        hooks.install(&mut main);
        let globals = seed(&chunk, shared);
        if let Some((entry, actuals)) = at {
            let base = main.stack.len();
            for value in actuals {
                main.stack.push(value);
            }
            main.frames.push(Frame {
                return_ip: chunk.ops.len(),
                stack_base: base,
                slots: Vec::new(),
                // The same fusevm 0.17.0 obligation `crate::procs::call_op` has:
                // this frame is a subroutine activation entered without an
                // `Op::Call`, so it names its own entry point or it is not a Tcl
                // level as far as `levels` is concerned.
                entry_ip: Some(entry),
            });
            main.ip = entry;
        }
        // Recorded for as long as this chunk is the running one, so that a
        // `proc` it defines can be called from another chunk later.
        shared
            .lock()
            .expect("interpreter lock")
            .running
            .push(Arc::clone(&chunk));

        let mut machine = Machine {
            hooks,
            chunk,
            contexts: vec![Context {
                vm: Some(main),
                catches: Vec::new(),
                name: None,
                resumer: None,
                park: Park::Running,
            }],
            live: HashMap::new(),
            created: HashSet::new(),
            globals,
            current: 0,
        };
        let outcome = machine.drive();
        // The machine's first context is the VM this run started on; a
        // coroutine's is not, and is dropped with the machine. Only the first
        // goes back to the pool, and only if the run left it there — a
        // coroutine switch can take it.
        if let Some(vm) = machine.contexts[0].vm.take() {
            release_vm(&machine.chunk, vm);
        }
        // The variables a failing script did set are still set, as they are in
        // the reference interpreter, so the write-back happens either way.
        flush(&machine.chunk, shared, &machine.globals);
        shared.lock().expect("interpreter lock").running.pop();
        // Likewise the output: an error the caller prints must not overtake
        // what the failing script had already written.
        machine.hooks.output.flush();

        match outcome? {
            VMResult::Ok(v) => Ok(v),
            VMResult::Halted => Ok(Value::Str(Arc::new(String::new()))),
            VMResult::Error(e) => Err(TclError::plain(e)),
        }
    }

    /// Run contexts until the main script finishes or an error escapes it.
    fn drive(&mut self) -> Result<VMResult, TclError> {
        loop {
            let outcome = self.run_current();

            let raised = self
                .hooks
                .error
                .lock()
                .expect("error lock")
                .take()
                .or_else(|| match &outcome {
                    VMResult::Error(e) => Some(TclError::plain(e.clone())),
                    _ => None,
                });
            if let Some(e) = raised {
                self.raise(e)?;
                continue;
            }

            let request = self.hooks.pending.lock().expect("request lock").take();
            if let Some(request) = request {
                // The op halted mid-expression, so `run` popped a live value
                // from underneath it. Put it back before anything else touches
                // this stack.
                if let VMResult::Ok(v) = outcome {
                    self.vm(self.current).stack.push(v);
                }
                if let Err(e) = self.service(request) {
                    self.raise(e)?;
                }
                continue;
            }

            // Nothing was raised and nothing was requested: this context ran to
            // the end of its program.
            if self.current == 0 {
                return Ok(outcome);
            }
            self.retire(outcome);
        }
    }

    /// Swap the running context's state in, run its VM, and take the state back
    /// out. Only one VM runs at a time, so the global table and the open
    /// `catch` regions move rather than being copied.
    fn run_current(&mut self) -> VMResult {
        let current = self.current;
        let name = self.contexts[current].name.clone();
        *self.hooks.current.lock().expect("coroutine lock") = name.clone();
        *self.hooks.catches.lock().expect("catch lock") =
            std::mem::take(&mut self.contexts[current].catches);
        let globals = std::mem::take(&mut self.globals);
        // A script this VM starts runs a machine of its own, which reads this to
        // see what is running around it — see `State::running`.
        self.hooks
            .interp
            .lock()
            .expect("interpreter lock")
            .contexts
            .push(name);

        let vm = self.vm(current);
        vm.globals = globals;
        vm.clear_halt();
        let outcome = vm.run();
        let globals = std::mem::take(&mut vm.globals);
        self.hooks
            .interp
            .lock()
            .expect("interpreter lock")
            .contexts
            .pop();

        self.globals = globals;
        self.contexts[current].catches =
            std::mem::take(&mut self.hooks.catches.lock().expect("catch lock"));
        outcome
    }

    fn vm(&mut self, context: usize) -> &mut VM {
        self.contexts[context].vm.as_mut().expect("live context")
    }

    /// Report a Tcl error in the running context: resume at its innermost open
    /// `catch` handler, or — for a coroutine with none — end the coroutine and
    /// report the error to whoever resumed it, as the reference implementation
    /// does. `Err` means nothing was left to catch it.
    fn raise(&mut self, mut e: TclError) -> Result<(), TclError> {
        // Every VM call frame the code unwinds past is a procedure-call
        // boundary, and a `return` spends one level at each — which is what
        // makes `proc p {} {return -code break}` break the loop that called
        // `p` rather than reporting code 2 there. A call that crosses a *chunk*
        // boundary runs on a VM of its own, so its frames are not in this
        // count; [`call_in_chunk`] spends that level where the two VMs meet.
        let mut depth = self.vm(self.current).frames.len();
        loop {
            if let Some(frame) = self.contexts[self.current].catches.last().copied() {
                // Which op of *this region's* call frame is running, which is
                // what says whether a `continue` came out of a loop's step —
                // see [`FrameKind::Loop`]. A raise from a deeper procedure
                // stands, for this region, at the call that entered it, so
                // `proc p {} {return -code continue}` called from a step
                // leaves the loop exactly as a `continue` written there does.
                let vm = self.vm(self.current);
                let raised_at = match vm.frames.get(frame.frames) {
                    Some(inner) => inner.return_ip.saturating_sub(1),
                    None => vm.ip,
                };
                for _ in 0..depth.saturating_sub(frame.frames) {
                    e = e.descend();
                }
                depth = frame.frames;
                let code = e.visible_code();
                let resume = match frame.kind {
                    FrameKind::Catch(handler) => Some(handler),
                    // A loop takes a `break` or a `continue` and lets every
                    // other code — an error, a `return` — carry on outwards.
                    FrameKind::Loop { brk, .. } if code == TCL_BREAK => Some(brk),
                    FrameKind::Loop {
                        cont,
                        step_start,
                        step_end,
                        ..
                    } if code == TCL_CONTINUE => {
                        // Not from the step, which this loop does not absorb.
                        (!(step_start..step_end).contains(&raised_at)).then_some(cont)
                    }
                    FrameKind::Loop { .. } => None,
                };
                let Some(resume) = resume else {
                    // This region does not absorb the code, so the loop is being
                    // abandoned and its `LOOP_LEAVE` will never run: the record
                    // goes here, and the unwind carries on outwards.
                    self.contexts[self.current].catches.pop();
                    continue;
                };
                // A `catch` region ends at its handler — the `CATCH_END` that
                // would close it is on the ordinary path, which was jumped over
                // — so its record goes now. A *loop* region does not end: both
                // trampolines land inside the loop, and the `LOOP_LEAVE` at its
                // exit is still ahead and pops the record itself. Popping here
                // as well left the loop with no region at all, so the second
                // raised `continue` in one loop was `invoked "continue" outside
                // of a loop` where tclsh 9.0.4 runs the next iteration
                // (measured: `foreach i {1 2 3} {eval {continue}}`), and a
                // raised `break` in a nested loop closed the *outer* one's
                // region instead of its own.
                if matches!(frame.kind, FrameKind::Catch(_)) {
                    self.contexts[self.current].catches.pop();
                }
                let options = e.options();
                let vm = self.vm(self.current);
                // Unwind to the guarded script's entry state and hand the
                // handler the code, the options and the message.
                vm.frames.truncate(frame.frames);
                vm.stack.truncate(frame.stack);
                vm.stack.resize(frame.stack, Value::Undef);
                if matches!(frame.kind, FrameKind::Catch(_)) {
                    vm.push(Value::Int(code as i64));
                    vm.push(Value::Str(Arc::new(options)));
                    vm.push(Value::Str(Arc::new(e.msg)));
                }
                vm.ip = resume;
                return Ok(());
            }
            // No region left in this context, but a procedure *call* is still a
            // level even though it is not a region: `Op::Call` pushed the frame
            // and only `Op::ReturnValue` would have popped it.
            if e.level > 0 {
                match self.spend_call_level(&mut e) {
                    Some(true) => return Ok(()),
                    Some(false) => {
                        depth = self.vm(self.current).frames.len();
                        continue;
                    }
                    None => {}
                }
            }
            if self.current == 0 {
                return Err(e);
            }
            match self.discard(self.current) {
                Some(resumer) => self.current = resumer,
                None => return Err(e),
            }
        }
    }

    /// Spend one `return` level against the innermost procedure activation on
    /// this context's frame stack, ending the call when the code is then `ok`.
    ///
    /// [`call_in_chunk`] does exactly this where the boundary is a *chunk*
    /// boundary; this is the same ending for the calls the compiler lowered to
    /// `Op::Call`, which stay on one VM and are therefore invisible to it. The
    /// two answers a caller needs: `Some(true)` — the call returned and the VM
    /// is positioned to carry on; `Some(false)` — a level was spent and the code
    /// carries on outwards; `None` — there was no activation to spend it at.
    ///
    /// Only reachable from a `return` the compiler could not lower to
    /// `Op::ReturnValue`, which is one written inside a region — and until
    /// [`Compiler::finally_region`](crate::compiler::Compiler) that meant one
    /// inside a `catch`, which absorbs it before it ever gets here. A `return`
    /// out of a `dict update` body is the first that reaches this: without it
    /// `proc p {} {dict update d k v {return $d}}` ended the whole *script* with
    /// the procedure's result instead of ending the procedure (measured against
    /// tclsh 9.0.4, which prints the dictionary and carries on).
    fn spend_call_level(&mut self, e: &mut TclError) -> Option<bool> {
        let vm = self.vm(self.current);
        // Frames that enter no subroutine — the base frame, an `Op::PushFrame`
        // scope frame — are not Tcl levels, so they are dropped without one
        // being spent. `Frame::entry_ip` is what tells the two apart, as it does
        // for [`levels`].
        let mut activation = None;
        while let Some(frame) = vm.frames.last() {
            let frame = frame.clone();
            if frame.entry_ip.is_some() {
                activation = Some(frame);
                break;
            }
            vm.frames.pop();
            vm.stack.truncate(frame.stack_base);
        }
        let frame = activation?;
        vm.frames.pop();
        // The same ending `Op::ReturnValue` performs: the callee's stack goes,
        // the caller resumes past the call, and the value lands where the call's
        // would have.
        vm.stack.truncate(frame.stack_base);
        *e = std::mem::replace(e, TclError::plain(String::new())).descend();
        if e.visible_code() != TCL_OK {
            return Some(false);
        }
        vm.stack
            .push(Value::Str(Arc::new(std::mem::take(&mut e.msg))));
        vm.ip = frame.return_ip;
        Some(true)
    }

    /// The running coroutine's body returned: its value is the value of the
    /// call that resumed it, and the coroutine is gone.
    fn retire(&mut self, outcome: VMResult) {
        let result = match outcome {
            VMResult::Ok(v) => v,
            _ => Value::Str(Arc::new(String::new())),
        };
        let resumer = self
            .discard(self.current)
            .expect("a running coroutine has a resumer");
        self.vm(resumer).stack.push(result);
        self.current = resumer;
    }

    /// Delete a coroutine's context, answering where control returns to.
    fn discard(&mut self, context: usize) -> Option<usize> {
        if let Some(name) = self.contexts[context].name.take() {
            self.live.remove(&name);
        }
        self.contexts[context].vm = None;
        self.contexts[context].catches.clear();
        self.contexts[context].resumer.take()
    }

    /// Service one coroutine request. `Err` is a Tcl error raised in the
    /// context that made the request.
    fn service(&mut self, request: Request) -> Result<(), TclError> {
        self.service_inner(request).map_err(TclError::plain)
    }

    fn service_inner(&mut self, request: Request) -> Result<(), String> {
        match request {
            Request::Create {
                name,
                command,
                args,
            } => self.create(name, &command, args),
            Request::Resume { name, args } => {
                let target = self.suspended(&name)?;
                let value = self.resumption(target, &name, args)?;
                let resumer = self.current;
                self.enter(target, value, Some(resumer));
                Ok(())
            }
            Request::Yield(value) => {
                self.in_coroutine("yield")?;
                self.contexts[self.current].park = Park::AtYield;
                let resumer = self.contexts[self.current]
                    .resumer
                    .take()
                    .expect("a running coroutine has a resumer");
                self.vm(resumer).stack.push(value);
                self.current = resumer;
                Ok(())
            }
            Request::YieldTo { name, args } => {
                self.in_coroutine("yieldto")?;
                // `info coroutine` reports a qualified name and the juggler
                // example cedes control to exactly that; with one namespace,
                // `::c` and `c` are the same command.
                let name = name.strip_prefix("::").unwrap_or(&name).to_string();
                if !self.created.contains(&name) {
                    // A `yieldto` whose target is a word could name any
                    // command; only a coroutine of this script can be ceded to.
                    return Err(format!(
                        "\"yieldto {name}\": ceding control to a command that is not a \
                         coroutine of this script is not supported"
                    ));
                }
                let target = self.suspended(&name)?;
                // The argument check happens before control moves, so a bad
                // one is an error in the coroutine that wrote the `yieldto`.
                let value = self.resumption(target, &name, args)?;
                self.contexts[self.current].park = Park::AtYieldTo;
                // The target inherits this coroutine's resumer: whatever it
                // eventually produces is the value of the call that got us
                // here, and this coroutine now has nowhere of its own to
                // return to until something resumes it.
                let inherited = self.contexts[self.current].resumer.take();
                self.enter(target, value, inherited);
                Ok(())
            }
        }
    }

    /// `coroutine name command ?arg…?`: a fresh VM over the same chunk,
    /// positioned at the command's entry with the actual arguments below a
    /// frame that returns past the end of the program, so the body returning
    /// ends that VM's run.
    fn create(&mut self, name: String, command: &str, args: Vec<Value>) -> Result<(), String> {
        let entry = self
            .chunk
            .names
            .iter()
            .position(|n| n == command)
            .and_then(|idx| self.chunk.find_sub(idx as u16))
            .ok_or_else(|| format!("invalid command name \"{command}\""))?;

        let mut vm = VM::new((*self.chunk).clone());
        self.hooks.install(&mut vm);
        let base = vm.stack.len();
        for a in args {
            vm.stack.push(a);
        }
        vm.frames.push(Frame {
            return_ip: self.chunk.ops.len(),
            stack_base: base,
            slots: Vec::new(),
            // The body is a procedure of this chunk, so the frame carries which
            // one: that is what lets the VM answer a slot's name inside a
            // coroutine, as it does inside an ordinary call.
            entry_ip: Some(entry),
        });
        vm.ip = entry;

        // A name that is still live is being re-created: the old context goes,
        // as it does in the reference implementation.
        if let Some(&old) = self.live.get(&name) {
            self.discard(old);
        }
        let context = self.contexts.len();
        self.contexts.push(Context {
            vm: Some(vm),
            catches: Vec::new(),
            name: Some(name.clone()),
            resumer: Some(self.current),
            park: Park::Running,
        });
        self.created.insert(name.clone());
        self.live.insert(name, context);
        self.current = context;
        Ok(())
    }

    /// Give a suspended context its resumption value and run it, recording
    /// where it returns to.
    fn enter(&mut self, target: usize, value: Value, resumer: Option<usize>) {
        self.vm(target).stack.push(value);
        self.contexts[target].resumer = resumer;
        self.contexts[target].park = Park::Running;
        self.current = target;
    }

    /// The context of the suspended coroutine `name`.
    fn suspended(&self, name: &str) -> Result<usize, String> {
        let Some(&context) = self.live.get(name) else {
            return Err(format!("invalid command name \"{name}\""));
        };
        if self.contexts[context].park == Park::Running {
            return Err(format!("coroutine \"{name}\" is already running"));
        }
        Ok(context)
    }

    /// The single value a resumption delivers, which depends on how the
    /// coroutine suspended: `yield` produces its argument, so it takes at most
    /// one; `yieldto` produces the whole argument list.
    fn resumption(&self, target: usize, name: &str, args: Vec<Value>) -> Result<Value, String> {
        match self.contexts[target].park {
            Park::AtYieldTo => {
                let words: Vec<String> = args.iter().map(to_tcl_string).collect();
                Ok(Value::Str(Arc::new(list::join(&words))))
            }
            _ => match <[Value; 1]>::try_from(args) {
                Ok([value]) => Ok(value),
                Err(rest) if rest.is_empty() => Ok(Value::Str(Arc::new(String::new()))),
                Err(_) => Err(format!("wrong # args: should be \"{name} ?arg?\"")),
            },
        }
    }

    /// `yield` and `yieldto` are errors outside a coroutine — and are refused,
    /// for a different reason and with a different message, inside a script that
    /// a coroutine reached through `eval`, `uplevel` or `apply`.
    fn in_coroutine(&self, command: &str) -> Result<(), String> {
        if self.contexts[self.current].name.is_some() {
            return Ok(());
        }
        // Suspending here would have to park a VM that is waiting inside an op
        // handler, several Rust frames below this one: the nested script's state
        // is not part of what the outer VM saves when it parks, so resuming it
        // could not come back to the middle of this script. It is refused
        // outright rather than approximated, because every approximation loses
        // whatever the nested script had set.
        let nested = self
            .hooks
            .interp
            .lock()
            .expect("interpreter lock")
            .contexts
            .iter()
            .any(Option::is_some);
        if nested {
            return Err(format!(
                "{command} inside a script run by \"eval\", \"uplevel\" or \"apply\" is not \
                 supported: a coroutine cannot suspend across one"
            ));
        }
        Err(format!("{command} can only be called in a coroutine"))
    }
}

/// A Tcl number: integral until something forces a double.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Num {
    Int(i64),
    Float(f64),
    /// An integer past what an `i64` holds. Tcl 9's integers are arbitrary
    /// precision, so this is a value like any other rather than an error.
    ///
    /// It is reached only by a spelling that does not fit or by an operation
    /// that overflowed: the VM computes on `i64` in registers and hands the
    /// frontend the operands only when its checked arithmetic fails, or when a
    /// comparison of two numbers it holds natively would have to round to
    /// answer. Neither happens on a hot path, so an ordinary loop never builds
    /// one (`fusevm`'s `NumericHook`, and [`numeric`]).
    Big(BigInt),
}

impl Num {
    fn as_f64(&self) -> f64 {
        match self {
            Num::Int(i) => *i as f64,
            Num::Float(f) => *f,
            // `to_f64` saturates to an infinity for a magnitude no double can
            // hold, which is what Tcl answers for the same conversion.
            Num::Big(b) => b.to_f64().unwrap_or(f64::INFINITY),
        }
    }

    /// The value as a `BigInt`, for an operation with a bignum on either side.
    /// `None` for a double, which promotes the *other* side instead.
    fn as_big(&self) -> Option<BigInt> {
        match self {
            Num::Int(i) => Some(BigInt::from(*i)),
            Num::Big(b) => Some(b.clone()),
            Num::Float(_) => None,
        }
    }

    fn is_big(&self) -> bool {
        matches!(self, Num::Big(_))
    }

    /// Whether this is an integer of any width, as opposed to a double.
    ///
    /// The distinction that matters for ordering: a pair with an integer on
    /// either side is ordered exactly, and only a pair of two doubles is
    /// ordered as doubles. `is_big` is the wrong question there — an `i64`
    /// past 2^53 is no more representable as a double than a bignum is, and
    /// `expr {3**34 == double(3**34)}` is 0 in tclsh for exactly that reason.
    fn is_integral(&self) -> bool {
        matches!(self, Num::Int(_) | Num::Big(_))
    }
}

/// Order two numbers exactly, for any pair with an integer on either side.
///
/// Going through `f64` would be wrong in both directions: it rounds an integer
/// past 2^53 to the nearest double, which makes distinct integers compare
/// equal, and it cannot represent one larger than `f64::MAX` at all. Width is
/// not the test — `3**34` fits an `i64` and still rounds. `None` only for a
/// NaN, which has no ordering, and for a pair of two doubles, which has no
/// integer to compare against and belongs on the `f64` path.
pub(crate) fn big_cmp(p: &Num, q: &Num) -> Option<std::cmp::Ordering> {
    match (p, q) {
        (Num::Float(f), _) | (_, Num::Float(f)) if f.is_nan() => None,
        // An infinity is beyond every integer, so its side decides outright.
        (Num::Float(f), _) if f.is_infinite() => Some(if *f < 0.0 {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Greater
        }),
        (_, Num::Float(f)) if f.is_infinite() => Some(if *f < 0.0 {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Less
        }),
        // One side a finite double: compare against its integer part, and let
        // the fraction break a tie. `2 < 2.5` and `3 > 2.5` both fall out of
        // that, exactly, without either side becoming the other's type.
        (left, Num::Float(f)) => {
            let whole = BigInt::from_f64(f.trunc())?;
            Some(match left.as_big()?.cmp(&whole) {
                std::cmp::Ordering::Equal => 0.0.partial_cmp(&(f - f.trunc()))?,
                other => other,
            })
        }
        (Num::Float(f), right) => {
            let whole = BigInt::from_f64(f.trunc())?;
            Some(match whole.cmp(&right.as_big()?) {
                std::cmp::Ordering::Equal => (f - f.trunc()).partial_cmp(&0.0)?,
                other => other,
            })
        }
        (left, right) => Some(left.as_big()?.cmp(&right.as_big()?)),
    }
}

/// A `BigInt` as the value a script sees: an `i64` when it fits, and its
/// canonical decimal spelling when it does not.
///
/// Demoting matters as much as promoting. `expr {(1 << 64) >> 64}` is 1 in
/// tclsh, an ordinary integer again, and a value that stayed wide would compare
/// and print the same but would take the slow path on every later operation.
pub(crate) fn from_big(b: BigInt) -> Value {
    match i64::try_from(&b) {
        Ok(i) => Value::Int(i),
        Err(_) => Value::Str(Arc::new(b.to_string())),
    }
}

/// Why a string is not a number this frontend can use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotNumeric {
    /// No numeric spelling at all.
    Unparsable,
}

/// Interpret a value as a Tcl number. Leading and trailing whitespace is
/// allowed, as are the radix prefixes `0x`, `0o` and `0b`.
pub(crate) fn tcl_num(v: &Value) -> Result<Num, NotNumeric> {
    match v {
        Value::Int(i) => Ok(Num::Int(*i)),
        Value::Float(f) => Ok(Num::Float(*f)),
        Value::Bool(b) => Ok(Num::Int(*b as i64)),
        _ => parse_number(v.as_str_cow().trim()),
    }
}

/// A number, falling back to the nearest double for a spelling that is not one.
///
/// Only comparison uses this, and an integer of any width now parses exactly
/// through [`parse_number`], so the fallback is reached only by a spelling
/// `tcl_num` rejects outright. Ordering a pair with an integer on either side
/// goes through [`big_cmp`] instead, which is exact — the nearest double would
/// make distinct integers compare equal.
fn approx_num(v: &Value) -> Option<Num> {
    if let Ok(n) = tcl_num(v) {
        return Some(n);
    }
    let text = v.as_str_cow();
    let body = text.trim();
    let (sign, digits) = match body.strip_prefix('-') {
        Some(rest) => (-1.0, rest),
        None => (1.0, body.strip_prefix('+').unwrap_or(body)),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<f64>().ok().map(|f| Num::Float(sign * f))
}

pub(crate) fn parse_number(text: &str) -> Result<Num, NotNumeric> {
    if text.is_empty() {
        return Err(NotNumeric::Unparsable);
    }
    let (sign, body) = match text.as_bytes()[0] {
        b'-' => (-1i64, &text[1..]),
        b'+' => (1, &text[1..]),
        _ => (1, text),
    };
    // Tcl 9's radix prefixes. A leading zero is *not* one of them: `010` is ten,
    // as `0d10` is, which is why there is a `0d` at all.
    //
    // Matched on bytes. Slicing `&body[..2]` panics when the second character is
    // multi-byte — `héllo` has `é` across bytes 1..3 — and a condition reaches
    // this with whatever text a variable holds.
    let radix = match body.as_bytes() {
        [b'0', k, _, ..] => match k.to_ascii_lowercase() {
            b'x' => Some(16),
            b'o' => Some(8),
            b'b' => Some(2),
            b'd' => Some(10),
            _ => None,
        },
        _ => None,
    };

    // `_` is numeric whitespace, not part of any value.
    let cleaned;
    let body = if body.contains('_') {
        match without_separators(body, radix.unwrap_or(10)) {
            Some(text) => {
                cleaned = text;
                cleaned.as_str()
            }
            None => return Err(NotNumeric::Unparsable),
        }
    } else {
        body
    };

    if let Some(radix) = radix {
        let digits = &body[2..];
        return match i64::from_str_radix(digits, radix) {
            Ok(v) => Ok(Num::Int(sign * v)),
            // Digits of the right shape that do not fit are a bignum; a `0x`
            // with no valid digit at all is simply not a number.
            Err(_) if !digits.is_empty() && digits.chars().all(|c| c.is_digit(radix)) => {
                match BigInt::parse_bytes(digits.as_bytes(), radix) {
                    Some(b) => Ok(Num::Big(if sign < 0 { -b } else { b })),
                    None => Err(NotNumeric::Unparsable),
                }
            }
            Err(_) => Err(NotNumeric::Unparsable),
        };
    }
    if let Ok(i) = body.parse::<i64>() {
        return Ok(Num::Int(sign * i));
    }
    // An integer spelling that does not fit an `i64` is a bignum, and must not
    // fall through to the double parser — which would take it and answer with a
    // value the script never wrote.
    if !body.is_empty() && body.bytes().all(|b| b.is_ascii_digit()) {
        return match BigInt::parse_bytes(body.as_bytes(), 10) {
            Some(b) => Ok(Num::Big(if sign < 0 { -b } else { b })),
            None => Err(NotNumeric::Unparsable),
        };
    }
    // Tcl accepts Inf and NaN spellings that Rust's parser also takes; it does
    // not accept a bare `.` or an empty mantissa, and neither does Rust's.
    body.parse::<f64>()
        .map(|f| Num::Float(sign as f64 * f))
        .map_err(|_| NotNumeric::Unparsable)
}

/// Remove Tcl 9's numeric whitespace, or answer `None` when one of the `_` runs
/// is not where a separator may be.
///
/// `TclParseNumber` (`tclStrToD.c`) accepts a run of `_` only between two digits
/// of the number's own radix, and never at either end: `1_0`, `1__0`,
/// `1_000_000`, `0x1_0` and `1e1_0` are numbers, and `_1`, `1_`, `1_.5`,
/// `1_e3` and `0x_10` are not.
fn without_separators(body: &str, radix: u32) -> Option<String> {
    let bytes = body.as_bytes();
    let digit = |i: usize| -> bool { bytes.get(i).is_some_and(|b| (*b as char).is_digit(radix)) };
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'_' {
            i += 1;
            continue;
        }
        let run_start = i;
        while i < bytes.len() && bytes[i] == b'_' {
            i += 1;
        }
        if run_start == 0 || !digit(run_start - 1) || !digit(i) {
            return None;
        }
    }
    Some(body.replace('_', ""))
}

/// Tcl's boolean rule, which is not the VM's truthiness rule: a condition must
/// be a number or one of the words `true`, `false`, `yes`, `no`, `on`, `off`,
/// abbreviated to any non-ambiguous prefix and in any case.
///
/// Ported from `ParseBoolean` and `Tcl_GetBoolFromObj` (`tclObj.c`): the word
/// table is tried first, and anything it rejects is offered to the number
/// parser, so `007`, `0x10`, `1_0`, ` 1 ` and `1e3` are all true and `b`, `o`
/// and `""` are errors. `o` is an error because it is a prefix of both `on` and
/// `off`, which is why the two are only accepted from two characters up.
pub(crate) fn tcl_bool(v: &Value) -> Result<bool, String> {
    match v {
        Value::Int(i) => return Ok(*i != 0),
        Value::Bool(b) => return Ok(*b),
        Value::Float(f) => return float_bool(*f),
        _ => {}
    }
    let text = v.as_str_cow();
    if let Some(b) = boolean_word(&text) {
        return Ok(b);
    }
    match parse_number(text.trim()) {
        Ok(Num::Int(i)) => Ok(i != 0),
        Ok(Num::Float(f)) => float_bool(f),
        // A spelling too wide for an `i64` has a magnitude larger than
        // `i64::MAX`, so it is nonzero without consulting the digits.
        Ok(Num::Big(_)) => Ok(true),
        Err(NotNumeric::Unparsable) => Err(format!(
            "expected boolean value but got {}",
            named(&text, 50)
        )),
    }
}

fn float_bool(f: f64) -> Result<bool, String> {
    if f.is_nan() {
        return Err("floating point value is Not a Number".to_string());
    }
    Ok(f != 0.0)
}

/// `ParseBoolean`'s word table. `None` means "not one of the words", which is
/// the cue to try the number parser rather than to fail.
pub(crate) fn boolean_word(text: &str) -> Option<bool> {
    // "false" is the longest spelling, so nothing longer can be one of these —
    // and the reference implementation measures bytes, not characters.
    if text.is_empty() || text.len() > 5 {
        return None;
    }
    if text == "0" {
        return Some(false);
    }
    if text == "1" {
        return Some(true);
    }
    let lower = text.to_ascii_lowercase();
    // Only the letters the six words are spelled with; anything else is not a
    // word, which keeps `0x10` out of the prefix matching below.
    if !lower.bytes().all(|b| b"aeflnorstuy".contains(&b)) {
        return None;
    }
    for (word, value) in [
        ("yes", true),
        ("no", false),
        ("true", true),
        ("false", false),
        ("on", true),
        ("off", false),
    ] {
        // `on` and `off` share their first letter, so a one-character prefix of
        // either is ambiguous and rejected.
        let shortest = if word.starts_with('o') { 2 } else { 1 };
        if lower.len() >= shortest && word.starts_with(&lower) {
            return Some(value);
        }
    }
    None
}

/// How the reference interpreter names an unusable value in a diagnostic: `a
/// list` when the text could be one, and otherwise the text quoted and cut at
/// `limit` bytes.
pub(crate) fn named(text: &str, limit: usize) -> String {
    if list::looks_like_a_list(text) {
        return "a list".to_string();
    }
    let mut end = text.len().min(limit);
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("\"{}\"", &text[..end])
}

/// An integer operand, in the wording of the commands that want one — `incr`
/// and `format`'s integer conversions — rather than of `expr`'s operators.
pub(crate) fn tcl_int(v: &Value) -> Result<i64, String> {
    if let Value::Int(i) = v {
        return Ok(*i);
    }
    let text = to_tcl_string(v);
    match parse_number(text.trim()) {
        Ok(Num::Int(i)) => Ok(i),
        // The callers here want a machine integer specifically: `format`'s
        // integer conversions are C's, and tclsh itself narrows for them —
        // `format %d 99999999999999999999` is 1661992959 there, the low 32
        // bits. Narrowing silently is the one thing this frontend will not do,
        // so the operand is refused and the divergence is recorded rather than
        // guessed at (BUGS.md).
        Ok(Num::Big(_)) => Err(too_large()),
        _ => Err(format!("expected integer but got {}", named(&text, 50))),
    }
}

/// Add two Tcl integers, promoting on overflow, for a command that increments a
/// value of its own rather than through `expr`.
///
/// `dict incr` is the caller. It cannot use [`tcl_int`], which refuses a bignum
/// because `format`'s integer conversions must narrow or refuse; incrementing
/// past an `i64` is ordinary in Tcl — `dict incr d k` on 9223372036854775807 is
/// 9223372036854775808 there — so this promotes instead. Floats are refused with
/// the wording tclsh's own `incr` uses.
pub(crate) fn incr_text(current: &str, by: &str) -> Result<String, String> {
    let one = incr_operand(current)?;
    let other = incr_operand(by)?;
    if let (Num::Int(x), Num::Int(y)) = (&one, &other) {
        if let Some(sum) = x.checked_add(*y) {
            return Ok(sum.to_string());
        }
    }
    // Either side is already wide, or the sum left the range: both widen.
    let (x, y) = (
        one.as_big().expect("an integer is never a float here"),
        other.as_big().expect("an integer is never a float here"),
    );
    Ok(to_tcl_string(&from_big(x + y)))
}

/// One operand of [`incr_text`]: an integer in any of Tcl's spellings, refused
/// the way tclsh's `incr` refuses it.
fn incr_operand(text: &str) -> Result<Num, String> {
    match parse_number(text.trim()) {
        Ok(n) if !matches!(n, Num::Float(_)) => Ok(n),
        _ => Err(format!("expected integer but got {}", named(text, 50))),
    }
}

/// `incr`'s own wording for an operand that is not an integer.
///
/// `incr` takes an integer, so it names the value rather than the operator, and
/// it names whichever of the two is at fault — the variable first, since that is
/// the one tclsh reports when both are. `None` when neither operand explains the
/// failure, which leaves the arithmetic's own message in place rather than
/// inventing one.
fn incr_operand_error(a: &Value, b: &Value) -> Option<String> {
    for operand in [a, b] {
        // `Undef` is the variable not existing, which `incr` reads as zero — the
        // undef hook answered it deliberately for this site. Absent is not the
        // same as not-an-integer.
        if matches!(operand, Value::Undef) {
            continue;
        }
        // Integral of *any* width, not `tcl_int`'s machine integer: a promoted
        // value is still an integer, and `incr y -1` on 10^20 is arithmetic
        // tclsh performs rather than refuses.
        let integral = matches!(
            parse_number(to_tcl_string(operand).trim()),
            Ok(Num::Int(_)) | Ok(Num::Big(_))
        );
        if !integral {
            return Some(format!(
                "expected integer but got {}",
                named(&to_tcl_string(operand), 50)
            ));
        }
    }
    None
}

/// The numeric hook: fusevm calls this instead of answering an operation
/// itself. Three things bring it here, and only the first two are about an
/// operand the VM cannot use:
///
/// * an operand that is not a native number — a string, mostly — which Tcl
///   reads as a number when it parses as one and as text when it does not;
/// * an integer operation whose result left `i64`, which in Tcl 9 is a
///   promotion to arbitrary precision rather than an error;
/// * a *comparison* of two numbers the VM could compare but would have to
///   round to do it — an integer past 2^53 against a double. Only the
///   frontend knows whether its language wants the rounded answer; Tcl does
///   not, so this orders such a pair exactly (see [`big_cmp`]).
///
/// The third is a comparison rule and not an arithmetic one. Tcl's arithmetic
/// on the same pair *does* promote the integer to a double —
/// `expr {3**34 - double(3**34)}` is 0.0 in tclsh 9.0.4, not the exact 1 —
/// and the arms below keep it that way.
fn numeric(op: NumOp, a: &Value, b: &Value) -> Result<Value, String> {
    // Comparisons prefer numbers but fall back to string order, which is what
    // makes `expr {"10" < "9"}` false and `expr {10 < 9}` also false while
    // `expr {"abc" < "abd"}` is true.
    let cmp = matches!(
        op,
        NumOp::Lt | NumOp::Gt | NumOp::Le | NumOp::Ge | NumOp::Eq | NumOp::Ne
    );
    if cmp {
        let ordering = match (approx_num(a), approx_num(b)) {
            (Some(Num::Int(i)), Some(Num::Int(j))) => i.cmp(&j),
            // An integer on either side orders exactly, never through a
            // double. The difference is observable: `99999999999999999999 <
            // 1e20` is true and `== 1e20` is false, though both sides are the
            // same double once converted — while `1e20 ==
            // 100000000000000000000` is true, because that one really is the
            // same integer.
            //
            // Width is not what decides it. An `i64` past 2^53 rounds on the
            // way to a double just as a bignum does, so `3**34` and
            // `double(3**34)` are one apart and tclsh 9.0.4 answers 0 for `==`
            // and 1 for `>`. Reading either through an `f64` first would make
            // them the same value and answer both the other way. Two doubles
            // fall through to the arm below, which is where they belong: they
            // are already the values being compared, and `big_cmp` cannot
            // order a pair with no integer in it.
            (Some(p), Some(q)) if p.is_integral() || q.is_integral() => match big_cmp(&p, &q) {
                Some(ordering) => ordering,
                None => return Ok(Value::Int(matches!(op, NumOp::Ne) as i64)),
            },
            (Some(p), Some(q)) => match p.as_f64().partial_cmp(&q.as_f64()) {
                Some(ordering) => ordering,
                // A NaN operand has no ordering at all, and IEEE 754 is what
                // Tcl follows here: every ordered comparison against one is
                // false, and `!=` is the single one that is true. No `Ordering`
                // can express that — calling it `Greater` made `nan > 1` and
                // `nan >= 1` answer 1 where tclsh answers 0 — so answer here.
                None => return Ok(Value::Int(matches!(op, NumOp::Ne) as i64)),
            },
            _ => a.as_str_cow().cmp(&b.as_str_cow()),
        };
        let truth = match op {
            NumOp::Lt => ordering.is_lt(),
            NumOp::Gt => ordering.is_gt(),
            NumOp::Le => ordering.is_le(),
            NumOp::Ge => ordering.is_ge(),
            NumOp::Eq => ordering.is_eq(),
            _ => !ordering.is_eq(),
        };
        return Ok(Value::Int(truth as i64));
    }

    let sym = match op {
        NumOp::Add => "+",
        NumOp::Sub => "-",
        NumOp::Mul => "*",
        NumOp::Div => "/",
        NumOp::Mod => "%",
        NumOp::Pow => "**",
        NumOp::Neg => "-",
        _ => "?",
    };
    // `Neg` is the one unary op that reaches here, and its operand is `a`; every
    // other op names the side its bad operand was on.
    let unary = matches!(op, NumOp::Neg);
    let left = if unary { Side::Only } else { Side::Left };
    // `incr` on a variable that does not exist counts from zero — `proc p {}
    // {incr n; return $n}` is 1 in tclsh — and `incr` lowers to a native
    // `Op::Add` on the variable's value, deliberately, so that a counting loop
    // stays trace-eligible (see `Compiler::cmd_incr`). An absent variable
    // reaches this hook as `Value::Undef`, which no assignment can produce —
    // `set x ""` stores `Value::Str("")` — so reading it as zero is exactly the
    // `incr` case and not the empty string. The cost is that `expr {$unset +
    // 1}` answers 1 rather than refusing the operand; tclrs already reads an
    // absent variable as absent rather than raising (BUGS.md, allowlist A1),
    // and this is that same deviation reaching arithmetic.
    let zeroed = |v: &Value| matches!(op, NumOp::Add) && *v == Value::Undef;
    let x = if zeroed(a) {
        Num::Int(0)
    } else {
        num_operand(a, left, sym)?
    };
    // A unary op has no right operand, and reads as zero for the same reason an
    // absent one does: the arm below adds it to `x` and answers `x`.
    let y = if unary || zeroed(b) {
        Num::Int(0)
    } else {
        num_operand(b, Side::Right, sym)?
    };

    let value = match (op, &x, &y) {
        (NumOp::Neg, Num::Float(f), _) => Value::Float(-f),
        (NumOp::Neg, _, _) => from_big(-x.as_big().expect("a non-float negates as an integer")),
        // Either operand a double makes the result a double, bignum or not:
        // `expr {99999999999999999999 + 0.5}` is 1e+20 in tclsh.
        (_, Num::Float(_), _) | (_, _, Num::Float(_)) => {
            let (p, q) = (x.as_f64(), y.as_f64());
            Value::Float(match op {
                NumOp::Add => p + q,
                NumOp::Sub => p - q,
                NumOp::Mul => p * q,
                _ => return Err(format!("unsupported operation {sym}")),
            })
        }
        // Two integers, at least one of which the VM could not fold — either it
        // overflowed `i64` or it arrived as a spelling wider than one. This is
        // the whole bignum path: it is reached only after fusevm's checked
        // arithmetic has already failed, so a loop that never overflows never
        // builds a `BigInt` at all.
        _ => {
            let (p, q) = (
                x.as_big().expect("an integer operand"),
                y.as_big().expect("an integer operand"),
            );
            from_big(match op {
                NumOp::Add => p + q,
                NumOp::Sub => p - q,
                NumOp::Mul => p * q,
                _ => return Err(format!("unsupported integer operation {sym}")),
            })
        }
    };
    Ok(value)
}

/// Which operand of an operator a diagnostic is about. `expr(n)` words a
/// binary operator's two sides differently and a unary operator's only side
/// differently again, so the side travels with the refusal rather than being
/// guessed from the operator's spelling — `-` is both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Right,
    /// A unary operator's only operand: `as operand of`, with no side named.
    Only,
}

impl Side {
    fn phrase(self) -> &'static str {
        match self {
            Side::Left => "as left operand of",
            Side::Right => "as right operand of",
            Side::Only => "as operand of",
        }
    }
}

/// How `expr(n)` names an operand it will not compute on.
///
/// Three shapes, measured against tclsh 9.0.4:
///
/// ```text
/// expr {"a" + 1}   cannot use non-numeric string "a" as left operand of "+"
/// expr {1.5 % 2}   cannot use floating-point value "1.5" as left operand of "%"
/// expr {"a b" + 1} cannot use a list as left operand of "+"
/// expr {~1.5}      cannot use floating-point value "1.5" as operand of "~"
/// ```
///
/// A value that could hold several elements is named `a list` and never quoted,
/// which is [`looks_like_a_list`](crate::list::looks_like_a_list) — the same
/// screen `incr` and `format` use — and the text is *not* truncated here, unlike
/// theirs: `expr {[string repeat q 80] + 1}` quotes all eighty.
fn operand(v: &Value, kind: &str, side: Side, op: &str) -> String {
    let text = to_tcl_string(v);
    if list::looks_like_a_list(&text) {
        return format!("cannot use a list {} \"{op}\"", side.phrase());
    }
    format!("cannot use {kind} \"{text}\" {} \"{op}\"", side.phrase())
}

/// An operand that is not a number at all.
fn non_numeric(v: &Value, side: Side, op: &str) -> String {
    operand(v, "non-numeric string", side, op)
}

/// An operand that is a perfectly good double where the operator wants an
/// integer — `%` and the bitwise operators.
fn non_integer(v: &Value, side: Side, op: &str) -> String {
    operand(v, "floating-point value", side, op)
}

/// What an operator says about an operand it cannot use: the overflow this
/// frontend documents in place of a bignum, or the non-numeric refusal.
fn operand_error(why: NotNumeric, v: &Value, side: Side, op: &str) -> String {
    match why {
        NotNumeric::Unparsable => non_numeric(v, side, op),
    }
}

/// An operand of an arithmetic operator, refused in `expr(n)`'s words.
///
/// A NaN is named as its own third kind — `expr {"nan" + 0}` is `cannot use
/// non-numeric floating-point value "nan" as left operand of "+"` — because a
/// NaN reaching an operator is a refusal, where a NaN *produced* by one is the
/// domain error [`nan_checked`] reports.
fn num_operand(v: &Value, side: Side, op: &str) -> Result<Num, String> {
    match tcl_num(v) {
        Ok(Num::Float(f)) if f.is_nan() => {
            Err(operand(v, "non-numeric floating-point value", side, op))
        }
        Ok(n) => Ok(n),
        Err(why) => Err(operand_error(why, v, side, op)),
    }
}

/// The refusal for an integer this frontend will not build: past `i64` is a
/// promotion now, but past [`MAX_INT_BITS`] is still an error, and so is a
/// value handed to a command that wants a machine integer specifically.
fn too_large() -> String {
    "integer value too large to represent".to_string()
}

/// The frontend's extension ops.
fn extension(vm: &mut VM, id: u16, arg: u8) -> Result<(), String> {
    match id {
        ext::DIV | ext::POW => {
            let b = vm.pop();
            let a = vm.pop();
            let x = num_operand(&a, Side::Left, sym_of(id))?;
            let y = num_operand(&b, Side::Right, sym_of(id))?;
            vm.push(arith(id, x, y)?);
            Ok(())
        }
        // `%` wants two integers, and checks the left operand completely before
        // looking at the right: `expr {1.5 % "a"}` reports the float, not the
        // string (measured against tclsh 9.0.4).
        ext::MOD => {
            let b = vm.pop();
            let a = vm.pop();
            let x = match big_operand(&a, Side::Left, "%")? {
                BigOperand::Int(i) => Num::Int(i),
                BigOperand::Big(b) => Num::Big(b),
            };
            let y = match big_operand(&b, Side::Right, "%")? {
                BigOperand::Int(i) => Num::Int(i),
                BigOperand::Big(b) => Num::Big(b),
            };
            vm.push(arith(id, x, y)?);
            Ok(())
        }
        ext::BIT_AND | ext::BIT_OR | ext::BIT_XOR => {
            let b = vm.pop();
            let a = vm.pop();
            let sym = sym_of(id);
            let x = big_operand(&a, Side::Left, sym)?;
            let y = big_operand(&b, Side::Right, sym)?;
            // Two `i64`s answer as one, which is every ordinary script; a
            // bignum on either side widens both, since `num-bigint`'s bitwise
            // operators are two's-complement over an infinite sign extension —
            // the same model Tcl's are (`expr {99999999999999999999 & 255}` is
            // 255 there, and `~99999999999999999999` is negative).
            let value = match (x, y) {
                (BigOperand::Int(x), BigOperand::Int(y)) => Value::Int(match id {
                    ext::BIT_AND => x & y,
                    ext::BIT_OR => x | y,
                    _ => x ^ y,
                }),
                (x, y) => {
                    let (x, y) = (x.into_big(), y.into_big());
                    from_big(match id {
                        ext::BIT_AND => x & y,
                        ext::BIT_OR => x | y,
                        _ => x ^ y,
                    })
                }
            };
            vm.push(value);
            Ok(())
        }
        ext::SHL | ext::SHR => {
            let b = vm.pop();
            let a = vm.pop();
            let sym = sym_of(id);
            let x = big_operand(&a, Side::Left, sym)?;
            // The distance is an `i64` in every case: tclsh refuses a negative
            // one, and a positive one wide enough not to fit would ask for a
            // value no memory holds.
            let by = int_operand(&b, Side::Right, sym)?;
            vm.push(shift(id, x, by)?);
            Ok(())
        }
        ext::BIT_NOT => {
            let a = vm.pop();
            let value = match big_operand(&a, Side::Only, "~")? {
                BigOperand::Int(i) => Value::Int(!i),
                BigOperand::Big(b) => from_big(!b),
            };
            vm.push(value);
            Ok(())
        }
        // Tcl's boolean rule, which the VM's own truthiness is not: the value a
        // condition produced is 1 or 0, or the condition is refused.
        ext::BOOL => {
            let v = vm.pop();
            let truth = if arg == 1 {
                // `!`, whose refusal is an operand error rather than a boolean
                // one, because `expr(n)` gives it a numeric operand and accepts
                // a boolean word only as a second reading.
                match tcl_num(&v) {
                    Ok(Num::Int(i)) => i == 0,
                    // A NaN is `!`'s operand refusal, not the boolean rule's
                    // "floating point value is Not a Number" — which is what a
                    // *condition* answers for the same value (`if {"nan"} …`).
                    Ok(Num::Float(f)) if f.is_nan() => {
                        return Err(operand(
                            &v,
                            "non-numeric floating-point value",
                            Side::Only,
                            "!",
                        ))
                    }
                    Ok(Num::Float(f)) => !float_bool(f)?,
                    // `!` of a bignum is 0: it is nonzero by construction, so
                    // negating its truth needs none of its digits.
                    Ok(Num::Big(_)) => false,
                    Err(NotNumeric::Unparsable) => !boolean_word(&v.as_str_cow())
                        .ok_or_else(|| non_numeric(&v, Side::Only, "!"))?,
                }
            } else {
                tcl_bool(&v)?
            };
            vm.push(Value::Int(truth as i64));
            Ok(())
        }
        // Membership is a string test against the list's elements: `1 in {01}`
        // is false even though the two are numerically equal.
        ext::IN | ext::NI => {
            let haystack = vm.pop();
            let needle = vm.pop();
            // Tcl's string form of the list, not the VM's: a double reaching
            // here as a `Value::Float` — which a literal operand now does —
            // spells itself `3` through `as_str_cow` and `3.0` through Tcl's
            // formatter, and the membership test is on the latter.
            let elements = crate::list::split(&to_tcl_string(&haystack))?;
            let needle = to_tcl_string(&needle);
            let found = elements.contains(&needle);
            vm.push(Value::Int(i64::from(found == (id == ext::IN))));
            Ok(())
        }
        // `expr`'s always-string comparisons, on Tcl's string form of each
        // operand rather than the VM's.
        ext::STR_CMP => {
            let b = to_tcl_string(&vm.pop());
            let a = to_tcl_string(&vm.pop());
            let hit = match arg {
                0 => a < b,
                1 => a > b,
                2 => a <= b,
                3 => a >= b,
                4 => a == b,
                _ => a != b,
            };
            vm.push(Value::Int(hit as i64));
            Ok(())
        }
        // The value an `expr` answers with when its result is a bare operand
        // rather than something arithmetic: the *number* the operand spells,
        // and a refusal if that number is a NaN.
        //
        // This is what the old normalizing op did after every expression. It is
        // emitted only where [`crate::compiler::Compiler::yields_number`] says
        // the result could still be a string — never after arithmetic — so a
        // counted loop keeps a body of native ops and the tracing JIT keeps it.
        ext::CANON => {
            let v = vm.pop();
            let canonical = match v {
                // A double is already a number; what it still needs is the NaN
                // refusal and Tcl's spelling.
                Value::Float(f) => Value::Str(Arc::new(nan_checked(f)?)),
                other => canonical_number(other)?,
            };
            vm.push(canonical);
            Ok(())
        }
        // Unary `+`: the identity on a number, a refusal on anything else. The
        // number it answers with is the canonical one, as `expr {+007}` is 7.
        ext::UPLUS => {
            let v = vm.pop();
            // `num_operand` rather than `tcl_num`: a NaN operand is `+`'s own
            // refusal, and reaching `canonical_number` with one would report the
            // domain error a NaN *result* gets instead.
            num_operand(&v, Side::Only, "+")?;
            vm.push(canonical_number(v)?);
            Ok(())
        }
        ext::MATCH => {
            let pattern = to_tcl_string(&vm.pop());
            let subject = to_tcl_string(&vm.pop());
            let hit = if arg == 1 {
                list::glob_match(&pattern, &subject)
            } else {
                subject == pattern
            };
            vm.push(Value::Int(hit as i64));
            Ok(())
        }
        // `error` and `return -code error` raise the message as the error, so
        // the enclosing `catch` — or the caller of `eval` — receives it.
        ext::ERROR => {
            // `error`'s `errorInfo` and `errorCode` words, off the stack in the
            // order they were pushed; see [`ext::ERROR`] for why they are
            // evaluated and then dropped.
            for _ in 0..arg {
                vm.pop();
            }
            Err(to_tcl_string(&vm.pop()))
        }
        // `throw type message`. The type has to be a list of at least one
        // element — `Tcl_ThrowObjCmd` asks `TclListObjLength` and then its own
        // length test — and the message is then raised as an ordinary error.
        ext::THROW => {
            let message = to_tcl_string(&vm.pop());
            let kind = to_tcl_string(&vm.pop());
            match list::split(&kind) {
                Err(e) => Err(e),
                Ok(items) if items.is_empty() => Err("type must be non-empty list".to_string()),
                Ok(_) => Err(message),
            }
        }
        // The ranges are tested from the highest base down, so that a lower
        // one's `id >= BASE` does not swallow a higher module's ops.
        //
        // Every block above [`ext::REGEXP_BASE`] is tested as a bounded range,
        // not as `id >= BASE` — the `info` block's arm below is what would
        // otherwise claim `encoding`'s ids, and `regexp`'s would claim all of
        // them. `tests/ext_ids.rs` pins the map they are numbered from.
        // ── the info block ───────────────────────────────────────────────
        // A bounded range for the same reason the encoding one below is bounded:
        // this is now the highest block allocated, and an open-ended
        // `id >= INFO_BASE` would claim whatever block is added above it next.
        id if (ext::INFO_BASE..ext::INFO_END).contains(&id) => {
            crate::cmd_info::extension(vm, id, arg)
        }
        // ── end of the info block ────────────────────────────────────────
        // ── the binary block ─────────────────────────────────────────────
        // Bounded, and ahead of the two open-ended arms below it, for the
        // reason the `info` arm above states.
        id if crate::cmd_binary::is_op(id) => crate::cmd_binary::extension(vm, id, arg),
        // ── end of the binary block ──────────────────────────────────────
        // ── the encoding block ───────────────────────────────────────────
        id if crate::cmd_encoding::is_op(id) => crate::cmd_encoding::extension(vm, id, arg),
        // ── end of the encoding block ────────────────────────────────────
        id if id >= ext::FILE_BASE => crate::cmd_file::extension(vm, id, arg),
        id if id >= ext::CLOCK_BASE => crate::cmd_clock::extension(vm, id, arg),
        id if id >= ext::MATH_BASE => crate::expr_math::extension(vm, id, arg),
        id if id >= ext::REGEXP_BASE => crate::regexp::extension(vm, id, arg),
        id if id >= ext::STRING_BASE => crate::cmd_string::extension(vm, id, arg),
        id if id >= ext::ASSOC_BASE => crate::assoc::extension(vm, id, arg),
        id if id >= ext::LIST_BASE => crate::cmd_list::run(vm, id, arg),
        other => Err(format!("unknown extension op {other}")),
    }
}

fn sym_of(id: u16) -> &'static str {
    match id {
        ext::DIV => "/",
        ext::MOD => "%",
        ext::BIT_AND => "&",
        ext::BIT_OR => "|",
        ext::BIT_XOR => "^",
        ext::SHL => "<<",
        ext::SHR => ">>",
        ext::BIT_NOT => "~",
        _ => "**",
    }
}

/// An operand of an integer-only operator — `%`, `&`, `|`, `^`, `<<`, `>>`, `~`
/// — refused in `expr(n)`'s own words when it is anything else.
///
/// The two refusals are distinct and both are the operator's, not a command's:
/// a string that is no number at all is `non-numeric string`, and a perfectly
/// good double is `floating-point value`. fusevm's native `Op::BitAnd` and
/// friends would take either, coercing through `Value::to_int` — `expr {1.5 |
/// 2}` answered 3 — so these operators are lowered to extension ops whenever
/// the compiler cannot prove both operands integral
/// ([`crate::compiler::Compiler::yields_integer`]).
fn int_operand(v: &Value, side: Side, op: &str) -> Result<i64, String> {
    match big_operand(v, side, op)? {
        BigOperand::Int(i) => Ok(i),
        // Every caller of this either handles a bignum itself before asking, or
        // is an operator with no bignum meaning; none can answer from a
        // truncation, so reaching here with one is a bug rather than a script
        // error.
        BigOperand::Big(b) => Err(format!("integer value too large to represent: {b}")),
    }
}

/// An integer operand that may be wider than an `i64`.
enum BigOperand {
    Int(i64),
    Big(BigInt),
}

impl BigOperand {
    fn into_big(self) -> BigInt {
        match self {
            BigOperand::Int(i) => BigInt::from(i),
            BigOperand::Big(b) => b,
        }
    }
}

/// The same refusals as [`int_operand`], with a bignum allowed through.
fn big_operand(v: &Value, side: Side, op: &str) -> Result<BigOperand, String> {
    match num_operand(v, side, op)? {
        Num::Int(i) => Ok(BigOperand::Int(i)),
        Num::Big(b) => Ok(BigOperand::Big(b)),
        Num::Float(_) => Err(non_integer(v, side, op)),
    }
}

/// `<<` and `>>` in tclsh 9.0.4's semantics.
///
/// A negative distance is refused outright. A left shift grows the value rather
/// than losing bits off the top — `1 << 64` is 18446744073709551616 — which is
/// what makes this a bignum operation and not an `i64` one. A right shift is
/// arithmetic and *saturates* rather than wrapping the distance: `1 >> 200` is
/// 0 and `-1 >> 200` is -1, where Rust's `>>` would mask the distance to
/// 200 % 64 = 8.
fn shift(id: u16, value: BigOperand, by: i64) -> Result<Value, String> {
    if by < 0 {
        return Err("negative shift argument".to_string());
    }
    if id == ext::SHR {
        return Ok(match value {
            // Every bit has left the word; only the sign remains.
            BigOperand::Int(v) if by >= 63 => Value::Int(if v < 0 { -1 } else { 0 }),
            BigOperand::Int(v) => Value::Int(v >> by),
            // A bignum has no word to leave, so the distance is used as given;
            // `>>` on a `BigInt` is already arithmetic.
            BigOperand::Big(b) => from_big(b >> shift_distance(by)?),
        });
    }
    Ok(match value {
        BigOperand::Int(0) => Value::Int(0),
        // The `i64` fast case, kept exact: `checked_shl` bounds the distance but
        // not the value, so the round trip is what says a bit was lost. Losing
        // one means the answer is wider than an `i64` and the shift is redone
        // as a bignum.
        BigOperand::Int(v) if by < 64 => match v
            .checked_shl(by as u32)
            .filter(|shifted| shifted >> by == v)
        {
            Some(shifted) => Value::Int(shifted),
            None => from_big(BigInt::from(v) << shift_distance(by)?),
        },
        BigOperand::Int(v) => from_big(BigInt::from(v) << shift_distance(by)?),
        BigOperand::Big(b) => from_big(b << shift_distance(by)?),
    })
}

/// How wide a promoted integer may get before this frontend refuses to build
/// it: 2^20 bits, a little over 315,000 decimal digits.
///
/// The bound is this frontend's, not Tcl's, and it is the same trade
/// `expr::MAX_EXPR_DEPTH` already makes. tclsh has no bound: `expr {10 **
/// 123456789}` asks it for a 123-million-digit number and it will sit there
/// trying — measured, still running after 30 seconds, where `10 ** 100000`
/// takes 3.5. A script that asks for that has almost always made a mistake, and
/// a Tcl error it can catch is a better answer than an allocation that ends the
/// process. Everything tclsh computes in reasonable time is well inside this:
/// `10 ** 100000` is 332,193 bits.
const MAX_INT_BITS: u64 = 1 << 20;

/// A shift distance as `num-bigint` takes it, bounded by [`MAX_INT_BITS`].
fn shift_distance(by: i64) -> Result<usize, String> {
    if by as u64 > MAX_INT_BITS {
        return Err(int_too_wide());
    }
    Ok(by as usize)
}

fn int_too_wide() -> String {
    "integer value too large to represent".to_string()
}

/// `/`, `%` and `**` where an `i64` cannot hold an operand or the answer.
///
/// The semantics are the same ones the `i64` arms implement, which is the point:
/// division and remainder floor toward negative infinity rather than truncating
/// toward zero, so `-99999999999999999999 / 7` is -14285714285714285715 and the
/// remainder is 6, both measured against tclsh 9.0.4.
fn big_arith(id: u16, p: BigInt, q: BigInt) -> Result<Value, String> {
    if matches!(id, ext::DIV | ext::MOD) && q.is_zero() {
        return Err("divide by zero".to_string());
    }
    match id {
        ext::DIV | ext::MOD => {
            // `BigInt`'s `/` and `%` truncate, as Rust's do. Floor by hand: a
            // remainder whose sign differs from the divisor's is one step past
            // the floor.
            let (quotient, remainder) = (&p / &q, &p % &q);
            let stepped = !remainder.is_zero() && (remainder.is_negative() != q.is_negative());
            Ok(if id == ext::DIV {
                from_big(if stepped { quotient - 1 } else { quotient })
            } else {
                from_big(if stepped { remainder + &q } else { remainder })
            })
        }
        _ => {
            if q.is_negative() {
                // An integral base raised to a negative power truncates toward
                // zero, and only ±1 survives it — the same rule the `i64` arm
                // applies, and a bignum base is never ±1.
                return match () {
                    _ if p.is_zero() => Err("exponentiation of zero by negative power".to_string()),
                    _ => Ok(Value::Int(0)),
                };
            }
            let exp = u32::try_from(&q).map_err(|_| "exponent too large".to_string())?;
            // The width of the answer is the base's width times the exponent,
            // and it is knowable before a single digit is computed — which is
            // the only point at which refusing is still cheap.
            if p.bits() * u64::from(exp) > MAX_INT_BITS {
                return Err(int_too_wide());
            }
            Ok(from_big(p.pow(exp)))
        }
    }
}

/// Integer division and remainder floor toward negative infinity — `-57 / 10`
/// is -6 and `-57 % 10` is 3 — and `**` keeps integral operands integral.
fn arith(id: u16, x: Num, y: Num) -> Result<Value, String> {
    // A bignum on either side of an integer operator, before the `i64` arms
    // below: those are the fast path and stay exactly as they were.
    if matches!(id, ext::DIV | ext::MOD | ext::POW) && (x.is_big() || y.is_big()) {
        if let (Some(p), Some(q)) = (x.as_big(), y.as_big()) {
            return big_arith(id, p, q);
        }
    }
    match (id, x, y) {
        (ext::DIV, Num::Int(_), Num::Int(0)) | (ext::MOD, Num::Int(_), Num::Int(0)) => {
            Err("divide by zero".to_string())
        }
        // `i64::MIN / -1` is the one integer division whose true quotient does
        // not fit an `i64`; Tcl's answer is the bignum, and now so is this one.
        (ext::DIV, Num::Int(i64::MIN), Num::Int(-1)) => Ok(from_big(-BigInt::from(i64::MIN))),
        (ext::DIV, Num::Int(i), Num::Int(j)) => Ok(Value::Int(
            i.div_euclid(j)
                - i64::from(
                    // div_euclid rounds toward negative infinity only for a positive
                    // divisor; for a negative one it rounds the other way.
                    j < 0 && i.rem_euclid(j) != 0,
                ),
        )),
        (ext::MOD, Num::Int(i), Num::Int(j)) => {
            // The same pair overflows `%` on the way to a remainder that is
            // plainly 0, so answer directly instead of computing it.
            let r = i.checked_rem(j).unwrap_or(0);
            Ok(Value::Int(if r != 0 && (r < 0) != (j < 0) {
                r + j
            } else {
                r
            }))
        }
        // An exponent past what `checked_pow` even takes is its own diagnostic
        // in tclsh 9.0.4 — `expr {2 ** 9999999999}` is "exponent too large",
        // not the overflow the product would report.
        (ext::POW, Num::Int(i), Num::Int(j)) if j >= 0 => {
            let exp = u32::try_from(j).map_err(|_| "exponent too large".to_string())?;
            match i.checked_pow(exp) {
                Some(v) => Ok(Value::Int(v)),
                // The product left `i64`, which is a promotion and not an
                // error: `expr {2 ** 100}` is exact in tclsh.
                None => big_arith(id, BigInt::from(i), BigInt::from(j)),
            }
        }
        // Integral operands keep an integral result even when the exponent is
        // negative, so the true value is truncated toward zero: `2 ** -1` is 0,
        // not 0.5. Only ±1 survives, and 1/0 has no value at all. Measured
        // against tclsh 9.0.4, which answers 0 / 1 / -1 / the error here and
        // uses `powf` only when an operand is itself a double.
        (ext::POW, Num::Int(i), Num::Int(j)) => match i {
            0 => Err("exponentiation of zero by negative power".to_string()),
            1 => Ok(Value::Int(1)),
            // `j` may be `i64::MIN`, whose `abs()` does not fit, so read the
            // parity off the low bit rather than off a negated copy.
            -1 => Ok(Value::Int(if j % 2 == 0 { 1 } else { -1 })),
            _ => Ok(Value::Int(0)),
        },
        (ext::DIV, p, q) => float_result(p.as_f64() / q.as_f64()),
        // `%` never reaches here with a double: `int_operand` refused it, in the
        // order tclsh checks the two sides.
        (ext::MOD, _, _) => unreachable!("`%` operands are integers by now"),
        // A double operand anywhere makes the result a double, and a zero base
        // raised to a negative power still has no value.
        (_, p, q) => {
            if p.as_f64() == 0.0 && q.as_f64() < 0.0 {
                return Err("exponentiation of zero by negative power".to_string());
            }
            float_result(p.as_f64().powf(q.as_f64()))
        }
    }
}

/// The storage a variable lives in, grown to reach it — the same growth
/// `VM::set_var` and `VM::set_slot` do, which those cannot be used for here
/// because both hand back a clone rather than the value itself.
///
/// An op that *takes* the value out of this leaves it unshared, which is what
/// lets `lappend` and `append` extend the string the variable already holds
/// instead of building a copy of it every time (`crate::cmd_list`,
/// `crate::cmd_string`). `None` only for a frame slot with no frame.
pub(crate) fn var_cell(vm: &mut VM, place: Place) -> Option<&mut Value> {
    match place {
        Place::Global(index) => {
            let index = index as usize;
            if index >= vm.globals.len() {
                vm.globals.resize(index + 1, Value::Undef);
            }
            Some(&mut vm.globals[index])
        }
        Place::Slot(slot) => {
            let frame = vm.frames.last_mut()?;
            let slot = slot as usize;
            if slot >= frame.slots.len() {
                frame.slots.resize(slot + 1, Value::Undef);
            }
            Some(&mut frame.slots[slot])
        }
        // A name `upvar` bound: the slot holds a descriptor, and the cell is
        // wherever that descriptor points. Every op that reaches a variable
        // itself follows the link here, which is what makes one `upvar` serve
        // `set`, `$`, `incr`, `append`, `lappend`, `unset` and the `array`
        // subcommands alike — see [`crate::cmd_scope`].
        Place::Link(slot) => {
            let link = crate::cmd_scope::link_at(vm, slot)?;
            crate::cmd_scope::write_link(vm, &link)
        }
    }
}

/// An `expr` result that is a double: its Tcl spelling, unless it is a NaN,
/// which `expr(n)` reports rather than answers.
///
/// `expr {0.0/0.0}`, `expr {inf-inf}` and `expr {nan}` are all `domain error:
/// argument not in valid range` in tclsh 9.0.4 — measured, not inferred from
/// the C library's errno.
/// A double an arithmetic extension op computed, refused when it is a NaN.
///
/// tclsh raises at the operation that *produces* the NaN, not where the value
/// is later used: `set x [expr {0/0.0}]` never reaches the next command, and
/// `expr {(inf-inf) < 1}` reports rather than answering 0. `/` and `**` are the
/// two operators here that can make one out of operands that were not NaN
/// themselves; `+`, `-` and `*` are native ops and are covered by
/// `compiler::Compiler::may_be_non_finite` instead.
fn float_result(f: f64) -> Result<Value, String> {
    if f.is_nan() {
        return Err("domain error: argument not in valid range".to_string());
    }
    Ok(Value::Float(f))
}

fn nan_checked(f: f64) -> Result<String, String> {
    if f.is_nan() {
        return Err("domain error: argument not in valid range".to_string());
    }
    Ok(format_double(f))
}

/// The value an `expr` answers with when its result is a bare operand.
///
/// Tcl's `expr` yields the *number* an operand spells, not the text that spelt
/// it: `expr {007}` is 7, `expr {0x10}` is 16, `expr {" 42 "}` is 42 and `expr
/// {1e3}` is 1000.0. A string that spells no number at all is its own value —
/// `expr {"abc"}` is `abc` — and so is an integer too large for an `i64`, which
/// is the one case where the text is the only representation this frontend has
/// (see the note in `expr.rs` on decimal literals).
fn canonical_number(v: Value) -> Result<Value, String> {
    if matches!(v, Value::Int(_)) {
        return Ok(v);
    }
    let text = v.as_str_cow();
    match parse_number(text.trim()) {
        Ok(Num::Int(i)) => Ok(Value::Int(i)),
        Ok(Num::Float(f)) => Ok(Value::Str(Arc::new(nan_checked(f)?))),
        // `expr {0x1ffffffffffffffff}` is its decimal value in tclsh, as every
        // other radix spelling is; the canonical form of a bignum is the same
        // decimal `from_big` writes.
        Ok(Num::Big(b)) => Ok(from_big(b)),
        Err(_) => {
            drop(text);
            Ok(v)
        }
    }
}

/// Take a variable's value, leaving its place empty.
///
/// A value is taken rather than read when it is about to be changed in place,
/// which needs its string unshared — so the list-splitting cache in
/// [`crate::cmd_list`], whose entries are shares, is emptied first. This is the
/// one door a value leaves its variable through, which is why the call is here
/// rather than at each of the three commands that grow one. See
/// `crate::cmd_list::SPLIT` for what the invariant is worth.
pub(crate) fn take_var(vm: &mut VM, place: Place) -> Value {
    crate::cmd_list::forget_split();
    match var_cell(vm, place) {
        Some(value) => std::mem::replace(value, Value::Undef),
        None => Value::Undef,
    }
}

/// Where an in-place op was told its variable lives: the operand the compiler
/// pushed, read back as a [`Place`].
pub(crate) fn place_of(vm: &mut VM, slot_form: bool) -> Result<Place, String> {
    let operand = vm.pop();
    place_at(&operand, slot_form)
}

/// The same, for an operand read where it sits on the stack.
pub(crate) fn place_at(operand: &Value, slot_form: bool) -> Result<Place, String> {
    match operand {
        // The frame form carries a third case in its sign: a link is written
        // `-(slot + 1)`, which the non-negative slot range cannot reach. See
        // [`Place::frame_operand`].
        Value::Int(index) if slot_form && *index < 0 => Ok(Place::Link((-index - 1) as u16)),
        Value::Int(index) => Ok(if slot_form {
            Place::Slot(*index as u16)
        } else {
            Place::Global(*index as u16)
        }),
        other => Err(format!("not a variable place: {other:?}")),
    }
}

/// The reads a lowering marked as tolerating an unset variable, keyed by the
/// chunk they belong to.
///
/// An op index alone does not identify a read: `eval` compiles a chunk of its
/// own whose indices start at zero again, so a set keyed by index would answer
/// for the wrong script — `eval {...}` followed by `incr counter` on an unset
/// counter would refuse where Tcl initialises. The key is fusevm's own
/// [`fusevm::UndefRead::chunk`] identity, and entries accumulate rather than
/// replacing, because a cached chunk can be run long after a later one was
/// lowered.
static TOLERANT_READS: Mutex<Option<HashSet<(u64, usize)>>> = Mutex::new(None);

/// fusevm's identity for a chunk: its ops **and** its names.
///
/// Deliberately not `Chunk::op_hash`, which ignores the name pool because it
/// keys the JIT's native-code cache, where a name is only an index. `incr x`
/// and `set y [expr {$z + 1}]` lower to the same op vector and disagree about
/// which read tolerates an unset variable, so a key that ignored names would
/// merge exactly the two this set exists to separate.
///
/// This must agree with `VM::chunk_identity`; the tests below run a script
/// through both sides, so a drift in either shows up as a refusal where Tcl
/// initialises rather than as a silent mismatch.
pub(crate) fn chunk_identity(chunk: &fusevm::Chunk) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    chunk.op_hash.hash(&mut h);
    chunk.names.hash(&mut h);
    h.finish() | 1
}

/// The arithmetic sites an `incr` lowered, as `(chunk identity, op index)`.
///
/// `incr x` and `expr {$x + 1}` are the same `Op::Add` on the same value, and
/// the reference interpreter refuses them in different words: `expected integer
/// but got "abc"` against `cannot use non-numeric string "abc" as left operand
/// of "+"`. Only the site separates them, which is what
/// `fusevm::NumericCall::ip` is for. Same shape as [`TOLERANT_READS`], and
/// accumulating for the same reason: a cached chunk runs long after a later one
/// was lowered.
static INCR_SITES: Mutex<Option<HashSet<(u64, usize)>>> = Mutex::new(None);

/// What `info args`, `info default` and `info body` need about one procedure:
/// each formal's name and its default, in declaration order, and the body's
/// source text.
#[derive(Clone, Debug, Default)]
pub(crate) struct ProcParams {
    pub(crate) params: Vec<(String, Option<String>)>,
    /// `None` when the definition computed its body — see
    /// [`crate::procs::Signature::body`].
    pub(crate) body: Option<String>,
}

/// Every procedure a chunk defines, keyed by chunk identity.
///
/// The compiler already collects signatures before it emits anything, to check
/// a call's arity; this publishes them so `info` can answer for a *computed*
/// procedure name as well as a literal one. Same shape as [`TOLERANT_READS`] and
/// [`INCR_SITES`], accumulating for the same reason: a cached chunk runs long
/// after a later one was lowered.
static PROC_TABLE: Mutex<Option<HashMap<(u64, String), ProcParams>>> = Mutex::new(None);

/// Record the procedures `chunk` defines and what their formals are.
pub(crate) fn note_procs(chunk: &fusevm::Chunk, procs: &[(String, ProcParams)]) {
    if procs.is_empty() {
        return;
    }
    let id = chunk_identity(chunk);
    let mut guard = PROC_TABLE.lock().expect("proc table lock");
    let table = guard.get_or_insert_with(HashMap::new);
    for (name, params) in procs {
        table.insert((id, name.clone()), params.clone());
    }
}

/// The body text of `name` as the running chunk declared it — `info body`.
pub(crate) fn proc_body(vm: &VM, name: &str) -> Option<String> {
    proc_params(vm, name)?.body
}

/// Tcl's level number for the code that is running: 0 at the script's own level
/// and one more per procedure activation.
///
/// [`levels`] rather than `vm.frames.len()`, because fusevm pushes frames that
/// are not Tcl levels — the base frame, an `Op::PushFrame` scope frame, and one
/// materialized after a JIT side exit. Counting those made `uplevel 1` at the
/// top level find the base frame and answer where tclsh reports `bad level "1"`,
/// and it is the same count `info level`, `uplevel` and `upvar` must agree on.
pub(crate) fn current_level(vm: &VM) -> i64 {
    levels(vm).len() as i64
}

/// The absolute VM frame index of Tcl level `level`, or `None` when there is no
/// such level. Level 0 is the script's own, which is no frame at all.
///
/// The inverse of [`current_level`]: level `current` is the innermost activation
/// and level 1 the outermost, so level `n` is `current - n` steps further out.
pub(crate) fn frame_of_level(vm: &VM, level: i64) -> Option<usize> {
    let ups = levels(vm);
    let out = usize::try_from(ups.len() as i64 - level).ok()?;
    let up = *ups.get(out)?;
    vm.frames.len().checked_sub(up + 1)
}

/// The VM frame of the innermost procedure activation, or `None` at the
/// script's own level, where there is no frame whose locals a name could be.
pub(crate) fn frame_of_current_level(vm: &VM) -> Option<usize> {
    frame_of_level(vm, current_level(vm))
}

/// The formals of `name` as the running chunk declared it, or `None` when the
/// chunk defines no such procedure.
pub(crate) fn proc_params(vm: &VM, name: &str) -> Option<ProcParams> {
    let id = chunk_identity(&vm.chunk);
    PROC_TABLE
        .lock()
        .expect("proc table lock")
        .as_ref()
        .and_then(|t| t.get(&(id, name.to_string())).cloned())
}

/// The procedures the running chunk defines.
fn chunk_procs(vm: &VM) -> impl Iterator<Item = String> + '_ {
    let id = chunk_identity(&vm.chunk);
    let names: Vec<String> = PROC_TABLE
        .lock()
        .expect("proc table lock")
        .as_ref()
        .map(|t| {
            t.keys()
                .filter(|(chunk, _)| *chunk == id)
                .map(|(_, name)| name.clone())
                .collect()
        })
        .unwrap_or_default();
    names.into_iter()
}

/// The file `info script` reports — what the binary was asked to run, empty when
/// the script came from `-c` or stdin, as tclsh answers for those.
pub(crate) fn current_script() -> String {
    CURRENT_SCRIPT
        .lock()
        .expect("script lock")
        .clone()
        .unwrap_or_default()
}

/// Record the path of the file being run.
pub fn note_script(path: &str) {
    *CURRENT_SCRIPT.lock().expect("script lock") = Some(path.to_string());
}

static CURRENT_SCRIPT: Mutex<Option<String>> = Mutex::new(None);

/// Record where `chunk`'s `incr` commands put their arithmetic.
pub(crate) fn note_incr_sites(chunk: &fusevm::Chunk, ips: &[usize]) {
    if ips.is_empty() {
        return;
    }
    let id = chunk_identity(chunk);
    let mut guard = INCR_SITES.lock().expect("incr sites lock");
    let set = guard.get_or_insert_with(HashSet::new);
    for &ip in ips {
        set.insert((id, ip));
    }
}

/// Whether the arithmetic at `ip` in the chunk `id` is an `incr`'s.
fn is_incr_site(id: u64, ip: usize) -> bool {
    INCR_SITES
        .lock()
        .expect("incr sites lock")
        .as_ref()
        .is_some_and(|set| set.contains(&(id, ip)))
}

/// Record which of `chunk`'s reads tolerate an unset variable.
pub(crate) fn note_tolerant_reads(chunk: &fusevm::Chunk, ips: &[usize]) {
    if ips.is_empty() {
        return;
    }
    let id = chunk_identity(chunk);
    let mut guard = TOLERANT_READS.lock().expect("tolerant reads lock");
    let set = guard.get_or_insert_with(HashSet::new);
    for &ip in ips {
        set.insert((id, ip));
    }
}

/// Whether the read at `ip` in the chunk `id` was lowered as a tolerant one.
fn tolerates_undef(id: u64, ip: usize) -> bool {
    TOLERANT_READS
        .lock()
        .expect("tolerant reads lock")
        .as_ref()
        .is_some_and(|set| set.contains(&(id, ip)))
}

/// A value's Tcl string form, borrowed when the value already carries one.
pub(crate) fn tcl_str(v: &Value) -> Cow<'_, str> {
    match v {
        Value::Float(f) => Cow::Owned(format_double(*f)),
        Value::Bool(b) => Cow::Borrowed(if *b { "1" } else { "0" }),
        other => other.as_str_cow(),
    }
}

/// A value's Tcl string form.
pub fn to_tcl_string(v: &Value) -> String {
    tcl_str(v).into_owned()
}

/// Format a double the way Tcl does: the shortest representation that reads
/// back exactly, never looking like an integer, and in exponential form when
/// the magnitude is outside what `%g` would print positionally.
pub fn format_double(f: f64) -> String {
    if f.is_nan() {
        return "NaN".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Inf" } else { "-Inf" }.to_string();
    }
    let mag = f.abs();
    if mag != 0.0 && !(1e-4..1e17).contains(&mag) {
        let raw = format!("{f:e}"); // e.g. "1e301", "1.5e-7"
        let (mantissa, exponent) = raw.split_once('e').expect("exponential form");
        let (sign, digits) = match exponent.strip_prefix('-') {
            Some(rest) => ('-', rest),
            None => ('+', exponent),
        };
        return format!("{mantissa}e{sign}{digits}");
    }
    let plain = format!("{f}");
    if plain.contains(['.', 'e', 'n', 'i']) {
        plain
    } else {
        format!("{plain}.0")
    }
}

#[cfg(test)]
mod numeric_hook_tests {
    use super::*;

    /// 3^34, the smallest power of three past 2^53, and the double it rounds
    /// to. `L` and its own `double()` image are one apart, so every ordered
    /// comparison between them has an answer that going through an `f64`
    /// cannot give: the conversion makes them the same bits.
    const L: i64 = 16_677_181_699_666_569;

    fn truth(op: NumOp, a: Value, b: Value) -> i64 {
        match numeric(op, &a, &b) {
            Ok(Value::Int(i)) => i,
            other => panic!("{op:?} answered {other:?}"),
        }
    }

    /// Measured against tclsh 9.0.4: `expr {$L == double($L)}` is 0 and
    /// `expr {$L > double($L)}` is 1. Tcl orders an integer against a double
    /// exactly, whatever the integer's width — the rounding that arithmetic
    /// does is not a rule comparison shares.
    #[test]
    fn an_integer_past_two_to_the_fifty_third_orders_exactly_against_its_double() {
        let (int, double) = (Value::Int(L), Value::Float(L as f64));
        assert_eq!(double.clone(), Value::Float(16_677_181_699_666_568.0));

        for (op, want) in [
            (NumOp::Eq, 0),
            (NumOp::Ne, 1),
            (NumOp::Lt, 0),
            (NumOp::Gt, 1),
            (NumOp::Le, 0),
            (NumOp::Ge, 1),
        ] {
            assert_eq!(truth(op, int.clone(), double.clone()), want, "{op:?} L,D");
        }
        // The mirrored pair, which must give the mirrored answer rather than
        // whichever side happened to be tested.
        for (op, want) in [
            (NumOp::Eq, 0),
            (NumOp::Ne, 1),
            (NumOp::Lt, 1),
            (NumOp::Gt, 0),
            (NumOp::Le, 1),
            (NumOp::Ge, 0),
        ] {
            assert_eq!(truth(op, double.clone(), int.clone()), want, "{op:?} D,L");
        }
    }

    /// The first integer that a double cannot hold at all, and a negative one:
    /// the sign must not flip which way the exact comparison goes.
    #[test]
    fn the_boundary_and_its_negative_order_exactly_too() {
        let m = 9_007_199_254_740_993i64; // 2^53 + 1
        assert_eq!(truth(NumOp::Eq, Value::Int(m), Value::Float(m as f64)), 0);
        assert_eq!(truth(NumOp::Gt, Value::Int(m), Value::Float(m as f64)), 1);
        assert_eq!(truth(NumOp::Lt, Value::Int(-L), Value::Float(-L as f64)), 1);
        assert_eq!(truth(NumOp::Gt, Value::Int(-L), Value::Float(-L as f64)), 0);
        assert_eq!(truth(NumOp::Eq, Value::Int(-L), Value::Float(-L as f64)), 0);
    }

    /// An integer a double holds exactly still compares equal, and two doubles
    /// still compare as doubles — the exact path must not change either.
    #[test]
    fn exactly_representable_and_double_only_pairs_are_unchanged() {
        assert_eq!(truth(NumOp::Eq, Value::Int(3), Value::Float(3.0)), 1);
        assert_eq!(truth(NumOp::Lt, Value::Int(2), Value::Float(2.5)), 1);
        assert_eq!(truth(NumOp::Gt, Value::Int(3), Value::Float(2.5)), 1);
        assert_eq!(truth(NumOp::Eq, Value::Float(1.5), Value::Float(1.5)), 1);
        assert_eq!(truth(NumOp::Lt, Value::Float(1.5), Value::Float(2.5)), 1);
    }

    /// IEEE 754's rule for a NaN operand survives the exact path: every
    /// ordered comparison is false and `!=` is the one that is true. An
    /// infinity is beyond every integer, in whichever direction its sign says.
    #[test]
    fn nan_and_infinity_keep_their_answers() {
        let nan = Value::Float(f64::NAN);
        for op in [NumOp::Eq, NumOp::Lt, NumOp::Gt, NumOp::Le, NumOp::Ge] {
            assert_eq!(truth(op, Value::Int(L), nan.clone()), 0, "{op:?} L,nan");
            assert_eq!(truth(op, nan.clone(), Value::Int(L)), 0, "{op:?} nan,L");
        }
        assert_eq!(truth(NumOp::Ne, Value::Int(L), nan.clone()), 1);
        assert_eq!(truth(NumOp::Ne, nan, Value::Int(L)), 1);

        let inf = Value::Float(f64::INFINITY);
        assert_eq!(truth(NumOp::Lt, Value::Int(L), inf.clone()), 1);
        assert_eq!(
            truth(NumOp::Gt, Value::Int(L), Value::Float(f64::NEG_INFINITY)),
            1
        );
        assert_eq!(truth(NumOp::Gt, inf, Value::Int(L)), 1);
    }

    /// Arithmetic is the other rule and must not follow comparison onto the
    /// exact path: tclsh answers `expr {$L - double($L)}` with 0.0, not 1,
    /// because a double operand promotes the integer. Measured against tclsh
    /// 9.0.4.
    #[test]
    fn arithmetic_still_promotes_the_integer_to_a_double() {
        let (int, double) = (Value::Int(L), Value::Float(L as f64));
        assert_eq!(
            numeric(NumOp::Sub, &int, &double),
            Ok(Value::Float(0.0)),
            "subtraction promotes rather than answering the exact 1"
        );
        assert_eq!(
            numeric(NumOp::Add, &int, &double),
            Ok(Value::Float(33_354_363_399_333_136.0))
        );
        assert_eq!(
            numeric(NumOp::Mul, &int, &Value::Float(1.0)),
            Ok(Value::Float(16_677_181_699_666_568.0))
        );
    }
}
