//! Running a compiled chunk: the numeric hook and extension ops that give
//! fusevm Tcl's arithmetic, the driver that owns interpreter state, error
//! unwinding and coroutine switching, and Tcl's number formatting.
//!
//! Two hooks carry all of the language-specific behavior:
//!
//! * the **numeric hook** catches operands the VM cannot compute on natively —
//!   strings, mostly — and applies Tcl's rules: an operand that parses as a
//!   number is one, comparisons fall back to string order when it does not, and
//!   arithmetic on a non-number is an error;
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
//! * a [`Machine`] drives one evaluation. A script that uses no coroutine is
//!   one VM run in a loop that only ever restarts it at a `catch` handler; a
//!   script that creates coroutines has one VM per context, and the same loop
//!   also services the requests their ops raise. Both paths share one
//!   mechanism: an op stashes something in a cell and halts, and the driver
//!   reads the cell after `run()` returns — the pattern fusevm's scheduler is
//!   built on;
//! * [`Hooks::install`] is the only place a hook is put on a VM, so the main
//!   VM, every coroutine's VM and every nested `eval`'s VM behave alike.
//!
//! The two ways of holding variables meet at [`seed`] and [`flush`]. Within one
//! evaluation every VM runs the same chunk, so the global table is a `Vec` the
//! driver moves into whichever VM is about to run. Across evaluations the chunk
//! differs — a chunk interns its own name table — so the interpreter keeps the
//! variables keyed by name and the vector is projected out of that map on entry
//! and read back into it on exit.
//!
//! The VM is asked for its highest tier: [`Hooks::install`] also calls
//! `enable_tracing_jit`, which makes `VM::run` consult fusevm's block JIT for a
//! wholly-eligible chunk and arm the trace recorder at every backward branch
//! otherwise. [`crate::tiers`] reports which of those tiers a given script
//! actually reaches — see the JIT section of the README for what that measures
//! on Tcl today.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::Write;
use std::sync::{Arc, Mutex};

use fusevm::{Chunk, Frame, NumOp, VMResult, Value, VM};

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

/// A script that would not compile, or that failed while running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TclError {
    /// The message, in the reference interpreter's wording.
    pub msg: String,
    /// The 1-based script line, when the failure was located while compiling.
    /// A failure raised by a running chunk carries no line, as the reference
    /// interpreter's does not either.
    pub line: Option<usize>,
}

impl TclError {
    pub(crate) fn plain(msg: impl Into<String>) -> Self {
        TclError {
            msg: msg.into(),
            line: None,
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
enum Output {
    Capture(Arc<Mutex<String>>),
    Stdout(Arc<Mutex<std::io::BufWriter<std::io::Stdout>>>),
}

impl Output {
    fn stdout() -> Output {
        Output::Stdout(Arc::new(Mutex::new(std::io::BufWriter::new(
            std::io::stdout(),
        ))))
    }

    fn write(&self, s: &str) {
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
    fn flush(&self) {
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
struct State {
    /// The variables, keyed by name. This is the authority, not the VM's slot
    /// vector — see [`seed`].
    globals: HashMap<String, Value>,
    cache: ChunkCache,
    /// Where the scripts of this interpreter write.
    output: Output,
    /// How many scripts are running, counting the outermost.
    depth: usize,
    limit: usize,
}

type Shared = Arc<Mutex<State>>;

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
                cache: ChunkCache::new(),
                output,
                depth: 0,
                limit: DEFAULT_RECURSION_LIMIT,
            })),
        }
    }

    /// How deep `eval` may nest. Lower it when the interpreter runs on a stack
    /// smaller than [`RECOMMENDED_STACK`], because the depth is the only thing
    /// standing between a runaway script and a stack overflow.
    pub fn set_recursion_limit(&mut self, limit: usize) {
        self.lock().limit = limit.max(1);
    }

    /// Compile and run a script, returning the value of its last command.
    pub fn eval(&mut self, src: &str) -> Result<String, TclError> {
        run_source(&self.shared, src).map(|v| to_tcl_string(&v))
    }

