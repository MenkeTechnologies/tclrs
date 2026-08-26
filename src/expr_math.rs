//! `expr`'s math functions — the `mathfunc(n)` set.
//!
//! Every function tclsh 9.0.4 registers under `::tcl::mathfunc::` is here, and
//! each one is a port of the C implementation in `generic/tclBasic.c` rather
//! than a re-derivation from the reference page. That distinction is load
//! bearing in several places where the page and the interpreter disagree:
//!
//! * `int()` is documented as truncating toward the machine word, and in
//!   9.0.4 it does not truncate at all — `int(1e300)` is a 301-digit integer,
//!   exactly what `entier()` answers. `wide()` is the one that still wraps.
//! * `floor()` and `ceil()` do **not** go through the argument's `double`
//!   value when the argument is an integer. `Tcl_GetBignumFromObj` succeeds
//!   for one, and `TclFloor`/`TclCeil` (`generic/tclStrToD.c`) then round in a
//!   *direction* rather than to nearest: `floor(9223372036854775807)` is
//!   9.223372036854775e+18 where `double()` of the same value is
//!   9.223372036854776e+18.
//! * `log(0)` is `-Inf`, not a domain error: `CheckDoubleResult` accepts an
//!   infinity that came with `ERANGE` and only refuses a NaN.
//! * `srand()` handed a non-integer fails with an **empty** message, because
//!   `ExprSrandFunc` calls `TclGetWideBitsFromObj` with a null interpreter and
//!   then returns the error without writing one.
//!
//! Each call lowers to one extension op whose id is the function's index in
//! `FUNCTIONS` and whose inline operand is the actual argument count. Arity
//! is therefore checked when the call *runs*, which is where tclsh checks it:
//! `if {0} {expr {abs(1,2)}}` is silent there, and refusing while compiling
//! would make it an error.

use fusevm::{Op, Value};
use num_bigint::BigInt;
use num_traits::cast::{FromPrimitive, ToPrimitive};

use crate::compiler::{CompileError, Compiler};
use crate::expr::Expr;
use crate::runtime::{big_cmp, from_big, named, tcl_bool, tcl_num, Num};

/// How many arguments a function takes, in the form `ExprXxxFunc` checks it:
/// the count `objc` is compared against, with `objv[0]` included.
enum Arity {
    /// Exactly `n` values.
    Exactly(usize),
    /// At least one, as `max` and `min` take.
    AtLeastOne,
}

/// A function's name and arity. The index into this table **is** the extension
/// op id (offset by [`crate::compiler::ext::MATH_BASE`]), so entries may be
/// appended but never reordered — a chunk cached on disk carries the id, not
/// the name.
const FUNCTIONS: &[(&str, Arity)] = &[
    ("abs", Arity::Exactly(1)),
    ("acos", Arity::Exactly(1)),
    ("asin", Arity::Exactly(1)),
    ("atan", Arity::Exactly(1)),
    ("atan2", Arity::Exactly(2)),
    ("bool", Arity::Exactly(1)),
    ("ceil", Arity::Exactly(1)),
    ("cos", Arity::Exactly(1)),
    ("cosh", Arity::Exactly(1)),
    ("double", Arity::Exactly(1)),
    ("entier", Arity::Exactly(1)),
    ("exp", Arity::Exactly(1)),
    ("floor", Arity::Exactly(1)),
    ("fmod", Arity::Exactly(2)),
    ("hypot", Arity::Exactly(2)),
    ("int", Arity::Exactly(1)),
    ("isfinite", Arity::Exactly(1)),
    ("isinf", Arity::Exactly(1)),
    ("isnan", Arity::Exactly(1)),
    ("isnormal", Arity::Exactly(1)),
    ("isqrt", Arity::Exactly(1)),
    ("issubnormal", Arity::Exactly(1)),
    ("isunordered", Arity::Exactly(2)),
    ("log", Arity::Exactly(1)),
    ("log10", Arity::Exactly(1)),
    ("max", Arity::AtLeastOne),
    ("min", Arity::AtLeastOne),
    ("pow", Arity::Exactly(2)),
    ("rand", Arity::Exactly(0)),
    ("round", Arity::Exactly(1)),
    ("sin", Arity::Exactly(1)),
    ("sinh", Arity::Exactly(1)),
    ("sqrt", Arity::Exactly(1)),
    ("srand", Arity::Exactly(1)),
    ("tan", Arity::Exactly(1)),
    ("tanh", Arity::Exactly(1)),
    ("wide", Arity::Exactly(1)),
];

