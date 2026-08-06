//! Differential execution of the commands that reach another frame's
//! variables: `eval` inside a procedure body, `uplevel` and `apply`.
//!
//! Same contract as `proc_differential.rs` — no expected value is written by
//! hand. Every program below is run by tclsh and by tclrs and the two stdouts
//! are compared byte for byte, so which frame a script runs in, which variables
//! it may see, what a write through it does to the caller and the exact wording
//! of a `bad level` are checked against the reference implementation rather than
//! against a reading of the manual page.
//!
//! What makes this worth its own file: every program here depends on a fact that
//! is only true at run time — how deep the call stack is. A procedure's locals
//! are frame slots the compiler assigned, so a nested script can only find them
//! through the names the chunk records for that frame
//! (`fusevm::Chunk::sub_slot_names`, published by `src/procs.rs`). A test that
//! merely checked a value could pass while the script ran against the globals; a
//! test that compares against tclsh cannot, because tclsh refuses a bare read of
//! an undeclared global from inside a procedure and the globals do not.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const PROGRAMS: &[&str] = &[
    // ── eval inside a procedure body: the frame it runs in ──
    // A write reaches the procedure's own local, not a global of the same name.
    "proc f {} {set x 1\neval {set x 2}\nreturn $x}\nputs [f]",
    "set x outer\nproc f {} {set x inner\neval {set x changed}\nreturn $x}\nputs [f]\nputs $x",
    // A variable the nested script creates becomes a local of the procedure.
    "proc f {} {eval {set made 7}\nreturn $made}\nputs [f]",
    // A read sees the procedure's locals.
    "proc f {} {set x 5\nreturn [eval {expr {$x * 2}}]}\nputs [f]",
    "proc f {a b} {return [eval {expr {$a + $b}}]}\nputs [f 20 22]",
    // ...and only those: a global the body did not declare is refused, exactly
    // as a bare read of it in the body would be. This is the case a script run
    // against the globals would answer instead of refusing.
    "set nosuch global-value\nproc f {} {return [catch {eval {set nosuch}} m]:$m}\nputs [f]",
    // A declared global is visible, and a write to one persists.
    "set g 3\nproc f {} {global g\nreturn [eval {expr {$g + 1}}]}\nputs [f]",
    "set g 0\nproc f {} {global g\neval {set g 9}}\nf\nputs $g",
    "set g 1\nproc f {} {global g\nset l 2\nreturn [eval {expr {$g + $l}}]}\nputs [f]",
    // A callee cannot see its caller's locals through an eval, any more than it
    // can see them directly.
    "proc a {} {set secret 1\nb}\nproc b {} {return [catch {eval {set secret}}]}\nputs [a]",
    // Each frame of a recursive procedure has its own.
    "proc f {n} {set here $n\neval {set here [expr {$here * 10}]}\nif {$n > 0} {f [expr {$n - 1}]}\nreturn $here}\nputs [f 2]",
    // The nested script is itself a script: it may nest, loop and be built.
    "proc f {} {set x 5\nreturn [eval {eval {expr {$x * 3}}}]}\nputs [f]",
    "proc f {} {set t 0\nforeach n {1 2 3} {eval {incr t $n}}\nreturn $t}\nputs [f]",
    "proc f {} {set x 1\neval incr x 5\nreturn $x}\nputs [f]",
    "proc f {} {set s hello\nreturn [eval [list string toupper $s]]}\nputs [f]",
    // What a failing script set before it failed is set, in the frame it ran in.
    "proc f {} {set p none\ncatch {eval {set p half\nerror stop}}\nreturn $p}\nputs [f]",
    // ── uplevel: which level a script runs in ──
    "proc a {} {set caller 99\nb}\nproc b {} {return [uplevel 1 {set caller}]}\nputs [a]",
    "proc a {} {set w 1\nb\nreturn $w}\nproc b {} {uplevel 1 {set w 42}}\nputs [a]",
    // A variable the script creates is created in the level it ran in.
    "proc a {} {b\nreturn [set made]}\nproc b {} {uplevel 1 {set made 1}}\nputs [a]",
    "proc a {} {set l {}\nb\nreturn $l}\nproc b {} {uplevel 1 {lappend l x\nlappend l y}}\nputs [a]",
    // The level's own locals are what is visible — not the caller's, which are
    // one level further in.
    "proc a {} {set mine 1\nb}\nproc b {} {set mine 2\nreturn [uplevel 1 {set mine}]}\nputs [a]",
    // Level 0 is the frame the command is in.
    "proc f {} {set v 7\nreturn [uplevel 0 {set v}]}\nputs [f]",
    "set g gv\nputs [uplevel 0 {set g}]",
    // Counting outwards, and counting from the global level inwards.
    "proc a {} {set outer deep\nb}\nproc b {} {c}\nproc c {} {return [uplevel 2 {set outer}]}\nputs [a]",
    "set g gv\nproc f {} {return [uplevel #0 {set g}]}\nputs [f]",
    "proc a {} {set v top\nb}\nproc b {} {return [uplevel #1 {set v}]}\nputs [a]",
    // A procedure the top level called reaches the globals with `uplevel 1`,
    // and needs no `global` declaration to do it.
    "set g 5\nproc f {} {return [uplevel 1 {expr {$g * 2}}]}\nputs [f]",
    // The level word is optional and defaults to 1.
    "proc a {} {set c 8\nb}\nproc b {} {return [uplevel {set c}]}\nputs [a]",
    // A level that does not exist is reported, with the word as written.
    "puts [catch {uplevel 1 {set x 1}} m]:$m",
    "puts [catch {uplevel 2 {set x 1}} m]:$m",
    "puts [catch {uplevel #1 {set x 1}} m]:$m",
    "proc f {} {return [catch {uplevel 2 {set x 1}} m]:$m}\nputs [f]",
    "proc f {} {return [catch {uplevel 9 {set x 1}} m]:$m}\nputs [f]",
    // Several arguments are concatenated as `concat` does, which is what makes
    // `uplevel $cmd $args` work — and what strips one level of bracing.
    "set g gv\nputs [uplevel #0 {set} {g}]",
    "proc a {} {set n 1\nb}\nproc b {} {uplevel 1 incr n 4}\nputs [a]",
    "proc a {} {set y 0\nb\nreturn $y}\nproc b {} {puts [catch {uplevel 1 set y {a b}} m]:$m}\nputs [a]",
    // A write through an uplevel in a loop lands in the same variable each time.
    "proc a {} {set t 0\nfor {set i 0} {$i < 3} {incr i} {b}\nreturn $t}\nproc b {} {uplevel 1 {incr t 5}}\nputs [a]",
    // ── apply: a lambda is a procedure with its own frame ──
    "puts [apply {{a b} {expr {$a + $b}}} 1 2]",
    "puts [apply {{} {return nine}}]",
    "puts [apply {{x} {expr {$x * 2}} ::} 21]",
    "puts [apply {{a {b 10}} {expr {$a + $b}}} 5]",
    "puts [apply {{a args} {return \"$a:[llength $args]\"}} 1 2 3]",
    // Its locals are its own, and `return` returns from it.
    "puts [apply {{} {set v 1\nincr v\nreturn $v}}]",
    "puts [apply {{} {return early\nreturn late}}]",
    "puts [apply {{n} {set t 0\nfor {set i 0} {$i < $n} {incr i} {incr t $i}\nreturn $t}} 5]",
    // A lambda may be applied from a procedure, from another lambda, and from a
    // script an `eval` is running.
    "proc host {} {return [apply {{a} {expr {$a + 1}}} 41]}\nputs [host]",
    "puts [apply {{n} {apply {{m} {expr {$m * 3}}} $n}} 4]",
    "proc f {} {set x 5\nreturn [eval {apply {{a} {expr {$a * 2}}} $x}]}\nputs [f]",
    // The argument count is reported against the lambda, not against a name.
    "puts [catch {apply {{a b} {expr 1}} 1} m]:$m",
    "puts [catch {apply {{a} {expr 1}} 1 2} m]:$m",
    // A lambda that is not two or three elements, or whose namespace is not the
    // one this frontend has, is not a lambda.
    "puts [catch {apply {{a}} 1} m]:$m",
    "puts [catch {apply {{a} {expr 1} :: extra} 1} m]:$m",
    "puts [catch {apply notalambda} m]:$m",
];

