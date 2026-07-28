//! Running a compiled chunk: the numeric hook and extension ops that give
//! fusevm Tcl's arithmetic, the driver that owns error unwinding and coroutine
//! switching, and Tcl's number formatting.
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
//! The [`Machine`] below is the driver. A script that uses no coroutine is one
//! VM run in a loop that only ever restarts it at a `catch` handler; a script
//! that creates coroutines has one VM per context, and the same loop also
//! services the requests their ops raise. Both paths share one mechanism: an
//! op stashes something in a cell and halts, and the driver reads the cell
//! after `run()` returns — the pattern fusevm's scheduler is built on.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use fusevm::{Chunk, Frame, NumOp, VMResult, Value, VM};

use crate::compiler::{self, ext, ext_wide};
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

/// The cells every VM's hooks write into. One set is shared by the main VM and
/// every coroutine's; the driver swaps the per-context ones (`catches`,
/// `current`) around each `run()`, so a hook never has to know which VM it is
/// running inside.
struct Cells {
    output: Arc<Mutex<String>>,
    error: Arc<Mutex<Option<String>>>,
    /// `catch` regions the *running* VM has entered and not yet left.
    catches: Arc<Mutex<Vec<CatchFrame>>>,
    /// The coroutine request an op raised, for the driver to service.
    pending: Arc<Mutex<Option<Request>>>,
    /// The name of the coroutine whose VM is running, for `info coroutine`.
    current: Arc<Mutex<Option<String>>>,
}

impl Cells {
    fn new() -> Cells {
        Cells {
            output: Arc::new(Mutex::new(String::new())),
            error: Arc::new(Mutex::new(None)),
            catches: Arc::new(Mutex::new(Vec::new())),
            pending: Arc::new(Mutex::new(None)),
            current: Arc::new(Mutex::new(None)),
        }
    }

    /// Give `vm` the frontend's hooks. Every VM gets the same ones: a coroutine
    /// prints to the same stdout and raises errors through the same cell as the
    /// script that created it.
    fn install(&self, vm: &mut VM) {
        let sink = Arc::clone(&self.output);
        vm.set_output_sink(Box::new(move |s: &str| {
            sink.lock().expect("output lock").push_str(s);
        }));
        vm.set_numeric_hook(Arc::new(numeric));

        let err_cell = Arc::clone(&self.error);
        let open = Arc::clone(&self.catches);
        let pending = Arc::clone(&self.pending);
        let current = Arc::clone(&self.current);
        vm.set_extension_handler(Box::new(move |vm: &mut VM, id: u16, arg: u8| {
            if id == ext::CATCH_END {
                open.lock().expect("catch lock").pop();
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
            if let Err(msg) = extension(vm, id, arg) {
                *err_cell.lock().expect("error lock") = Some(msg);
                // `VM::run` pops one value when it stops, so leave it one to
                // pop: the stack then still holds what the failing op left, and
                // the catch driver's depth arithmetic stays exact.
                vm.push(Value::Undef);
                vm.request_halt();
            }
        }));

        let entered = Arc::clone(&self.catches);
        vm.set_extension_wide_handler(Box::new(move |vm: &mut VM, id: u16, payload: usize| {
            if id == ext_wide::CATCH {
                entered.lock().expect("catch lock").push(CatchFrame {
                    handler: payload,
                    stack: vm.stack.len(),
                    frames: vm.frames.len(),
                });
            }
        }));
    }
}

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

/// The driver: every execution context of one script, and the loop that runs
/// them.
struct Machine {
    cells: Cells,
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

/// Compile and run a script, capturing its output.
pub fn eval(src: &str) -> Result<Outcome, String> {
    let script = crate::parser::parse(src).map_err(|e| e.to_string())?;
    let chunk = compiler::compile(&script).map_err(|e| e.to_string())?;

    let cells = Cells::new();
    let mut main = VM::new(chunk.clone());
    cells.install(&mut main);
    let globals = std::mem::take(&mut main.globals);

    let mut machine = Machine {
        cells,
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
    let output = machine.cells.output.lock().expect("output lock").clone();

    match outcome {
        Ok(VMResult::Ok(v)) => Ok(Outcome {
            result: to_tcl_string(&v),
            output,
        }),
        Ok(_) => Ok(Outcome {
            result: String::new(),
            output,
        }),
        Err(msg) => Err(msg),
    }
}

impl Machine {
    /// Run contexts until the main script finishes or an error escapes it.
    fn drive(&mut self) -> Result<VMResult, String> {
        loop {
            let outcome = self.run_current();

            let raised = self
                .cells
                .error
                .lock()
                .expect("error lock")
                .take()
                .or_else(|| match &outcome {
                    VMResult::Error(e) => Some(e.clone()),
                    _ => None,
                });
            if let Some(msg) = raised {
                self.raise(msg)?;
                continue;
            }

            let request = self.cells.pending.lock().expect("request lock").take();
            if let Some(request) = request {
                // The op halted mid-expression, so `run` popped a live value
                // from underneath it. Put it back before anything else touches
                // this stack.
                if let VMResult::Ok(v) = outcome {
                    self.vm(self.current).stack.push(v);
                }
                if let Err(msg) = self.service(request) {
                    self.raise(msg)?;
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
        *self.cells.current.lock().expect("coroutine lock") = self.contexts[current].name.clone();
        *self.cells.catches.lock().expect("catch lock") =
            std::mem::take(&mut self.contexts[current].catches);
        let globals = std::mem::take(&mut self.globals);

        let vm = self.vm(current);
        vm.globals = globals;
        vm.clear_halt();
        let outcome = vm.run();
        let globals = std::mem::take(&mut vm.globals);

        self.globals = globals;
        self.contexts[current].catches =
            std::mem::take(&mut self.cells.catches.lock().expect("catch lock"));
        outcome
    }

    fn vm(&mut self, context: usize) -> &mut VM {
        self.contexts[context].vm.as_mut().expect("live context")
    }

    /// Report a Tcl error in the running context: resume at its innermost open
    /// `catch` handler, or — for a coroutine with none — end the coroutine and
    /// report the error to whoever resumed it, as the reference implementation
    /// does. `Err` means nothing was left to catch it.
    fn raise(&mut self, msg: String) -> Result<(), String> {
        loop {
            if let Some(frame) = self.contexts[self.current].catches.pop() {
                let vm = self.vm(self.current);
                // Unwind to the guarded script's entry state and hand the
                // handler the message.
                vm.frames.truncate(frame.frames);
                vm.stack.truncate(frame.stack);
                vm.stack.resize(frame.stack, Value::Undef);
                vm.push(Value::Str(Arc::new(msg)));
                vm.ip = frame.handler;
                return Ok(());
            }
            if self.current == 0 {
                return Err(msg);
            }
            match self.discard(self.current) {
                Some(resumer) => self.current = resumer,
                None => return Err(msg),
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
    fn service(&mut self, request: Request) -> Result<(), String> {
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
        self.cells.install(&mut vm);
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