/// The function names, for the reference page generator and the REPL.
pub fn names() -> Vec<&'static str> {
    FUNCTIONS.iter().map(|(name, _)| *name).collect()
}

// ── compiling ────────────────────────────────────────────────────────────

/// Lower `name(arg, …)`.
///
/// An unknown name is `invalid command name "tcl::mathfunc::name"`, which is
/// both tclsh's wording and one of the failures
/// [`crate::compiler::defers_to_run_time`] turns into code — so a call in a
/// branch that is never taken costs the script nothing, exactly as in tclsh
/// where the name is resolved by `INST_INVOKE`.
pub(crate) fn compile(c: &mut Compiler, name: &str, args: &[Expr]) -> Result<(), CompileError> {
    let Some(id) = FUNCTIONS.iter().position(|(n, _)| *n == name) else {
        return c.error(format!("invalid command name \"tcl::mathfunc::{name}\""));
    };
    let Ok(argc) = u8::try_from(args.len()) else {
        return c.error("too many arguments for one command");
    };
    for arg in args {
        c.expr(arg)?;
    }
    let id = crate::compiler::ext::MATH_BASE + id as u16;
    c.emit(Op::Extended(id, argc), 1 - args.len() as i32);
    Ok(())
}

// ── running ──────────────────────────────────────────────────────────────

/// `MathFuncWrongNumArgs` (`generic/tclBasic.c`), which names the function by
/// its last `::`-separated component.
fn wrong_args(name: &str, expected: usize, found: usize) -> String {
    let which = if found < expected {
        "not enough"
    } else {
        "too many"
    };
    format!("{which} arguments for math function \"{name}\"")
}

/// `Tcl_GetNumberFromObj`'s refusal.
fn not_a_number(v: &Value) -> String {
    format!(
        "expected number but got {}",
        named(&crate::runtime::tcl_str(v), 50)
    )
}

/// A number in any of Tcl's three kinds, NaN included — `Tcl_GetNumberFromObj`,
/// which reports `TCL_NUMBER_NAN` rather than refusing it.
fn number(v: &Value) -> Result<Num, String> {
    tcl_num(v).map_err(|_| not_a_number(v))
}

/// `Tcl_GetDoubleFromObj`: every numeric kind converts, and a NaN is refused
/// with a message of its own.
fn double(v: &Value) -> Result<f64, String> {
    match tcl_num(v) {
        Ok(Num::Float(f)) if f.is_nan() => Err("floating point value is Not a Number".to_string()),
        Ok(n) => Ok(as_f64(&n)),
        Err(_) => Err(format!(
            "expected floating-point number but got {}",
            named(&crate::runtime::tcl_str(v), 50)
        )),
    }
}

fn as_f64(n: &Num) -> f64 {
    match n {
        Num::Int(i) => *i as f64,
        Num::Float(f) => *f,
        Num::Big(b) => b.to_f64().unwrap_or(f64::INFINITY),
    }
}

/// `CheckDoubleResult`: a NaN is a domain error, and an infinity — which only
/// ever arrives here with `ERANGE` set — is the answer.
fn checked(f: f64) -> Result<Value, String> {
    if f.is_nan() {
        return Err("domain error: argument not in valid range".to_string());
    }
    Ok(Value::Float(f))
}

/// The refusal `Tcl_InitBignumFromDouble` raises for a non-finite double.
fn too_large() -> String {
    "integer value too large to represent".to_string()
}

/// `Tcl_InitBignumFromDouble`: the exact integer a finite double holds.
fn big_from_double(d: f64) -> Result<BigInt, String> {
    BigInt::from_f64(d.trunc()).ok_or_else(too_large)
}

/// An integral `Num` as a value, `i64` where it fits.
fn integer(n: Num) -> Value {
    match n {
        Num::Int(i) => Value::Int(i),
        Num::Big(b) => from_big(b),
        Num::Float(f) => Value::Float(f),
    }
}

