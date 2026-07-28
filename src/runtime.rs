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

use std::sync::{Arc, Mutex};

use fusevm::{NumOp, VMResult, Value, VM};

use crate::compiler::{self, ext, ext_wide};
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

/// Compile and run a script, capturing its output.
pub fn eval(src: &str) -> Result<Outcome, String> {
    let script = crate::parser::parse(src).map_err(|e| e.to_string())?;
    let chunk = compiler::compile(&script).map_err(|e| e.to_string())?;

    let output = Arc::new(Mutex::new(String::new()));
    let error = Arc::new(Mutex::new(None::<String>));
    let catches: Arc<Mutex<Vec<CatchFrame>>> = Arc::new(Mutex::new(Vec::new()));

    let mut vm = VM::new(chunk);
    let sink = Arc::clone(&output);
    vm.set_output_sink(Box::new(move |s: &str| {
        sink.lock().expect("output lock").push_str(s);
    }));
    vm.set_numeric_hook(Arc::new(numeric));
    let err_cell = Arc::clone(&error);
    let open = Arc::clone(&catches);
    vm.set_extension_handler(Box::new(move |vm: &mut VM, id: u16, arg: u8| {
        if id == ext::CATCH_END {
            open.lock().expect("catch lock").pop();
            return;
        }
        if let Err(msg) = extension(vm, id, arg) {
            *err_cell.lock().expect("error lock") = Some(msg);
            // `VM::run` pops one value when it stops, so leave it one to pop:
            // the stack then still holds what the failing op left, and the
            // catch driver's depth arithmetic stays exact.
            vm.push(Value::Undef);
            vm.request_halt();
        }
    }));
    let entered = Arc::clone(&catches);
    vm.set_extension_wide_handler(Box::new(move |vm: &mut VM, id: u16, payload: usize| {
        if id == ext_wide::CATCH {
            entered.lock().expect("catch lock").push(CatchFrame {
                handler: payload,
                stack: vm.stack.len(),
                frames: vm.frames.len(),
            });
        }
    }));

    loop {
        let outcome = vm.run();
        let raised = error
            .lock()
            .expect("error lock")
            .take()
            .or_else(|| match &outcome {
                VMResult::Error(e) => Some(e.clone()),
                _ => None,
            });

        if let Some(msg) = raised {
            let Some(frame) = catches.lock().expect("catch lock").pop() else {
                return Err(msg);
            };
            // Unwind to the guarded script's entry state and hand the handler
            // the message.
            vm.frames.truncate(frame.frames);
            vm.stack.truncate(frame.stack);
            vm.stack.resize(frame.stack, Value::Undef);
            vm.push(Value::Str(Arc::new(msg)));
            vm.ip = frame.handler;
            vm.clear_halt();
            continue;
        }

        let output = output.lock().expect("output lock").clone();
        return match outcome {
            VMResult::Ok(v) => Ok(Outcome {
                result: to_tcl_string(&v),
                output,
            }),
            VMResult::Halted => Ok(Outcome {
                result: String::new(),
                output,
            }),
            VMResult::Error(e) => Err(e),
        };
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
