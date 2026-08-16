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
    // A variable the nested script creates becomes a local of the procedure —
    // whether or not the body mentions it anywhere, which is the case that has
    // no slot for it and had to grow one at run time. The `info exists ::…`
    // lines are the half a script run against the globals would fail: it leaves
    // the name behind as a global of the same value.
    "proc f {} {eval {set made 7}\nreturn $made}\nputs [f]",
    "proc f {} {eval {set qq 9}}\nf\nputs [info exists ::qq]",
    "proc f {} {set keep 1\neval {set bb 8}}\nf\nputs [info exists ::bb]",
    "proc f {} {eval {set only 1}}\nf\nputs [catch {set only} m]:$m",
    // ...and it is still there for the next script in the same frame.
    "proc f {} {eval {set rr 7}\nreturn [eval {set rr}]}\nputs [f]",
    "proc f {} {set keep 1\neval {set bb 8}\nreturn [eval {set bb}]}\nputs [f]",
    // ...where `info locals` lists it beside the body's own.
    "proc f {} {eval {set dd 1}\nreturn [lsort [info locals]]}\nputs [f]",
    "proc f {} {set keep 1\neval {set cc 3}\nreturn [lsort [info locals]]}\nputs [f]",
    "proc f {} {set keep 1\neval {set cc 3}\nreturn [lsort [info vars]]}\nputs [f]",
    // ...and an `unset` of it takes it away again.
    "proc f {} {set keep 1\neval {set dd 3}\neval {unset dd}\nreturn [eval {info exists dd}]}\nputs [f]",
    "proc f {} {eval {set ee 3}\neval {unset ee}\nreturn [lsort [info locals]]}\nputs [f]",
    // It belongs to the activation, not to the procedure: a second call does not
    // find what the first one made.
    "proc f {n} {if {$n} {eval {set seen 1}}\nreturn [eval {info exists seen}]}\nputs [f 1][f 0]",
    // A body with no locals of its own still reaches a global written the one
    // way a procedure may reach one without declaring it.
    "set g 3\nproc f {} {return [subst {g=$::g}]}\nputs [f]",
    // ── a `::`-qualified name inside a script running in a frame ──
    //
    // `$g` is the frame's local and `$::g` is the interpreter's variable, so a
    // script running against the frame has to answer them differently. Every
    // way into such a script is here, because each reaches the name by its own
    // route: `subst` resolves it as a value, a nested script compiles it, and
    // `uplevel` does both.
    "set g 3\nproc p {} {set z 1\nreturn [subst {g=$::g}]}\nputs [p]",
    "set g 3\nproc p {} {set z 1\nreturn [eval {set ::g}]}\nputs [p]",
    "set g 3\nproc p {} {set z 1\nreturn [uplevel 0 {set ::g}]}\nputs [p]",
    "namespace eval nsx {variable v 9}\nproc p {} {set z 1\nreturn [subst {v=$::nsx::v}]}\nputs [p]",
    "namespace eval nsx {variable v 9}\nproc p {} {set z 1\nreturn [eval {set ::nsx::v}]}\nputs [p]",
    // A write through the qualified spelling reaches the interpreter's variable
    // and stays there, rather than becoming a local of the frame it ran in.
    "set g 3\nproc p {} {set z 1\neval {set ::g 42}}\np\nputs $g",
    "set g 3\nproc p {} {set z 1\neval {set ::g 42}\nreturn [list $::g [info exists z]]}\nputs [p]",
    // ...and the two spellings stay *one* variable where they mean one: at the
    // script's own level, in the same chunk, written one way and read the other.
    "set g 1\nputs $::g\nset ::g 2\nputs $g\nputs [info exists ::g][info exists g]",
    "namespace eval foo {set x 1}\nputs $::foo::x",
    // A qualified name for a variable that does not exist refuses as tclsh's
    // does — the projection is not answering for it, and neither is anything
    // else.
    "proc p {} {set z 1\nreturn [catch {eval {set ::nope}} m]:$m}\nputs [p]",
    // The same, in a body with no local of its own: such an activation used to
    // keep the interpreter's variable table rather than be projected, which is
    // what made a script in one write the *global* whenever a global already
    // wore the name it meant to make a local of.
    "set g 3\nproc p {} {eval {set g 99}}\np\nputs $g",
    "set g 3\nproc p {} {eval {set g 99}\nreturn [eval {list $::g $g}]}\nputs [p]",
    "set g 3\nproc p {} {eval {set g 99}\nreturn [lsort [info locals]]}\nputs [p]",
    // ── `info locals` from inside a script running in a frame ──
    //
    // The script is a chunk of its own, compiled at the script's own level,
    // where there is no scope to list: the frame it will be projected into is
    // only known when it runs.
    "proc p {} {eval {set v 1}\nreturn [eval {info locals}]}\nputs [p]",
    "proc p {} {set k 1\neval {set v 1}\nreturn [lsort [eval {info locals}]]}\nputs [p]",
    // ...including a name the script itself creates, which is in the chunk's own
    // slots and not yet in the interpreter's table when it asks.
    "proc p {} {eval {set v 1\nreturn [lsort [info locals]]}}\nputs [p]",
    "proc p {} {set k 1\neval {set v 1\nreturn [lsort [info locals]]}}\nputs [p]",
    // A name the body declared `global` is visible to the script but is not one
    // of the frame's locals.
    "set g 5\nproc p {} {global g\nset k 1\nreturn [lsort [eval {info locals}]]}\nputs [p]",
    // The pattern is applied to the same set, and the level `uplevel` ran in is
    // the one answered for.
    "proc p {} {set kk 1\nset zz 2\nreturn [lsort [eval {info locals k*}]]}\nputs [p]",
    "proc a {} {set av 1\nreturn [b]}\nproc b {} {return [lsort [uplevel 1 {info locals}]]}\nputs [a]",
    "proc p {} {set k 1\nreturn [eval {eval {info locals}}]}\nputs [p]",
    // At the script's own level there is no frame and no local, either asked
    // directly or through a script.
    "set g 1\nputs \"[info locals]|[eval {info locals}]\"",
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
    // A variable the script creates is created in the level it ran in — whether
    // or not the procedure running there ever writes the name, which is the
    // shape that had no slot to be written into.
    "proc a {} {b\nreturn [set made]}\nproc b {} {uplevel 1 {set made 1}}\nputs [a]",
    "proc a {} {set l {}\nb\nreturn $l}\nproc b {} {uplevel 1 {lappend l x\nlappend l y}}\nputs [a]",
    "proc a {} {b\nreturn [lsort [info locals]]}\nproc b {} {uplevel 1 {set made 1}}\nputs [a]",
    "proc a {} {b\nreturn [eval {set grown}]}\nproc b {} {uplevel 1 {set grown 42}}\nputs [a]",
    "proc a {} {set keep 1\nb\nreturn [eval {set g2}]}\nproc b {} {uplevel 1 {set g2 7}}\nputs [a]",
    // ── upvar to a name the target procedure never writes ──
    //
    // A link is the address of one frame slot, and such a name has none until
    // the frame grows one. That the *caller* then sees what the link wrote is
    // the half a slot that went nowhere would fail.
    "proc a {} {b\nreturn [eval {set v}]}\nproc b {} {upvar 1 v z\nset z 5}\nputs [a]",
    "proc a {} {b\nreturn [lsort [info locals]]}\nproc b {} {upvar 1 fresh alias\nset alias 3}\nputs [a]",
    "proc a {} {set n grownup\nb $n\nreturn [eval {set grownup}]}\nproc b {nm} {upvar 1 $nm z\nset z 11}\nputs [a]",
    // A link that is never written creates nothing, in either frame.
    "proc a {} {b\nreturn [eval {info exists ghost}]}\nproc b {} {upvar 1 ghost q\nreturn [info exists q]}\nputs [a]",
    // The name the link made is the caller's local and dies with the call.
    "proc a {} {b}\nproc b {} {upvar 1 late z\nset z 1}\na\nputs [info exists ::late]",
    "proc a {} {b\nreturn [eval {array get arr}]}\nproc b {} {upvar 1 arr(k) e\nset e 7}\nputs [a]",
    // Naming an element *creates the array*, before anything is written through
    // the link and whether or not anything ever is: the target is looked up with
    // `createPart1` set. The array is all that is created — the element itself
    // stays absent, which is what `array size` and `array names` report on.
    "proc a {} {b\nreturn \"[info exists arr] [array exists arr] [array size arr] \
     [array names arr] [lsort [info locals]]\"}\nproc b {} {upvar 1 arr(k) e\nreturn}\nputs [a]",
    "proc b {} {upvar #0 gz(k) e\nreturn}\nb\n\
     puts \"[info exists gz] [array exists gz] [array size gz] [lsort [info globals gz]]\"",
    // ...and an element of a variable that is already a scalar is refused there
    // and then, rather than left as a link that could never be written through.
    "set sc 1\nproc b {} {return [catch {upvar #0 sc(k) e} m]:$m}\nputs [b]",
    "proc a {} {set sc 1\nreturn [b]}\nproc b {} {return [catch {upvar 1 sc(k) e} m]:$m}\nputs [a]",
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
    // ── a variable whose *name* the script computes ──
    //
    // `set $n 1` is the same indirection `upvar` is, without the link: the name
    // is a value, so which variable it is depends on the frame the command runs
    // in. That is why these belong here rather than beside the other `set`
    // cases — every line below would pass against the globals and still be
    // wrong inside a procedure.
    "set n foo\nset $n 42\nputs $foo",
    "set n foo\nputs [set $n 42]",
    "set foo 7\nset n foo\nputs [set $n]",
    "set n c\nincr $n\nincr $n 4\nputs $c",
    "set s pre\nset n s\nputs [append $n X Y]\nputs $s",
    "set n s\nappend $n abc def\nputs $s",
    "set n L\nlappend $n a b\nputs [lappend $n c]\nputs $L",
    // The name is one word of the command, so it is substituted once — a
    // command substitution spelling it must not run twice.
    "set c 0\nproc nm {} {incr ::c\nreturn v}\nset v 1\nappend [nm] X\nputs \"$v $c\"",
    "set c 0\nproc nm {} {incr ::c\nreturn v}\nset v 1\nincr [nm]\nputs \"$v $c\"",
    "set c 0\nproc nm {} {incr ::c\nreturn v}\nlappend [nm] a\nputs \"$v $c\"",
    // Inside a procedure it is that activation's variable, not a global.
    "proc f {} {set a 1\nset n a\nreturn [set $n]}\nputs [f]",
    "proc f {} {set n a\nset $n 9\nreturn $a}\nputs [f]",
    "proc f {} {set n zz\nset $n 3}\nf\nputs [info exists ::zz]",
    "proc f {} {set n q\nset $n 1\nreturn [lsort [info locals]]}\nputs [f]",
    // ...unless the body declared it global, which leaves no trace in the frame
    // and so has to be carried to the op that resolves the name.
    "set g 0\nproc f {} {global g\nset n g\nset $n 5}\nf\nputs $g",
    "set g 0\nproc f {} {global g\nset n g\nincr $n 2}\nf\nputs $g",
    "proc f {} {set n ::h\nset $n 6}\nf\nputs $h",
    // A name `upvar` bound resolves to what the link points at, not to the
    // descriptor sitting in its slot.
    "proc p {vn} {upvar 1 $vn y\nset n y\nset $n 77}\nset z 0\np z\nputs $z",
    "proc p {vn} {upvar 1 $vn y\nset n y\nincr $n 5}\nset z 1\np z\nputs $z",
    // ...which is the same following a second `upvar` through the first needs,
    // and a `dict with` key naming such a name.
    "proc a {vn} {upvar 1 $vn y\nb}\nproc b {} {upvar 1 y q\nset q 5}\nset z 0\na z\nputs $z",
    "proc a {vn} {upvar 1 $vn y\nset d [dict create y 9]\ndict with d {}\nputs $y}\nset z 0\na z\nputs $z",
    // `info exists` must not bring the variable into being by asking.
    "set n v\nputs [info exists $n]",
    "set v 1\nset n v\nputs [info exists $n]",
    "set n v\nputs [info exists $n][info exists v]",
    "proc f {} {set n nope\nreturn [info exists $n][llength [info locals]]}\nputs [f]",
    // `unset` through a computed name, and what an absent one answers.
    "set v 1\nset n v\nunset $n\nputs [info exists v]",
    "set n v\nputs [catch {unset $n} m]:$m",
    "set n v\nunset -nocomplain $n\nputs ok",
    // An `a(i)` spelling the name carries is an array element there too.
    "set a(1) x\nset n a(1)\nputs [set $n]",
    "set n a(2)\nset $n y\nputs [array names a]",
    "set n a(3)\nset $n z\nunset $n\nputs [array names a]",
    // Reading a whole array as a scalar, and an element of a scalar, refuse the
    // way the written-out spellings refuse. The three ways an element read can
    // fail are three different messages, and a computed name reaches all three.
    "set a(1) x\nset n a\nputs [catch {set $n} m]:$m",
    "set b 1\nset n b(1)\nputs [catch {set $n} m]:$m",
    "set n c(1)\nputs [catch {set $n} m]:$m",
    "set a(9) x\nset n a(1)\nputs [catch {set $n} m]:$m",
    "set b 1\nset n b(1)\nputs [catch {set $n v} m]:$m",
    "set a(1) x\nset n a\nputs [catch {set $n v} m]:$m",
    // ...and so does an unset of one.
    "set b 1\nset n b(1)\nputs [catch {unset $n} m]:$m",
    "set a(9) x\nset n a(1)\nputs [catch {unset $n} m]:$m",
    "set b 1\nset n b(1)\nunset -nocomplain $n\nputs ok",
    "set a(9) x\nset n a(1)\nunset -nocomplain $n\nputs ok",
    // A tolerant read — the one `append` and `incr` do — creates the variable
    // rather than refusing, but only where the name could ever have been one.
    "set b 1\nset n b(1)\nputs [catch {append $n x} m]:$m",
    "set b 1\nset n b(1)\nputs [catch {incr $n} m]:$m",
    "set a(9) x\nset n a(1)\nappend $n q\nputs [lsort [array names a]]",
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
///
/// The `upvar` entry that replaced them — a name the target procedure never
/// wrote, which had no frame slot to be the address of — has since gone the same
/// way: a frame grows a slot for such a name when a script asks for one, and the
/// programs above compare the result against tclsh.
#[test]
fn what_the_frame_commands_do_not_do_yet() {
    // A lambda's body is a procedure body, so what a body refuses it refuses.
    // `upvar 1 x y` stood here while `upvar` was absent; from a lambda, level 1
    // is the chunk the synthesised procedure was called from and that level is
    // name-addressed, so `upvar` reaches it. What a lambda body refuses is what
    // any body refuses — here, a `namespace eval` whose unqualified names would
    // become frame slots.
    let src = "puts [apply {{} {namespace eval foo {set x 1}}}]";
    let expected = "\"namespace eval\" inside a procedure is not supported yet";
    let err = tclrs::eval(src).expect_err(&format!("{src:?} should fail"));
    assert!(
        err.contains(expected),
        "{src:?}: expected an error mentioning {expected:?}, got {err:?}"
    );

    // A `break` in a dynamically evaluated script carries its code out to the
    // level the script ran in, which is what a `catch` around it reports and
    // what a loop around it absorbs. Both values are tclsh 9.0.3's.
    let outcome = tclrs::eval("while {1} {puts [catch {eval {break}} m]:$m\nbreak}")
        .expect("the code is caught");
    assert_eq!(outcome.output, "3:\n");

    // Uncaught, the same code reaches the loop and ends it — across `eval`,
    // across `uplevel`, and out of a procedure that returned one.
    for (src, expected) in [
        (
            "set n 0\nwhile {1} {incr n\nif {$n > 3} {eval {break}}}\nputs $n",
            "4\n",
        ),
        (
            "proc stop {} {return -code break}\nset n 0\nwhile {1} {incr n\nstop}\nputs $n",
            "1\n",
        ),
        (
            "proc skip {} {return -code continue}\nset n 0\n\
             for {set i 0} {$i < 5} {incr i} {if {$i == 2} {skip}\nincr n}\nputs $n",
            "4\n",
        ),
        // A `return` from the script `uplevel` ran returns from the procedure
        // that ran it, which is the level the script belongs to.
        ("proc f {} {return [uplevel 1 {return x}]}\nputs [f]", "x\n"),
    ] {
        let outcome = tclrs::eval(src).unwrap_or_else(|e| panic!("{src:?} failed: {e}"));
        assert_eq!(outcome.output, expected, "{src:?}");
    }
}
