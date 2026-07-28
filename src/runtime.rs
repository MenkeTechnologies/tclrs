//! Running a compiled chunk: the numeric hook and extension ops that give
//! fusevm Tcl's arithmetic, plus Tcl's number formatting.
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
//! A script is not always run alone. An [`Interp`] owns the variables and holds
//! them between evaluations, which is what makes a REPL a REPL and what lets
//! the `eval` command run a script built at run time and see the same state the
//! script that built it sees.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use fusevm::{Chunk, NumOp, VMResult, Value, VM};

use crate::cache::ChunkCache;
use crate::compiler::ext;

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

/// Compile and run a script in a fresh interpreter, capturing its output.
///
/// A one-shot convenience over [`Interp`]: the state it builds is discarded
/// when it returns.
pub fn eval(src: &str) -> Result<Outcome, String> {
    let mut interp = Interp::capturing();
    let result = interp.eval(src).map_err(|e| e.to_string())?;
    Ok(Outcome {
        result,
        output: interp.take_output(),
    })
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
    /// `Some` when output is captured rather than written to stdout.
    output: Option<String>,
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
        Interp::with_output(None)
    }

    /// An interpreter that collects what its scripts write, for
    /// [`Interp::take_output`].
    pub fn capturing() -> Self {
        Interp::with_output(Some(String::new()))
    }

    fn with_output(output: Option<String>) -> Self {
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

    /// Take everything captured so far, leaving the buffer empty. Always empty
    /// for an interpreter built by [`Interp::new`], which does not capture.
    pub fn take_output(&mut self) -> String {
        match self.lock().output.as_mut() {
            Some(buf) => std::mem::take(buf),
            None => String::new(),
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
    let chunk = {
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
    let result = chunk.and_then(|chunk| {
        // `VM::new` takes the chunk by value, so the cached one is cloned
        // rather than moved out; the parse and the lowering are what the cache
        // saves.
        run_chunk(shared, (*chunk).clone())
    });
    shared.lock().expect("interpreter lock").depth -= 1;
    result
}

fn run_chunk(shared: &Shared, chunk: Chunk) -> Result<Value, TclError> {
    let capturing = shared.lock().expect("interpreter lock").output.is_some();

    let mut vm = VM::new(chunk);
    if capturing {
        let sink = Arc::clone(shared);
        vm.set_output_sink(Box::new(move |s: &str| {
            if let Some(buf) = sink.lock().expect("interpreter lock").output.as_mut() {
                buf.push_str(s);
            }
        }));
    }
    vm.set_numeric_hook(Arc::new(numeric));

    let error = Arc::new(Mutex::new(None::<TclError>));
    let err_cell = Arc::clone(&error);
    let handler_state = Arc::clone(shared);
    vm.set_extension_handler(Box::new(move |vm: &mut VM, id: u16, arg: u8| {
        let outcome = match id {
            ext::EVAL => eval_op(&handler_state, vm, arg),
            _ => extension(vm, id, arg).map_err(TclError::plain),
        };
        if let Err(e) = outcome {
            *err_cell.lock().expect("error lock") = Some(e);
            vm.request_halt();
        }
    }));

    seed(&mut vm, shared);
    let outcome = vm.run();
    // The variables a failing script did set are still set, as they are in the
    // reference interpreter, so the write-back happens either way.
    flush(&mut vm, shared);

    if let Some(e) = error.lock().expect("error lock").take() {
        return Err(e);
    }
    match outcome {
        VMResult::Ok(v) => Ok(v),
        VMResult::Halted => Ok(Value::Str(Arc::new(String::new()))),
        VMResult::Error(e) => Err(TclError::plain(e)),
    }
}

/// A chunk interns its own name table, so the slot holding a given variable
/// differs from chunk to chunk and a slot vector cannot be carried from one run
/// to the next. The interpreter's map is the authority; a chunk's slots are a
/// projection of it, built here on entry and read back by [`flush`] on exit.
fn seed(vm: &mut VM, shared: &Shared) {
    let state = shared.lock().expect("interpreter lock");
    vm.globals.clear();
    vm.globals.reserve(vm.chunk.names.len());
    for name in &vm.chunk.names {
        let value = state.globals.get(name).cloned().unwrap_or(Value::Undef);
        vm.globals.push(value);
    }
}

/// Write a finished chunk's slots back into the interpreter's variables. A slot
/// left `Undef` — never assigned, or unset — removes the variable rather than
/// storing an empty value, so `unset` survives into the next evaluation.
fn flush(vm: &mut VM, shared: &Shared) {
    let mut state = shared.lock().expect("interpreter lock");
    for (slot, name) in vm.chunk.names.iter().enumerate() {
        // The compiler's own loop state is named with a leading NUL so that no
        // Tcl variable can collide with it. It is rebuilt on every entry to the
        // loop that owns it, so it is not interpreter state.
        if name.starts_with('\u{0}') {
            continue;
        }
        match vm.globals.get(slot) {
            Some(Value::Undef) | None => {
                state.globals.remove(name);
            }
            Some(value) => {
                state.globals.insert(name.clone(), value.clone());
            }
        }
    }
}

/// The `eval` command: concatenate the arguments and run the result as a
/// script, against the state of the interpreter that reached this op.
///
/// The outer chunk's slots are written back before the nested script runs and
/// re-read after it, so the two see one set of variables in both directions —
/// including when the nested script fails, since what it did set before failing
/// is set.
fn eval_op(shared: &Shared, vm: &mut VM, argc: u8) -> Result<(), TclError> {
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

    flush(vm, shared);
    let result = run_source(shared, &src);
    seed(vm, shared);
    vm.push(result?);
    Ok(())
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

/// Interpret a value as a Tcl number, or `None` when it has no numeric
/// interpretation. Leading and trailing whitespace is allowed, as are the
/// radix prefixes `0x`, `0o` and `0b`.
fn tcl_num(v: &Value) -> Option<Num> {
    match v {
        Value::Int(i) => Some(Num::Int(*i)),
        Value::Float(f) => Some(Num::Float(*f)),
        Value::Bool(b) => Some(Num::Int(*b as i64)),
        _ => parse_num(v.as_str_cow().trim()),
    }
}

pub(crate) fn parse_num(text: &str) -> Option<Num> {
    if text.is_empty() {
        return None;
    }
    let (sign, body) = match text.as_bytes()[0] {
        b'-' => (-1i64, &text[1..]),
        b'+' => (1, &text[1..]),
        _ => (1, text),
    };
    let radix = if body.len() > 2 {
        match &body[..2] {
            "0x" | "0X" => Some(16),
            "0o" | "0O" => Some(8),
            "0b" | "0B" => Some(2),
            _ => None,
        }
    } else {
        None
    };
    if let Some(radix) = radix {
        return i64::from_str_radix(&body[2..], radix)
            .ok()
            .map(|v| Num::Int(sign * v));
    }
    if let Ok(i) = body.parse::<i64>() {
        return Some(Num::Int(sign * i));
    }
    // Tcl accepts Inf and NaN spellings that Rust's parser also takes; it does
    // not accept a bare `.` or an empty mantissa, and neither does Rust's.
    body.parse::<f64>()
        .ok()
        .map(|f| Num::Float(sign as f64 * f))
}

/// The numeric hook: called when an operand is not something the VM can
/// compute on natively, or when an integer operation overflows.
fn numeric(op: NumOp, a: &Value, b: &Value) -> Result<Value, String> {
    let (x, y) = (tcl_num(a), tcl_num(b));

    // Comparisons prefer numbers but fall back to string order, which is what
    // makes `expr {"10" < "9"}` false and `expr {10 < 9}` also false while
    // `expr {"abc" < "abd"}` is true.
    let cmp = matches!(
        op,
        NumOp::Lt | NumOp::Gt | NumOp::Le | NumOp::Ge | NumOp::Eq | NumOp::Ne
    );
    if cmp {
        let ordering = match (x, y) {
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
    let x = x.ok_or_else(|| non_numeric(a, sym))?;
    let y = if matches!(op, NumOp::Neg) {
        Num::Int(0)
    } else {
        y.ok_or_else(|| non_numeric(b, sym))?
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
            let x = tcl_num(&a).ok_or_else(|| non_numeric(&a, sym_of(id)))?;
            let y = tcl_num(&b).ok_or_else(|| non_numeric(&b, sym_of(id)))?;
            vm.push(arith(id, x, y)?);
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
        ext::NORM => {
            let v = vm.pop();
            let normalized = if arg == 1 {
                // A logical operator's result is 1 or 0, never the operand.
                Value::Int(v.is_truthy() as i64)
            } else {
                match v {
                    Value::Bool(b) => Value::Int(b as i64),
                    Value::Float(f) => Value::Str(Arc::new(format_double(f))),
                    other => other,
                }
            };
            vm.push(normalized);
            Ok(())
        }
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
        (ext::DIV, Num::Int(i), Num::Int(j)) => Ok(Value::Int(
            i.div_euclid(j)
                - i64::from(
                    // div_euclid rounds toward negative infinity only for a positive
                    // divisor; for a negative one it rounds the other way.
                    j < 0 && i.rem_euclid(j) != 0,
                ),
        )),
        (ext::MOD, Num::Int(i), Num::Int(j)) => {
            let r = i % j;
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
        (ext::DIV, p, q) => Ok(Value::Float(p.as_f64() / q.as_f64())),
        (ext::MOD, _, _) => Err("can't use floating-point value as operand of \"%\"".to_string()),
        (_, p, q) => Ok(Value::Float(p.as_f64().powf(q.as_f64()))),
    }
}

/// A value's Tcl string form.
pub fn to_tcl_string(v: &Value) -> String {
    match v {
        Value::Float(f) => format_double(*f),
        Value::Bool(b) => (*b as i64).to_string(),
        other => other.as_str_cow().into_owned(),
    }
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