    /// Run a chunk this interpreter did not compile, against its variables.
    ///
    /// [`Interp::eval`] is the ordinary way in, and it compiles through the
    /// cache. This is for a caller holding a chunk that was lowered
    /// differently — the debug adapter runs
    /// [`crate::compiler::compile_debug`]'s output, which is the same script
    /// with a line marker before every command.
    pub fn run_chunk(&mut self, chunk: fusevm::Chunk) -> Result<String, TclError> {
        Machine::run(&self.shared, chunk).map(|v| to_tcl_string(&v))
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
fn run_source(shared: &Shared, src: &str) -> Result<Value, TclError> {
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
        state.cache.compile(src)
    };
    // The depth is given back however this returns, including the compile
    // failure above, which is why it is not a `?` in the block.
    let result = compiled.and_then(|chunk| {
        // `VM::new` takes the chunk by value, so the cached one is cloned
        // rather than moved out; the parse and the lowering are what the cache
        // saves.
        Machine::run(shared, (*chunk).clone())
    });
    shared.lock().expect("interpreter lock").depth -= 1;
    result
}

/// A chunk interns its own name table, so the slot holding a given variable
/// differs from chunk to chunk and a slot vector cannot be carried from one run
/// to the next. The interpreter's map is the authority; a chunk's slots are a
/// projection of it, built here on entry and read back by [`flush`] on exit.
fn seed(chunk: &Chunk, shared: &Shared) -> Vec<Value> {
    let state = shared.lock().expect("interpreter lock");
    chunk
        .names
        .iter()
        .map(|name| state.globals.get(name).cloned().unwrap_or(Value::Undef))
        .collect()
}

/// Write a finished chunk's slots back into the interpreter's variables. A slot
/// left `Undef` — never assigned, or unset — removes the variable rather than
/// storing an empty value, so `unset` survives into the next evaluation.
fn flush(chunk: &Chunk, shared: &Shared, globals: &[Value]) {
    let mut state = shared.lock().expect("interpreter lock");
    for (slot, name) in chunk.names.iter().enumerate() {
        // The compiler's own loop state is named with a leading NUL so that no
        // Tcl variable can collide with it. It is rebuilt on every entry to the
        // loop that owns it, so it is not interpreter state.
        if name.starts_with('\u{0}') {
            continue;
        }
        match globals.get(slot) {
            Some(Value::Undef) | None => {
                state.globals.remove(name);
            }
            Some(value) => {
                state.globals.insert(name.clone(), value.clone());
            }
        }
    }
}

// ── the hooks ────────────────────────────────────────────────────────────

/// A `catch` region the VM has entered and not yet left.
///
/// The two depths are what makes resuming possible: an error can be raised
/// anywhere below, including inside a procedure the guarded script called, and
/// restoring them puts the VM back exactly where the handler was compiled to
/// expect it.
struct CatchFrame {
    /// Op index of the handler block the compiler emitted for this region.
    handler: usize,
    /// Value-stack length when the region was entered.
    stack: usize,
    /// Call-frame count when the region was entered.
    frames: usize,
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
        vm.set_numeric_hook(Arc::new(numeric));

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
            if id == ext::CATCH_END {
                open.lock().expect("catch lock").pop();
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
                ext::EVAL => eval_op(&interp, vm, arg),
                ext::FFI_CALL => ffi_op(vm, arg).map_err(TclError::plain),
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
        vm.set_extension_wide_handler(Box::new(move |vm: &mut VM, id: u16, payload: usize| {
            if id == ext_wide::DBG_LINE {
                // Only a chunk compiled by `compile_debug` carries these, and
                // only `--dap` answers them; without a session attached this is
                // one `Option` check.
                crate::dap::at_line(vm, payload);
                return;
            }
            if id == ext_wide::CATCH {
                entered.lock().expect("catch lock").push(CatchFrame {
                    handler: payload,
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
fn jit_enabled() -> bool {
    !matches!(
        std::env::var("TCLRS_JIT").as_deref(),
        Ok("off") | Ok("0") | Ok("no")
    )
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
    let globals = seed(&vm.chunk, interp);
    vm.globals = globals;
    vm.push(result?);
    Ok(())
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
    /// The compiled program, from which each coroutine's VM is built.
    chunk: Chunk,
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
    fn run(shared: &Shared, chunk: Chunk) -> Result<Value, TclError> {
        let hooks = Hooks::new(Arc::clone(shared));
        let mut main = VM::new(chunk.clone());
        hooks.install(&mut main);
        let globals = seed(&chunk, shared);

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
        // The variables a failing script did set are still set, as they are in
        // the reference interpreter, so the write-back happens either way.
        flush(&machine.chunk, shared, &machine.globals);
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
        *self.hooks.current.lock().expect("coroutine lock") = self.contexts[current].name.clone();
        *self.hooks.catches.lock().expect("catch lock") =
            std::mem::take(&mut self.contexts[current].catches);
        let globals = std::mem::take(&mut self.globals);

        let vm = self.vm(current);
        vm.globals = globals;
        vm.clear_halt();
        let outcome = vm.run();
        let globals = std::mem::take(&mut vm.globals);

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
    fn raise(&mut self, e: TclError) -> Result<(), TclError> {
        loop {
            if let Some(frame) = self.contexts[self.current].catches.pop() {
                let vm = self.vm(self.current);
                // Unwind to the guarded script's entry state and hand the
                // handler the message.
                vm.frames.truncate(frame.frames);
                vm.stack.truncate(frame.stack);
                vm.stack.resize(frame.stack, Value::Undef);
                vm.push(Value::Str(Arc::new(e.msg)));
                vm.ip = frame.handler;
                return Ok(());
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

        let mut vm = VM::new(self.chunk.clone());
        self.hooks.install(&mut vm);
        let base = vm.stack.len();
        for a in args {
            vm.stack.push(a);
        }
        vm.frames.push(Frame {
            return_ip: self.chunk.ops.len(),
            stack_base: base,
            slots: Vec::new(),
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

    /// `yield` and `yieldto` are errors outside a coroutine.
    fn in_coroutine(&self, command: &str) -> Result<(), String> {
        if self.contexts[self.current].name.is_none() {
            return Err(format!("{command} can only be called in a coroutine"));
        }
        Ok(())
    }
}

/// A Tcl number: integral until something forces a double.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Num {
    Int(i64),
    Float(f64),
}

impl Num {
    fn as_f64(self) -> f64 {
        match self {
            Num::Int(i) => i as f64,
            Num::Float(f) => f,
        }
    }
}

/// Why a string is not a number this frontend can use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotNumeric {
    /// No numeric spelling at all.
    Unparsable,
    /// An integer spelling whose value does not fit an `i64`. Tcl promotes it to
    /// a bignum; this frontend has none, so the operand is refused rather than
    /// silently becoming the nearest double — `expr {99999999999999999999 + 1}`
    /// is the overflow error, not `1e+20`.
    TooLarge,
}

/// Interpret a value as a Tcl number. Leading and trailing whitespace is
/// allowed, as are the radix prefixes `0x`, `0o` and `0b`.
fn tcl_num(v: &Value) -> Result<Num, NotNumeric> {
    match v {
        Value::Int(i) => Ok(Num::Int(*i)),
        Value::Float(f) => Ok(Num::Float(*f)),
        Value::Bool(b) => Ok(Num::Int(*b as i64)),
        _ => parse_number(v.as_str_cow().trim()),
    }
}

/// A number, reading an integer too large for an `i64` as the nearest double.
///
/// Only comparison uses this. An operator that computes has to refuse the
/// operand, since answering with the nearest double would be answering with a
/// value the script never wrote; ordering has no exact answer without a bignum
/// either way, and the nearest double orders far better than the string does —
/// `99999999999999999999 < 200000000000000000000` is true as doubles and false
/// as text.
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
            // Digits of the right shape that do not fit are the bignum case; a
            // `0x` with no valid digit at all is simply not a number.
            Err(_) if !digits.is_empty() && digits.chars().all(|c| c.is_digit(radix)) => {
                Err(NotNumeric::TooLarge)
            }
            Err(_) => Err(NotNumeric::Unparsable),
        };
    }
    if let Ok(i) = body.parse::<i64>() {
        return Ok(Num::Int(sign * i));
    }
    // An integer spelling that does not fit is the bignum case, and must not
    // fall through to the double parser — which would take it, exactly, and
    // answer with a value the script never wrote.
    if !body.is_empty() && body.bytes().all(|b| b.is_ascii_digit()) {
        return Err(NotNumeric::TooLarge);
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
        // A boolean needs no bignum: an integer spelling that does not fit an
        // `i64` has a magnitude larger than `i64::MAX`, so it is nonzero, and
        // that is the whole question here. Refusing it would make
        // `if {99999999999999999999}` an error where tclsh takes the branch.
        Err(NotNumeric::TooLarge) => Ok(true),
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
fn boolean_word(text: &str) -> Option<bool> {
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
        Err(NotNumeric::TooLarge) => Err(too_large()),
        _ => Err(format!("expected integer but got {}", named(&text, 50))),
    }
}

/// The numeric hook: called when an operand is not something the VM can
/// compute on natively, or when an integer operation overflows.
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
            (Some(p), Some(q)) => p
                .as_f64()
                .partial_cmp(&q.as_f64())
                .unwrap_or(std::cmp::Ordering::Greater),
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
    let x = tcl_num(a).map_err(|why| operand_error(why, a, sym))?;
    let y = if matches!(op, NumOp::Neg) {
        Num::Int(0)
    } else {
        tcl_num(b).map_err(|why| operand_error(why, b, sym))?
    };

    let value = match (op, x, y) {
        (NumOp::Neg, Num::Int(i), _) => i.checked_neg().map(Value::Int).ok_or_else(too_large)?,
        (NumOp::Neg, Num::Float(f), _) => Value::Float(-f),
        (_, Num::Int(i), Num::Int(j)) => {
            let folded = match op {
                NumOp::Add => i.checked_add(j),
                NumOp::Sub => i.checked_sub(j),
                NumOp::Mul => i.checked_mul(j),
                _ => return Err(format!("unsupported integer operation {sym}")),
            };
            Value::Int(folded.ok_or_else(too_large)?)
        }
        (_, p, q) => {
            let (p, q) = (p.as_f64(), q.as_f64());
            Value::Float(match op {
                NumOp::Add => p + q,
                NumOp::Sub => p - q,
                NumOp::Mul => p * q,
                _ => return Err(format!("unsupported operation {sym}")),
            })
        }
    };
    Ok(value)
}

fn non_numeric(v: &Value, op: &str) -> String {
    format!(
        "can't use non-numeric string as operand of \"{op}\": \"{}\"",
        v.as_str_cow()
    )
}

/// What an operator says about an operand it cannot use: the overflow this
/// frontend documents in place of a bignum, or the non-numeric refusal.
fn operand_error(why: NotNumeric, v: &Value, op: &str) -> String {
    match why {
        NotNumeric::TooLarge => too_large(),
        NotNumeric::Unparsable => non_numeric(v, op),
    }
}

/// Tcl promotes an overflowing integer to arbitrary precision. This frontend
/// has no bignum yet, so the operation fails rather than wrapping silently.
fn too_large() -> String {
    "integer value too large to represent".to_string()
}

/// The frontend's extension ops.
fn extension(vm: &mut VM, id: u16, arg: u8) -> Result<(), String> {
    match id {
        ext::DIV | ext::MOD | ext::POW => {
            let b = vm.pop();
            let a = vm.pop();
            let x = tcl_num(&a).map_err(|why| operand_error(why, &a, sym_of(id)))?;
            let y = tcl_num(&b).map_err(|why| operand_error(why, &b, sym_of(id)))?;
            vm.push(arith(id, x, y)?);
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
                    Ok(Num::Float(f)) => !float_bool(f)?,
                    Err(NotNumeric::TooLarge) => return Err(too_large()),
                    Err(NotNumeric::Unparsable) => {
                        !boolean_word(&v.as_str_cow()).ok_or_else(|| non_numeric(&v, "!"))?
                    }
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
            let elements = crate::list::split(&haystack.as_str_cow())?;
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
        ext::ERROR => Err(to_tcl_string(&vm.pop())),
        // The ranges are tested from the highest base down, so that a lower
        // one's `id >= BASE` does not swallow a higher module's ops.
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
        _ => "**",
    }
}

/// Integer division and remainder floor toward negative infinity — `-57 / 10`
/// is -6 and `-57 % 10` is 3 — and `**` keeps integral operands integral.
fn arith(id: u16, x: Num, y: Num) -> Result<Value, String> {
    match (id, x, y) {
        (ext::DIV, Num::Int(_), Num::Int(0)) | (ext::MOD, Num::Int(_), Num::Int(0)) => {
            Err("divide by zero".to_string())
        }
        // `i64::MIN / -1` is the one integer division whose true quotient does
        // not fit. Tcl answers with a bignum; this frontend has none, so it
        // reports the overflow rather than trapping on it.
        (ext::DIV, Num::Int(i64::MIN), Num::Int(-1)) => Err(too_large()),
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
        (ext::POW, Num::Int(i), Num::Int(j)) if j >= 0 => {
            let exp = u32::try_from(j).map_err(|_| too_large())?;
            i.checked_pow(exp).map(Value::Int).ok_or_else(too_large)
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
        (ext::DIV, p, q) => Ok(Value::Float(p.as_f64() / q.as_f64())),
        (ext::MOD, _, _) => Err("can't use floating-point value as operand of \"%\"".to_string()),
        // A double operand anywhere makes the result a double, and a zero base
        // raised to a negative power still has no value.
        (_, p, q) => {
            if p.as_f64() == 0.0 && q.as_f64() < 0.0 {
                return Err("exponentiation of zero by negative power".to_string());
            }
            Ok(Value::Float(p.as_f64().powf(q.as_f64())))
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
    }
}

/// Take a variable's value, leaving its place empty.
pub(crate) fn take_var(vm: &mut VM, place: Place) -> Value {
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
        Value::Int(index) => Ok(if slot_form {
            Place::Slot(*index as u16)
        } else {
            Place::Global(*index as u16)
        }),
        other => Err(format!("not a variable place: {other:?}")),
    }
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