fn tclsh() -> Option<PathBuf> {
    for name in ["tclsh", "tclsh9.0", "tclsh8.6"] {
        if let Ok(out) = Command::new("sh")
            .arg("-c")
            .arg(format!("command -v {name}"))
            .output()
        {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }
    }
    None
}

static SEQ: AtomicUsize = AtomicUsize::new(0);

fn reference_output(tclsh: &PathBuf, program: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "tclrs-frame-{}-{}.tcl",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path, program).expect("write program");
    let out = Command::new(tclsh).arg(&path).output().expect("run tclsh");
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "tclsh rejected program:\n{program}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn frame_commands_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };

    let mut failures = Vec::new();
    for program in PROGRAMS {
        let expected = reference_output(&tclsh, program);
        match tclrs::eval(program) {
            Ok(outcome) if outcome.output == expected => {}
            Ok(outcome) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs: {:?}",
                outcome.output
            )),
            Err(e) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs failed: {e}"
            )),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} programs diverge:\n\n{}",
        failures.len(),
        PROGRAMS.len(),
        failures.join("\n\n")
    );
}

/// The nested script must run against the frame, not against a copy of it: a
/// value it writes has to be there for the *next* command of the body, not only
/// at the end.
///
/// A projection that were written back only when the procedure returned would
/// pass every program above and fail this, which is why it is checked
/// separately.
#[test]
fn a_write_through_a_nested_script_is_visible_immediately() {
    let outcome = tclrs::eval(
        "proc f {} {\n\
         set x 1\n\
         eval {set x 2}\n\
         puts \"during: $x\"\n\
         eval {incr x}\n\
         puts \"after: $x\"\n\
         return $x\n\
         }\n\
         puts \"result: [f]\"",
    )
    .expect("eval in a procedure body");
    assert_eq!(outcome.output, "during: 2\nafter: 3\nresult: 3\n");
}