/// `TclFloor` (`generic/tclStrToD.c`): the largest double no greater than the
/// integer — a *directed* rounding, where `Tcl_GetDoubleFromObj` rounds to
/// nearest. Recursion is the C function's own: each of the pair defers the
/// negative case to the other on the magnitude.
fn floor_big(a: &BigInt) -> f64 {
    if a.sign() == num_bigint::Sign::Minus {
        return -ceil_big(&-a);
    }
    let bits = a.bits() as i64;
    if bits > 1024 {
        return f64::MAX;
    }
    let shift = 53 - bits;
    let b = if shift > 0 { a << shift } else { a >> -shift };
    scale(&b, bits)
}

/// `TclCeil`: the smallest double no less than the integer.
fn ceil_big(a: &BigInt) -> f64 {
    if a.sign() == num_bigint::Sign::Minus {
        return -floor_big(&-a);
    }
    let bits = a.bits() as i64;
    if bits > 1024 {
        return f64::INFINITY;
    }
    let shift = 53 - bits;
    let mut b = if shift > 0 {
        a << shift
    } else {
        let truncated = a >> -shift;
        // Round away from zero when the shift dropped any bit, which is what
        // makes this the *ceiling* rather than the floor.
        if &truncated << -shift == *a {
            truncated
        } else {
            truncated + 1u32
        }
    };
    if b.bits() as i64 > 53 {
        // Carrying out of 53 bits: 2^53 is still exact, so only the scale moves.
        b >>= 1;
        return scale(&b, bits + 1);
    }
    scale(&b, bits)
}

/// The mantissa `b` — at most 53 bits, so exact as a double — put back at the
/// magnitude `bits` names. `2f64.powi` cannot overflow here: the exponent is at
/// most 1024 − 53.
fn scale(b: &BigInt, bits: i64) -> f64 {
    let mantissa = b.to_f64().unwrap_or(0.0);
    mantissa * 2f64.powi((bits - 53) as i32)
}

/// `TclGetWideBitsFromObj`: an integer's low 64 bits. A double has none, and
/// the caller reports that with the empty message tclsh leaves behind.
fn wide_bits(n: &Num) -> Option<i64> {
    match n {
        Num::Int(i) => Some(*i),
        Num::Float(_) => None,
        Num::Big(b) => {
            let mask = (BigInt::from(1u8) << 64u32) - 1u8;
            (b & mask).to_u64().map(|u| u as i64)
        }
    }
}

// The random seed. tclsh keeps one per interpreter (`Interp::randSeed`); this
// crate's interpreters are per thread, so the state is too. Nothing
// observable rides on the difference: `rand()` is only reproducible after an
// explicit `srand()`, and that pairing is preserved.
thread_local! {
    static RAND_SEED: std::cell::Cell<Option<i64>> = const { std::cell::Cell::new(None) };
}

const RAND_IA: i64 = 16807;
const RAND_IM: i64 = 2147483647;
const RAND_IQ: i64 = 127773;
const RAND_IR: i64 = 2836;
const RAND_MASK: i64 = 123459876;

/// `ExprRandFunc`'s recurrence: Park & Miller's minimal standard generator with
/// Schrage's factorization, seeded from the clock on first use.
fn next_random() -> f64 {
    RAND_SEED.with(|cell| {
        let mut seed = cell.get().unwrap_or_else(|| {
            let clicks = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_micros() as i64)
                .unwrap_or(1);
            clamp_seed(clicks)
        });
        let tmp = seed / RAND_IQ;
        seed = RAND_IA * (seed - tmp * RAND_IQ) - RAND_IR * tmp;
        if seed < 0 {
            seed += RAND_IM;
        }
        cell.set(Some(seed));
        seed as f64 * (1.0 / RAND_IM as f64)
    })
}

/// `1 <= seed <= 2^31 - 2`, as both `ExprRandFunc` and `ExprSrandFunc` enforce.
fn clamp_seed(raw: i64) -> i64 {
    let seed = raw & 0x7FFF_FFFF;
    if seed == 0 || seed == 0x7FFF_FFFF {
        seed ^ RAND_MASK
    } else {
        seed
    }
}

/// The IEEE class of a value, as `DoubleObjClass` computes it: an integer of
/// any width is classified by the double it converts to, and a NaN is its own
/// class without a conversion.
#[derive(PartialEq, Eq, Clone, Copy)]
enum Class {
    Nan,
    Infinite,
    Zero,
    Subnormal,
    Normal,
}

