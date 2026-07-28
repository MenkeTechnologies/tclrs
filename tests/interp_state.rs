//! The interpreter object: state held between evaluations, and the chunk cache
//! that keeps a repeated `eval` from recompiling.
//!
//! What these assert is not observable from outside a process — how many times
//! a script was compiled, and whether two evaluations saw one set of variables
//! — so they are asserted directly rather than against `tclsh`. What *is*
//! observable is compared with `tclsh` in `cli_differential`, which drives the
//! same code through the binary.

use tclrs::Interp;

/// A variable set by one evaluation is there for the next. Without this a REPL
/// is a sequence of unrelated processes.
#[test]
fn variables_survive_between_evaluations() {
    let mut interp = Interp::capturing();
    assert_eq!(interp.eval("set x 5").unwrap(), "5");
    assert_eq!(interp.eval("expr {$x * 2}").unwrap(), "10");
    assert_eq!(interp.eval("incr x").unwrap(), "6");
    assert_eq!(interp.eval("set x").unwrap(), "6");
}

/// Arrays and dicts are values in the same store, not a second one.
#[test]
fn arrays_and_dicts_survive_between_evaluations() {
    let mut interp = Interp::capturing();
    interp.eval("array set a {x 1 y 2}").unwrap();
    assert_eq!(interp.eval("array size a").unwrap(), "2");
    assert_eq!(interp.eval("set a(x)").unwrap(), "1");
    interp.eval("set a(z) 3").unwrap();
    assert_eq!(interp.eval("lsort [array names a]").unwrap(), "x y z");

    interp.eval("set d [dict create k v]").unwrap();
    assert_eq!(interp.eval("dict get $d k").unwrap(), "v");
}

/// `unset` has to survive too, which is why an unassigned slot removes the
/// variable rather than storing an empty value.
#[test]
fn unset_survives_between_evaluations() {
    let mut interp = Interp::capturing();
    interp.eval("set x 5").unwrap();
    assert_eq!(interp.global("x").as_deref(), Some("5"));
    interp.eval("unset x").unwrap();
    assert_eq!(interp.global("x"), None);
}

/// The variables a failed evaluation set before it failed are set, as they are
/// in the reference interpreter.
#[test]
fn state_written_before_a_failure_is_kept() {
    let mut interp = Interp::capturing();
    interp.eval("set a 1").unwrap();
    interp.eval("set b 2\nnosuchcmd").unwrap_err();
    // `nosuchcmd` is refused while compiling, so this evaluation never ran and
    // `b` was never set — but `a`, from the evaluation before, is untouched.
    assert_eq!(interp.global("a").as_deref(), Some("1"));

    interp.eval("set c 3\nputs [expr {1/0}]").unwrap_err();
    assert_eq!(interp.global("c").as_deref(), Some("3"));
}

/// Output accumulates across evaluations until it is taken.
#[test]
fn captured_output_accumulates_until_taken() {
    let mut interp = Interp::capturing();
    interp.eval("puts a").unwrap();
    interp.eval("puts b").unwrap();
    assert_eq!(interp.take_output(), "a\nb\n");
    assert_eq!(interp.take_output(), "");
    interp.eval("puts c").unwrap();
    assert_eq!(interp.take_output(), "c\n");
}

/// The host sets `argv0` / `argc` / `argv` this way.
#[test]
fn host_set_variables_are_readable_by_a_script() {
    let mut interp = Interp::capturing();
    interp.set_global("argv", "a b c");
    assert_eq!(interp.eval("llength $argv").unwrap(), "3");
    assert_eq!(interp.eval("lindex $argv 1").unwrap(), "b");
}

// ── the runtime-eval path ────────────────────────────────────────────────

/// A script built at run time is evaluated against the same state as the script
/// that built it, in both directions.
#[test]
fn eval_shares_the_interpreters_state() {
    let mut interp = Interp::capturing();
    interp.eval("set outer 1").unwrap();
    // Reads what was already set …
    assert_eq!(interp.eval("eval {expr {$outer + 1}}").unwrap(), "2");
    // … and writes what is read afterwards.
    interp.eval("eval {set inner 7}").unwrap();
    assert_eq!(interp.eval("set inner").unwrap(), "7");
    // A value written by a nested script is visible to the outer one at the
    // very next command, not only at the next evaluation.
    assert_eq!(
        interp.eval("set v 1\neval {set v 2}\nexpr {$v}").unwrap(),
        "2"
    );
}

/// The script text can be a variable, which is what the compiler cannot see and
/// this path exists for.
#[test]
fn eval_runs_a_script_that_is_not_a_literal() {
    let mut interp = Interp::capturing();
    interp.eval("set cmd {set answer 42}").unwrap();
    interp.eval("eval $cmd").unwrap();
    assert_eq!(interp.global("answer").as_deref(), Some("42"));
}

/// Nesting is not special-cased: a nested script may itself `eval`.
#[test]
fn eval_nests() {
    let mut interp = Interp::capturing();
    assert_eq!(
        interp.eval("eval {eval {eval {expr {6 * 7}}}}").unwrap(),
        "42"
    );
    interp.eval("eval {eval {set deep found}}").unwrap();
    assert_eq!(interp.global("deep").as_deref(), Some("found"));
}