/// `yield` inside a script run by `eval`, `uplevel` or `apply` is refused, and
/// says why.
///
/// tclsh suspends the coroutine from inside the nested script and resumes into
/// the middle of it. Here the nested script runs a machine of its own, several
/// Rust frames below the VM that would have to park, and that VM saves only its
/// own state — so resuming could not come back to where the script left off.
///
/// The refusal is the point of the test. Approximating it would lose whatever
/// the nested script had set, silently, at a yield; and the message has to be
/// this one rather than the reference interpreter's `can only be called in a
/// coroutine`, which would be false — the yield *is* in a coroutine.
#[test]
fn a_yield_inside_a_nested_script_is_refused_and_says_why() {
    let err =
        tclrs::eval("proc gen {} {eval {yield first}\nreturn done}\ncoroutine c gen\nputs [c]")
            .expect_err("a yield inside an eval should be refused");
    assert!(
        err.contains(
            "yield inside a script run by \"eval\", \"uplevel\" or \"apply\" is not \
                      supported"
        ),
        "got {err:?}"
    );

    // Outside a coroutine the message stays the reference interpreter's, which
    // `tests/coroutine_differential.rs` compares against tclsh.
    let err = tclrs::eval("puts [eval {yield x}]").expect_err("no coroutine to yield from");
    assert!(
        err.contains("yield can only be called in a coroutine"),
        "got {err:?}"
    );

    // An `eval` that does not yield is unaffected inside a coroutine: the
    // refusal is that one case and not the mechanism.
    let outcome = tclrs::eval(
        "proc gen {} {set n [eval {expr {2 + 3}}]\nyield $n\nreturn done}\n\
         coroutine c gen\nputs [c]",
    )
    .expect("an eval inside a coroutine that does not yield");
    assert_eq!(outcome.output, "done\n");
}

/// What these three commands still refuse, and in which words.
///
/// `upvar` used to be absent rather than refused — `invalid command name`, which
/// is what a Tcl interpreter says for a command it does not have, and was the
/// truth here. Two entries pinned that, and this is the test that says the day
/// came: `upvar` is implemented, its computed-name form included
/// (`proc f {n} {upvar 1 $n v}` sets the caller's variable, as tclsh does), and a
/// lambda's body reaches it like any procedure body's. Both entries moved to what
/// `upvar` still refuses, which is a name the target procedure never wrote — a
/// link is the address of one frame slot, and such a name has none — and, for the
/// lambda, to what any procedure body refuses.
#[test]
fn what_the_frame_commands_do_not_do_yet() {
    for (src, expected) in [
        // A caller's variable the caller itself never names.
        (
            "proc f {} {upvar 1 neverused v\nset v 2}\nproc g {} {f}\ng",
            "the procedure running there never names it",
        ),
        (
            "proc f {} {return [uplevel 1 {return x}]}\nputs [f]",
            "\"return\" outside of a procedure",
        ),
        // A lambda's body is a procedure body, so what a body refuses it
        // refuses. `upvar 1 x y` stood here while `upvar` was absent; from a
        // lambda, level 1 is the chunk the synthesised procedure was called from
        // and that level is name-addressed, so `upvar` reaches it. What a lambda
        // body refuses is what any body refuses — here, a `namespace eval` whose
        // unqualified names would become frame slots.
        (
            "puts [apply {{} {namespace eval foo {set x 1}}}]",
            "\"namespace eval\" inside a procedure is not supported yet",
        ),
    ] {
        let err = tclrs::eval(src).expect_err(&format!("{src:?} should fail"));
        assert!(
            err.contains(expected),
            "{src:?}: expected an error mentioning {expected:?}, got {err:?}"
        );
    }

    // A `break` in a dynamically evaluated script cannot carry its code out to a
    // loop in the level the script ran in: the script is a chunk of its own, and
    // this frontend does not propagate a return code across one. It raises
    // instead, so `catch` answers 1 where tclsh answers 3 and the loop breaks.
    //
    // This is a divergence rather than a refusal, and it is `eval`'s rather than
    // `uplevel`'s — `uplevel` inherits it. Pinned here with the value tclsh
    // gives, so the day the codes propagate this test is what says so. Recorded
    // in BUGS.md.
    let outcome = tclrs::eval("while {1} {puts [catch {eval {break}} m]:$m\nbreak}")
        .expect("the raise is caught");
    assert_eq!(outcome.output, "1:invoked \"break\" outside of a loop\n");
    // tclsh: "3:\n"
}