fn classify(v: &Value) -> Result<Class, String> {
    let d = match number(v)? {
        Num::Float(f) if f.is_nan() => return Ok(Class::Nan),
        n => as_f64(&n),
    };
    Ok(match d.classify() {
        std::num::FpCategory::Nan => Class::Nan,
        std::num::FpCategory::Infinite => Class::Infinite,
        std::num::FpCategory::Zero => Class::Zero,
        std::num::FpCategory::Subnormal => Class::Subnormal,
        std::num::FpCategory::Normal => Class::Normal,
    })
}

/// Run one math function. `id` is its index in [`FUNCTIONS`] and `arg` the
/// actual argument count.
pub(crate) fn extension(vm: &mut fusevm::VM, id: u16, arg: u8) -> Result<(), String> {
    let index = (id - crate::compiler::ext::MATH_BASE) as usize;
    let (name, arity) = &FUNCTIONS[index];
    let argc = arg as usize;
    let mut args = Vec::with_capacity(argc);
    for _ in 0..argc {
        args.push(vm.pop());
    }
    args.reverse();

    // Arity first, and in tclsh's counting, which includes the function name
    // itself in both the expected and the found count.
    match arity {
        Arity::Exactly(n) if argc != *n => {
            return Err(wrong_args(name, n + 1, argc + 1));
        }
        Arity::AtLeastOne if argc == 0 => {
            return Err(wrong_args(name, 2, 1));
        }
        _ => {}
    }

    let value = apply(name, &args)?;
    vm.push(value);
    Ok(())
}

fn apply(name: &str, args: &[Value]) -> Result<Value, String> {
    // The one-argument functions that are plain C library calls on a double.
    if let Some(f) = unary_double(name) {
        return checked(f(double(&args[0])?));
    }
    match name {
        "abs" => match number(&args[0])? {
            Num::Int(i64::MIN) => Ok(from_big(-BigInt::from(i64::MIN))),
            Num::Int(i) => Ok(Value::Int(i.abs())),
            Num::Float(f) if f.is_nan() => Err("floating point value is Not a Number".to_string()),
            // `abs(-0.0)` is `0.0`, and `abs(0.0)` keeps the value it was
            // given: `ExprAbsFunc` negates only what compares below `-0.0` or
            // *is* the negative zero (Tcl bug 2954959).
            Num::Float(f) => Ok(Value::Float(if f > -0.0 { f } else { -f })),
            Num::Big(b) => Ok(from_big(if b.sign() == num_bigint::Sign::Minus {
                -b
            } else {
                b
            })),
        },
        "bool" => Ok(Value::Int(tcl_bool(&args[0])? as i64)),
        // `ExprCeilFunc` asks `Tcl_GetDoubleFromObj` first and only then
        // whether the argument is an integer, so a string that is neither is
        // refused in the *double* wording rather than the number one.
        "ceil" | "floor" => {
            let d = double(&args[0])?;
            let up = name == "ceil";
            match number(&args[0])? {
                // A double stays with the C library's `ceil`/`floor`; only an
                // integer takes the exact directed rounding.
                Num::Float(_) => Ok(Value::Float(if up { d.ceil() } else { d.floor() })),
                Num::Int(i) => {
                    let b = BigInt::from(i);
                    Ok(Value::Float(if up { ceil_big(&b) } else { floor_big(&b) }))
                }
                Num::Big(b) => Ok(Value::Float(if up { ceil_big(&b) } else { floor_big(&b) })),
            }
        }
        "double" => Ok(Value::Float(double(&args[0])?)),
        // `int` and `entier` are the same function in 9.0.4: neither truncates
        // to the machine word, whatever `mathfunc(n)` says about `int`.
        "int" | "entier" | "round" | "wide" => integral(name, &args[0]),
        "isqrt" => isqrt(&args[0]),
        "sqrt" => {
            let d = double(&args[0])?;
            // The one case `ExprSqrtFunc` does not hand to the C library: an
            // integer so large it became an infinity, whose root may still be
            // finite.
            if d.is_infinite() && d > 0.0 {
                if let Ok(Num::Big(b)) = number(&args[0]) {
                    return Ok(Value::Float(b.sqrt().to_f64().unwrap_or(f64::INFINITY)));
                }
            }
            checked(d.sqrt())
        }
        "atan2" => checked(double(&args[0])?.atan2(double(&args[1])?)),
        "fmod" => {
            let (x, y) = (double(&args[0])?, double(&args[1])?);
            checked(libm_fmod(x, y))
        }
        "hypot" => checked(double(&args[0])?.hypot(double(&args[1])?)),
        "pow" => checked(double(&args[0])?.powf(double(&args[1])?)),
        "max" | "min" => extremum(name == "max", args),
        "rand" => Ok(Value::Float(next_random())),
        "srand" => {
            // `ExprSrandFunc` passes `TclGetWideBitsFromObj` a null
            // interpreter, so *every* refusal it makes — a double, a string
            // that is no number at all — carries no message. Reproduced rather
            // than improved: the empty string is what a script's `catch` sees,
            // and `expr {srand("a")}` in tclsh 9.0.4 leaves it empty.
            let bits = tcl_num(&args[0])
                .ok()
                .as_ref()
                .and_then(wide_bits)
                .ok_or_else(String::new)?;
            RAND_SEED.with(|cell| cell.set(Some(clamp_seed(bits))));
            Ok(Value::Float(next_random()))
        }
        "isfinite" => Ok(Value::Int(matches!(
            classify(&args[0])?,
            Class::Zero | Class::Subnormal | Class::Normal
        ) as i64)),
        "isinf" => Ok(Value::Int((classify(&args[0])? == Class::Infinite) as i64)),
        "isnan" => Ok(Value::Int((classify(&args[0])? == Class::Nan) as i64)),
        "isnormal" => Ok(Value::Int((classify(&args[0])? == Class::Normal) as i64)),
        "issubnormal" => Ok(Value::Int((classify(&args[0])? == Class::Subnormal) as i64)),
        "isunordered" => {
            let a = classify(&args[0])?;
            let b = classify(&args[1])?;
            Ok(Value::Int((a == Class::Nan || b == Class::Nan) as i64))
        }
        other => Err(format!("invalid command name \"tcl::mathfunc::{other}\"")),
    }
}