/// A failure inside a nested script is the evaluation's failure, reported with
/// the nested script's own message.
#[test]
fn a_failure_inside_eval_propagates() {
    let mut interp = Interp::capturing();
    let e = interp.eval("eval {nosuchcmd}").unwrap_err();
    assert_eq!(e.msg, "invalid command name \"nosuchcmd\"");

    let e = interp.eval("eval {set x {").unwrap_err();
    assert_eq!(e.msg, "missing close-brace");

    let e = interp.eval("eval {expr {1/0}}").unwrap_err();
    assert_eq!(e.msg, "divide by zero");
}

/// What a nested script set before it failed is set — the write-back happens on
/// the way out either way.
#[test]
fn state_from_a_failed_eval_is_kept() {
    let mut interp = Interp::capturing();
    interp.eval("eval {set kept 1; expr {1/0}}").unwrap_err();
    assert_eq!(interp.global("kept").as_deref(), Some("1"));
}

/// Nesting is bounded, and reaching the bound is an error rather than a stack
/// overflow. The limit is lowered here so the test costs nothing; the default
/// is checked against `tclsh` in `cli_differential`.
#[test]
fn nesting_is_bounded_by_an_error_not_a_crash() {
    let mut interp = Interp::capturing();
    interp.set_recursion_limit(8);

    let nested = |depth: usize| {
        let mut script = "set n ok".to_string();
        for _ in 0..depth {
            script = format!("eval {{{script}}}");
        }
        script
    };

    assert_eq!(interp.eval(&nested(8)).unwrap(), "ok");
    let e = interp.eval(&nested(9)).unwrap_err();
    assert_eq!(e.msg, "too many nested evaluations (infinite loop?)");

    // The depth is given back, so a refused script does not poison the ones
    // after it.
    assert_eq!(interp.eval(&nested(8)).unwrap(), "ok");
}

// ── the chunk cache ──────────────────────────────────────────────────────

/// The point of the cache: the same `eval` text in a loop is lowered once,
/// however many times it runs.
#[test]
fn a_repeated_eval_compiles_once() {
    let mut interp = Interp::capturing();
    let (_, before) = interp.cache_stats();
    interp
        .eval("set i 0\nwhile {$i < 100} {eval {incr i}}")
        .unwrap();
    let (hits, after) = interp.cache_stats();
    assert_eq!(interp.global("i").as_deref(), Some("100"));
    // Two compilations: the outer script, and `incr i` on the first pass.
    assert_eq!(
        after - before,
        2,
        "expected 2 compilations, got {}",
        after - before
    );
    // The other 99 passes were served from the cache.
    assert_eq!(hits, 99);
}

/// Two different scripts are two entries — the key is the text, so nothing is
/// conflated.
#[test]
fn distinct_eval_texts_are_distinct_entries() {
    let mut interp = Interp::capturing();
    let (_, before) = interp.cache_stats();
    interp
        .eval("set out {}\nforeach n {1 2 3} {eval \"lappend out [expr {$n * 10}]\"}")
        .unwrap();
    let (_, after) = interp.cache_stats();
    assert_eq!(interp.global("out").as_deref(), Some("10 20 30"));
    // The outer script plus one per distinct nested text.
    assert_eq!(after - before, 4);
}

/// A script that does not compile is not cached: the diagnostic is produced
/// again, and no slot is spent on it.
#[test]
fn a_script_that_fails_to_compile_is_not_cached() {
    let mut cache = tclrs::cache::ChunkCache::new();
    assert!(cache.compile("nosuchcmd").is_err());
    assert!(cache.compile("nosuchcmd").is_err());
    assert!(cache.is_empty());
    assert_eq!(cache.stats(), (0, 2));
}

/// The cache holds a bounded number of scripts, and stays correct across the
/// point where it drops what it held.
#[test]
fn the_cache_is_bounded() {
    let mut cache = tclrs::cache::ChunkCache::with_capacity(4);
    for i in 0..12 {
        cache.compile(&format!("set x {i}")).expect("compiles");
        assert!(cache.len() <= 4, "cache grew to {}", cache.len());
    }

    let mut interp = Interp::capturing();
    for i in 0..8 {
        interp.eval(&format!("eval {{set x {i}}}")).unwrap();
        assert_eq!(interp.global("x").as_deref(), Some(i.to_string().as_str()));
    }
}

/// One interpreter's state is its own.
#[test]
fn interpreters_do_not_share_state() {
    let mut a = Interp::capturing();
    let mut b = Interp::capturing();
    a.eval("set x mine").unwrap();
    assert_eq!(a.global("x").as_deref(), Some("mine"));
    assert_eq!(b.global("x"), None);
    b.eval("set x theirs").unwrap();
    assert_eq!(a.global("x").as_deref(), Some("mine"));
}

/// The one-shot `eval` keeps working, and keeps building a fresh interpreter
/// every call.
#[test]
fn the_one_shot_eval_is_still_one_shot() {
    assert_eq!(tclrs::eval("set x 5").unwrap().result, "5");
    assert_eq!(tclrs::eval("puts hi").unwrap().output, "hi\n");
    // No state from the call before.
    assert_eq!(tclrs::eval("set y $x").unwrap().result, "");
}