/// C's `fmod`, which Rust's `%` on doubles already is — the remainder with the
/// sign of the dividend — and which answers a NaN for a zero divisor, where
/// `CheckDoubleResult` turns it into the domain error.
fn libm_fmod(x: f64, y: f64) -> f64 {
    x % y
}

/// The functions `ExprUnaryFunc` covers: convert the one argument to a double,
/// call the C library, and check the result for a NaN.
fn unary_double(name: &str) -> Option<fn(f64) -> f64> {
    Some(match name {
        "acos" => f64::acos,
        "asin" => f64::asin,
        "atan" => f64::atan,
        "cos" => f64::cos,
        "cosh" => f64::cosh,
        "exp" => f64::exp,
        "log" => f64::ln,
        "log10" => f64::log10,
        "sin" => f64::sin,
        "sinh" => f64::sinh,
        "tan" => f64::tan,
        "tanh" => f64::tanh,
        _ => return None,
    })
}

/// `int`, `entier`, `round` and `wide` — the four that answer with an integer.
///
/// All three of the first go through `ExprIntFunc`'s conversion; `round` adds
/// the half-away-from-zero step before it, and `wide` takes the low 64 bits of
/// whatever the others produced.
fn integral(name: &str, v: &Value) -> Result<Value, String> {
    let n = number(v)?;
    let converted = match &n {
        Num::Float(f) if f.is_nan() => {
            return Err("floating point value is Not a Number".to_string())
        }
        Num::Float(f) => {
            if name == "round" {
                round_double(*f)?
            } else if *f >= 9223372036854775808.0 || *f <= -9223372036854775808.0 {
                from_big(big_from_double(*f)?)
            } else {
                Value::Int(*f as i64)
            }
        }
        // An integer of any width is already what these answer with.
        other => integer(other.clone()),
    };
    if name != "wide" {
        return Ok(converted);
    }
    let n = number(&converted)?;
    Ok(Value::Int(wide_bits(&n).ok_or_else(too_large)?))
}

/// `ExprRoundFunc`'s split of a double into an integral part and a fraction,
/// with the half-way cases rounded away from zero.
fn round_double(f: f64) -> Result<Value, String> {
    let whole = f.trunc();
    let fraction = f - whole;
    let step = if fraction <= -0.5 {
        -1
    } else if fraction >= 0.5 {
        1
    } else {
        0
    };
    // `WIDE_MAX` and `WIDE_MIN` move inward by one when the step would carry
    // past them, which is why the bound is not simply the machine range.
    let max = 9223372036854775808.0 - if step > 0 { 1.0 } else { 0.0 };
    let min = -9223372036854775808.0 + if step < 0 { 1.0 } else { 0.0 };
    if whole >= max || whole <= min {
        let big = big_from_double(whole)?;
        return Ok(from_big(big + step));
    }
    Ok(Value::Int(whole as i64 + step))
}

/// `ExprIsqrtFunc`: the integer square root, exact for every width.
fn isqrt(v: &Value) -> Result<Value, String> {
    /// `MAX_EXACT` — 2^53, past which a double no longer names every integer.
    const MAX_EXACT: f64 = 9007199254740992.0;
    let negative = || "square root of negative argument".to_string();
    let big = match number(v)? {
        Num::Float(f) if f.is_nan() => {
            return Err("floating point value is Not a Number".to_string())
        }
        Num::Float(f) if f < 0.0 => return Err(negative()),
        // The bound is `<=` for a double and `<` for a machine integer in
        // `ExprIsqrtFunc`; both are reproduced rather than unified.
        Num::Float(f) if f <= MAX_EXACT => return Ok(Value::Int(f.sqrt() as i64)),
        Num::Float(f) => big_from_double(f)?,
        Num::Int(i) if i < 0 => return Err(negative()),
        Num::Int(i) if (i as f64) < MAX_EXACT => return Ok(Value::Int((i as f64).sqrt() as i64)),
        Num::Int(i) => BigInt::from(i),
        Num::Big(b) if b.sign() == num_bigint::Sign::Minus => return Err(negative()),
        Num::Big(b) => b,
    };
    Ok(from_big(big.sqrt()))
}

/// `ExprMaxMinFunc`: every argument is checked for being a number before the
/// comparison that keeps one, and a tie keeps the earlier argument.
fn extremum(want_greater: bool, args: &[Value]) -> Result<Value, String> {
    let mut best: Option<Num> = None;
    for v in args {
        let n = match number(v)? {
            Num::Float(f) if f.is_nan() => {
                return Err("floating point value is Not a Number".to_string())
            }
            n => n,
        };
        let take = match &best {
            None => true,
            Some(current) => match (current, &n) {
                (Num::Int(a), Num::Int(b)) => {
                    if want_greater {
                        b > a
                    } else {
                        b < a
                    }
                }
                (a, b) => match big_cmp(b, a) {
                    Some(std::cmp::Ordering::Greater) => want_greater,
                    Some(std::cmp::Ordering::Less) => !want_greater,
                    _ => false,
                },
            },
        };
        if take {
            best = Some(n);
        }
    }
    Ok(integer(best.expect("arity checked before this")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`FUNCTIONS`] indexes the op ids, so a duplicate name would make one
    /// function unreachable and a `u16` overflow would collide with the next
    /// module's range.
    #[test]
    fn the_table_is_a_usable_id_space() {
        let mut seen = std::collections::HashSet::new();
        for (name, _) in FUNCTIONS {
            assert!(seen.insert(*name), "duplicate math function {name}");
        }
        let last = crate::compiler::ext::MATH_BASE + FUNCTIONS.len() as u16;
        assert!(
            last < crate::compiler::ext::CLOCK_BASE,
            "math ids run into the clock range"
        );
    }

    /// The directed roundings are the reason `floor` and `ceil` are not
    /// `f64::floor` of the argument's double value. Both bounds here were read
    /// off tclsh 9.0.4 (`expr {floor(9223372036854775807)}`).
    #[test]
    fn integers_round_in_a_direction() {
        let n = BigInt::from(9223372036854775807i64);
        assert_eq!(
            crate::runtime::format_double(floor_big(&n)),
            "9.223372036854775e+18"
        );
        assert_eq!(
            crate::runtime::format_double(ceil_big(&n)),
            "9.223372036854776e+18"
        );
        let exact = BigInt::from(1024i64);
        assert_eq!(floor_big(&exact), 1024.0);
        assert_eq!(ceil_big(&exact), 1024.0);
        assert_eq!(floor_big(&-&n), -9223372036854775808.0);
    }

    /// Park & Miller's recurrence, checked against the first value tclsh
    /// answers after `srand(1)`.
    #[test]
    fn the_seeded_sequence_matches() {
        RAND_SEED.with(|cell| cell.set(Some(clamp_seed(1))));
        assert_eq!(
            crate::runtime::format_double(next_random()),
            "7.826369259425611e-6"
        );
        assert_eq!(
            crate::runtime::format_double(next_random()),
            "0.13153778814316625"
        );
    }
}
