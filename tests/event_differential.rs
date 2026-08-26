//! Differential execution of the event loop and the scope commands: `after`,
//! `update`, `vwait`, `uplevel`, `upvar`, `variable`, `apply` and `info`.
//!
//! Same contract as `execution_differential.rs` — no expected value is written
//! by hand. Every program below is run by tclsh 9.0.4 and by tclrs and the two
//! stdouts are compared byte for byte, so the order `update` runs a timer and an
//! idle handler in, the shape of an `after#N` handle, the order `after info`
//! lists them, and what `uplevel #0` writes are all checked against the
//! reference implementation.
//!
//! **Two things are deliberately outside the byte-for-byte comparison.**
//!
//! * *Wall-clock durations.* A program that measures how long `after 40` slept
//!   would compare two machines' scheduling noise. The delays here are asserted
//!   as bounds, in [`a_bare_after_blocks_for_at_least_the_delay`].
//! * *Names that come from the host or from Tcl's script library.* `info
//!   hostname`, `info nameofexecutable` and `info script` differ by
//!   construction, and `info globals` / `info commands` / `info procs` differ
//!   because tclsh has `auto_path` and a library of auto-loaded procedures and
//!   tclrs has neither. Every program below that asks one of those questions
//!   asks it with a pattern that selects only the script's own names, which is
//!   the part both implementations are answering the same question about.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

const PROGRAMS: &[&str] = &[
    // ── after: the handle and the registry ──
    "puts [after 100000 {puts never}]",
    "puts [after 100000 {puts never}]\nputs [after 100000 {puts never}]",
    // The list is newest first, as Tcl pushes onto the front of it.
    "after 100000 a\nafter 100000 b\nputs [after info]",
    "set id [after 100000 {puts x}]\nputs [after info $id]",
    "set id [after idle {puts x}]\nputs [after info $id]",
    // Several script words concatenate the way `concat` concatenates them.
    "set id [after 100000 puts hello]\nputs [after info $id]",
    // ── after cancel ──
    "set id [after 100000 {puts x}]\nafter cancel $id\nputs \"<[after info]>\"",
    "after 100000 {puts x}\nafter cancel {puts x}\nputs \"<[after info]>\"",
    // Cancelling something that was never registered is not an error.
    "puts \"<[after cancel nosuch]>\"",
    "after 100000 a\nafter cancel after#99\nputs [after info]",
    // ── after ms script, driven by update ──
    "after 0 {puts fired}\nupdate\nputs done",
    "after 0 {puts one}\nafter 0 {puts two}\nupdate\nputs done",
    "after idle {puts idle}\nupdate\nputs done",
    // The event queue is serviced before the idle handlers, so a timer
    // registered *after* an idle handler still runs first.
    "after idle {puts idle1}\nafter 0 {puts timer}\nafter idle {puts idle2}\nupdate\nputs done",
    // `update idletasks` runs the idle handlers and leaves the timers alone.
    "after idle {puts idle}\nafter 100000 {puts never}\nupdate idletasks\nputs [after info]",
    // An `after` script runs at the global level whatever registered it.
    "set ::v 0\nproc p {} {after 0 {set ::v 1}}\np\nupdate\nputs $::v",
    // A script that fails does not stop the run: the failure goes to stderr and
    // the next handler still fires.
    "after 0 {error boom}\nafter 0 {puts survived}\nupdate\nputs done",
    // An `after` script may register another one, and `update` drains that too.
    "after 0 {after 0 {puts second}\nputs first}\nupdate\nputs done",
    // Nothing pending is not an error.
    "update\nputs done",
    "update idletasks\nputs done",
    // ── vwait ──
    "set ::done 0\nafter 0 {set ::done 1}\nvwait ::done\nputs $::done",
    "set ::n 0\nafter 0 {set ::n 1}\nafter 0 {set ::n 2}\nvwait ::n\nputs $::n",
    // A variable already at the value it will be set to still ends the wait when
    // some *other* write changes it first.
    "set ::s a\nafter 0 {set ::s b}\nvwait ::s\nputs $::s",
    // `vwait` with no argument is `update` in Tcl 9.
    "after 0 {puts fired}\nvwait\nputs done",
    // ── uplevel ──
    "proc p {} {uplevel #0 {set g 42}}\np\nputs $g",
    "proc p {} {uplevel #0 set g 42}\np\nputs $g",
    "proc p {} {uplevel {set q 9}}\np\nputs $q",
    "proc p {} {uplevel 1 {set q 9}}\np\nputs $q",
    "uplevel 0 {set t 3}\nputs $t",
    "uplevel #0 {set t 4}\nputs $t",
    "proc p {} {return [uplevel #0 {expr {1+1}}]}\nputs [p]",
    // The script sees what the caller's level already had.
    "set g 7\nproc p {} {uplevel #0 {incr g}}\np\np\nputs $g",
    // ── upvar #0 ──
    "proc p {} {upvar #0 g l\nset l 7}\np\nputs $g",
    "set g 1\nproc p {} {upvar #0 g l\nreturn $l}\nputs [p]",
    "set g 1\nproc p {} {upvar #0 g l\nincr l\nreturn $l}\nputs [p]\nputs $g",
    "proc p {} {upvar #0 a x b y\nset x 1\nset y 2}\np\nputs \"$a$b\"",
    "set g 1\nproc p {} {upvar #0 g l\nunset l\nreturn [info exists l]}\nputs [p]\nputs [info exists g]",
    "proc p {} {upvar #0 g l\nlappend l a b\nreturn $l}\nputs [p]\nputs $g",
    // ── upvar at a level that is a procedure activation ──
    // The default level is 1, which is the caller's frame. Every one of these
    // used to be a refusal in `unreachable_scopes_are_refused` below; the
    // slot-name table `src/cmd_scope.rs` publishes is what makes them answers.
    "proc inner {} {upvar x y\nset y 99}\nproc outer {} {set x 1\ninner\nputs $x}\nouter",
    "proc inner {} {upvar 1 x y\nreturn $y}\nproc outer {} {set x 7\nputs [inner]}\nouter",
    "proc i {} {upvar #1 v c\nreturn $c}\nproc o {} {set v top\nputs [i]}\no",
    // Two frames up, and two levels reached from one body.
    "proc l3 {} {upvar 1 v a\nupvar 2 v b\nreturn \"$a$b\"}\nproc l2 {} {set v B\nreturn [l3]}\nproc l1 {} {set v A\nputs [l2]}\nl1",
    // A write through the link reaches the caller's own local.
    "proc i {} {upvar 1 q z\nincr z 41}\nproc o {} {set q 1\ni\nreturn $q}\nputs [o]",
    // The level itself computed.
    "proc c3 {} {set n 2\nupvar $n w u\nreturn $u}\nproc c2 {} {return [c3]}\nproc c1 {} {set w W\nputs [c2]}\nc1",
    // `upvar 0` is the procedure's own frame, so the two names are one variable.
    "proc p {} {set x 0\nupvar 0 x y\nset y 9\nreturn $x}\nputs [p]",
    // ── upvar with a computed name ──
    // `upvar #0 $win data`, which is how tk.tcl addresses per-widget state.
    "proc p {w} {upvar #0 $w data\nset data hello}\nset win foo\np $win\nputs [set ::foo]",
    "proc p {n} {upvar $n l\nset l 1}\np ::zz\nputs $zz",
    // A qualified name resolves in the interpreter's variables whatever the
    // level, which is `TclObjLookupVar`'s rule and not a special case here.
    "proc p {n} {upvar 1 $n v\nreturn $v}\nset ::qq 4\nputs [p ::qq]",
    // ── upvar to an array element ──
    // `upvar ::tk::FocusGrab($index) data`, `library/tk.tcl:145`.
    "proc ae {i} {upvar #0 arr($i) e\nlappend e p\nappend e q\nreturn $e}\nputs [ae k]\nputs [array get arr]",
    "proc ae {i} {upvar #0 arr($i) e\nincr e 3\nreturn $e}\nputs [ae n]\nputs [array get arr]",
    "proc ae {} {upvar #0 arr(z) e\nset e 1\nunset e\nreturn [info exists e]}\nputs [ae]\nputs [info exists arr(z)]",
    "proc ae {i} {upvar 1 a($i) e\nreturn $e}\nproc caller {} {set a(k) v\nreturn [ae k]}\nputs [caller]",
    // An element of a *caller's* local array, written through.
    "proc w {i} {upvar 1 a($i) e\nset e W}\nproc caller {} {set a(k) v\nw k\nreturn [array get a]}\nputs [caller]",
    // ── upvar outside a procedure ──
    // Two globals made one variable, which is what `uplevel #0 [list upvar #0
    // ::tk::Priv.$disp ::tk::Priv]` does at `library/tk.tcl:257`.
    "upvar #0 a b\nset a 3\nputs $b",
    "upvar #0 a b\nset b 4\nputs $a",
    // The pair survives the chunk that made it, which is what `uplevel #0 [list
    // upvar #0 …]` at `library/tk.tcl:257` relies on: the `upvar` runs in a chunk
    // of its own and every later script sees one variable.
    "uplevel #0 {upvar #0 src alias}\nuplevel #0 {set src 5}\nputs $alias\nuplevel #0 {set alias 6}\nuplevel #0 {puts [set src]}",
    // ── uplevel into a procedure activation ──
    "proc inner {} {uplevel 1 {set x 1}}\nproc outer {} {set x 0\ninner\nputs $x}\nouter",
    "proc inner {} {uplevel #1 {set x 5}}\nproc outer {} {set x 0\ninner\nputs $x}\nouter",
    "proc p {} {set x 0\nuplevel 0 {set x 9}\nputs $x}\np",
    "proc inner {} {uplevel 1 {incr n 10}}\nproc outer {} {set n 1\ninner\nreturn $n}\nputs [outer]",
    // The uplevel'd script sees the frame's locals and not a global of the same
    // name, and the global is left as it was.
    "set v global\nproc inner {} {uplevel 1 {set r $v}}\nproc outer {} {set v local\nset r {}\ninner\nreturn $r}\nputs [outer]\nputs $v",
    // ── an array element as a command's variable ──
    "lappend a(i) x y\nputs $a(i)",
    "append b(j) hi there\nputs $b(j)",
    "incr c(k) 5\nputs $c(k)",
    "set a(i) x\nunset a(i)\nputs [info exists a(i)]",
    "lassign {1 2} d(p) d(q)\nputs \"$d(p),$d(q)\"",
    "set e(l) {1 2 3}\nlset e(l) 1 X\nputs $e(l)",
    "set e(l) {1 2 3}\nputs [lpop e(l)]\nputs $e(l)",
    "set e(l) {1 2 3}\nledit e(l) 0 0 Z\nputs $e(l)",
    "foreach f(x) {1 2} {puts $f(x)}",
    "puts [lmap g(y) {1 2} {expr {$g(y) * 3}}]",
    // The same inside a procedure, where the array is a frame slot.
    "proc p {} {lappend a(i) x\nappend a(i) !\nreturn [array get a]}\nputs [p]",
    // A run-time link is a frame entry from the declaration on, so `info vars`
    // lists it and `info locals` does not — which is what tclsh answers.
    "proc p {} {upvar 1 x y\nset l 1\nreturn [lsort [info vars]]}\nproc o {} {set x 1\nputs [p]}\no",
    "proc p {} {upvar 1 x y\nset l 1\nreturn [lsort [info locals]]}\nproc o {} {set x 1\nputs [p]}\no",
    "proc p {} {upvar 1 nope y\nreturn [list [info vars] [info exists y]]}\nproc o {} {set nope 1\nputs [p]}\no",
    "proc p {} {set a(i) {1 2}\nlset a(i) 0 Z\nreturn $a(i)}\nputs [p]",
    // ── variable ──
    "variable v 7\nputs $v",
    "variable a 1 b 2\nputs \"$a$b\"",
    "proc p {} {variable v\nset v 3}\np\nputs $v",
    "set v 5\nproc p {} {variable v\nreturn $v}\nputs [p]",
    "proc p {} {variable v 9\nreturn $v}\nputs [p]\nputs $v",
    // `variable name` with no value leaves the variable unset.
    "variable q\nputs [info exists q]",
    "puts \"<[variable]>\"",
    // ── apply ──
    "puts [apply {{x} {expr {$x*2}}} 21]",
    "puts [apply {{a b} {expr {$a+$b}}} 1 2]",
    "puts [apply {{} {return hi}}]",
    "puts [apply {args {llength $args}} 1 2 3]",
    "puts [apply {{a {b 9}} {list $a $b}} 1]",
    "puts [apply {{a {b 9}} {list $a $b}} 1 2]",
    "puts [apply {{a args} {list $a $args}} 1 2 3]",
    // The lambda's locals are its own: an outer variable of the same name is
    // untouched.
    "set x outer\nputs [apply {{x} {set x inner}} 1]\nputs $x",
    // A lambda that loops, so the body is compiled as a real sub rather than
    // substituted in place.
    "puts [apply {{n} {set s 0\nfor {set i 1} {$i <= $n} {incr i} {incr s $i}\nreturn $s}} 10]",
    // A lambda may be applied more than once, and recursion through a procedure
    // still works around it.
    "proc twice {v} {return [apply {{x} {expr {$x*2}}} $v]}\nputs [twice 3]\nputs [twice 5]",
    // The third element of a lambda names the namespace, and `::` is the one
    // this frontend has.
    "puts [apply {{x} {expr {$x+1}} ::} 41]",
    // ── info: constants and the version ──
    "puts [info tclversion]",
    // `info patchlevel` is deliberately NOT compared: it is the release each
    // interpreter IS, not behaviour they can agree on. tclrs answers with the
    // Tcl it is written against (9.0.4), and a reference from any other 9.0.x
    // answers with its own — a difference that says nothing about the port.
    // `tests/version_pin.rs` pins what tclrs reports.
    // ── info exists ──
    "set y 1\nputs [info exists y]",
    "puts [info exists nope]",
    "set y 1\nunset y\nputs [info exists y]",
    "set a(1) x\nputs [info exists a(1)]\nputs [info exists a(2)]\nputs [info exists a]",
    "proc p {} {set l 1\nlist [info exists l] [info exists nope]}\nputs [p]",
    "set g 1\nproc p {} {global g\nreturn [info exists g]}\nputs [p]",
    "proc p {} {set a(1) x\nlist [info exists a(1)] [info exists a(2)]}\nputs [p]",
    // ── info complete ──
    "puts [info complete {set x 1}]",
    "puts [info complete \"set x \\{1\"]",
    "puts [info complete \"puts \\\"a\"]",
    "puts [info complete \"set x \\[a\"]",
    "puts [info complete \"\\}\"]",
    "puts [info complete {set x 1;}]",
    "puts [info complete {}]",
    "set s {if {1} {}}\nputs [info complete $s]",
    "set s \"if \\{1\\} \\{\"\nputs [info complete $s]",
    // ── info args / body / default ──
    "proc p {a {b 2} args} {return x}\nputs [info args p]",
    "proc p {a} {return $a}\nputs [info body p]",
    "proc p {a {b 2}} {}\nputs [info default p b v],$v",
    "proc p {a {b 2}} {}\nputs [info default p a v],<$v>",
    // ── info level ──
    "puts [info level]",
    "proc p {} {return [info level]}\nputs [p]",
    "proc p {} {return [q]}\nproc q {} {return [info level]}\nputs [p]",
    "proc p {} {uplevel #0 {puts [info level]}}\np",
    // ── info procs / commands / globals / vars / locals ──
    // The pattern selects only the script's own names: tclsh also has the
    // procedures its script library auto-loads (`auto_execok`, `unknown`, …)
    // and tclrs has no script library at all, so a bare `info procs` compares
    // two different questions. Same reason for the `info commands` and
    // `info globals` programs below.
    "proc zalpha {} {}\nproc zbeta {} {}\nputs [lsort [info procs z*]]",
    "proc zalpha {} {}\nproc zbeta {} {}\nputs [lsort [info procs za*]]",
    "proc zalpha {} {}\nputs \"<[info procs nosuch*]>\"",
    "puts [lsort [info commands lrev*]]",
    "puts [lsort [info commands foreach]]",
    "set zeta 1\nset zebra 2\nputs [lsort [info globals ze*]]",
    "set zeta 1\nunset zeta\nputs \"<[info globals zeta]>\"",
    "set zeta 1\nputs [lsort [info vars zeta]]",
    "proc p {q} {set zloc 5\nreturn [lsort [info locals z*]]}\nputs [p 1]",
    "proc p {q} {return [lsort [info locals]]}\nputs [p 1]",
    "proc p {} {set zloc 5\nreturn [lsort [info vars zl*]]}\nputs [p]",
    // A `global` name is not a local.
    "set zg 1\nproc p {} {global zg\nreturn [lsort [info locals]]}\nputs [p]",
    // `info vars` inside a procedure answers about the *frame*: its locals and
    // the names bound into it, and not every global the interpreter holds.
    "set zg 1\nproc p {} {set l 1\nreturn [lsort [info vars]]}\nputs [p]",
    "set zg 1\nproc p {} {global zg\nset l 1\nreturn [lsort [info vars]]}\nputs [p]",
    "set zg 1\nproc p {} {upvar #0 zg alias\nset l 1\nreturn [lsort [info vars]]}\nputs [p]",
    "set zg 1\nproc p {} {variable zg\nset l 1\nreturn [lsort [info vars]]}\nputs [p]",
    // A local that has not been assigned yet is not listed.
    "proc p {} {set seen [lsort [info vars]]\nset later 1\nreturn $seen}\nputs \"<[p]>\"",
    // A declaration binds a *link* into the frame, so the name is listed even
    // when the variable it links to is unset — while `info exists` on it is 0.
    "proc p {} {global zunset\nset l 1\nreturn [lsort [info vars]]}\nputs [p]",
    "proc p {} {global zunset\nreturn [info exists zunset]}\nputs [p]",
    "proc p {} {upvar #0 zunset al\nreturn [list [info vars] [info exists al]]}\nputs [p]",
    "proc p {} {variable zunset\nreturn [info vars]}\nputs [p]",
    "proc p {} {global zunset\nreturn [lsort [info locals]]}\nputs \"<[p]>\"",
    // ── the commands' own results ──
    "puts \"<[after 0 {}]>\"" ,
    "puts \"<[update]>\"",
    "proc p {} {upvar #0 g l}\nputs \"<[p]>\"",
];

fn tclsh() -> Option<PathBuf> {
    for name in ["tclsh9.0", "tclsh", "tclsh8.6"] {
        let Ok(out) = Command::new("sh")
            .arg("-c")
            .arg(format!("command -v {name}"))
            .output()
        else {
            continue;
        };
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if path.is_empty() {
            continue;
        }
        // Only the exact release this port is written against is an oracle.
        // tclrs targets 9.0.4 (`src/cmd_info.rs`'s `TCL_PATCHLEVEL`), and a
        // reference from any other release reports ITS version's differences
        // as tclrs failures: 8.6 words errors differently ("couldn't compile
        // regular expression" for "cannot compile") and has a different
        // ensemble membership, while 9.0.3 predates the lseq fixes (a zero
        // step yields the empty list where the manual says it yields `count`
        // elements, and a bareword argument is still an expr). The ubuntu CI
        // image ships 8.6, so CI skips these and they run against a matching
        // tclsh locally.
        let Ok(v) = Command::new("sh")
            .arg("-c")
            .arg(format!("printf 'puts [info patchlevel]\\n' | {path}"))
            .output()
        else {
            continue;
        };
        if String::from_utf8_lossy(&v.stdout).trim() == "9.0.4" {
            return Some(PathBuf::from(path));
        }
    }
    None
}

static SEQ: AtomicUsize = AtomicUsize::new(0);

fn reference_output(tclsh: &PathBuf, program: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "tclrs-event-{}-{}.tcl",
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
fn events_and_scopes_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
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

/// The messages, which are the reference interpreter's wherever the command
/// exists in both.
#[test]
fn event_and_scope_errors_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    // Each program prints the return code and then the message, so the
    // comparison covers both halves of what `catch` saw.
    for src in [
        "puts [catch {after} e]\nputs $e",
        "puts [catch {after foo} e]\nputs $e",
        "puts [catch {after info bogus} e]\nputs $e",
        "puts [catch {after info a b} e]\nputs $e",
        "puts [catch {after cancel} e]\nputs $e",
        "puts [catch {after idle} e]\nputs $e",
        "puts [catch {update bogus} e]\nputs $e",
        "puts [catch {update a b} e]\nputs $e",
        "puts [catch {uplevel} e]\nputs $e",
        "puts [catch {uplevel 5 {set x 1}} e]\nputs $e",
        "puts [catch {uplevel #9 {set x 1}} e]\nputs $e",
        "puts [catch {uplevel -1 {set x 1}} e]\nputs $e",
        "puts [catch {uplevel 1 {set x 1}} e]\nputs $e",
        "puts [catch {info exists} e]\nputs $e",
        "puts [catch {info bogusbogus} e]\nputs $e",
        "proc p {} {}\nputs [catch {info default p nosucharg v} e]\nputs $e",
        "puts [catch {info body nosuchproc} e]\nputs $e",
        "puts [catch {info args nosuchproc} e]\nputs $e",
        "puts [catch {apply {{a b}}} e]\nputs $e",
        "puts [catch {apply notalambda} e]\nputs $e",
    ] {
        let expected = reference_output(&tclsh, src);
        let outcome = tclrs::eval(src).unwrap_or_else(|e| panic!("{src:?} should run: {e}"));
        assert_eq!(outcome.output, expected, "diverged on {src:?}");
    }
}

/// `vwait` on a variable nothing will write answers rather than hanging.
///
/// This is a deliberate divergence, and the *reference implementation* is the
/// one that does not do what its own source says: `Tcl_VwaitObjCmd` reports
/// `can't wait for variable(s)/channel(s): would wait forever` when
/// `Tcl_DoOneEvent` answers 0 (`generic/tclEvent.c:1755-1763`), and on macOS
/// that never happens — the CFRunLoop blocks with no timeout, so tclsh 9.0.4
/// hangs on `vwait neverset` until it is killed (measured: `kill -9` after five
/// seconds, twice, with stdin both a terminal and `/dev/null`). The message is
/// Tcl's own; what differs is that it is reachable here.
///
/// A default build only: with `--features tk` there *is* a notifier, and
/// whether one of its sources could still become ready is a question only it can
/// answer, so `src/cmd_after.rs` blocks in `Tcl_DoOneEvent` and inherits
/// tclsh's answer — including the hang. Asserting the refusal there would be
/// asserting that the notifier is absent.
#[cfg(not(feature = "tk"))]
#[test]
fn a_wait_that_could_never_end_is_reported_rather_than_entered() {
    let started = Instant::now();
    let err = tclrs::eval("vwait neverset").expect_err("the wait cannot be entered");
    assert!(
        err.contains("can't wait for variable(s)/channel(s): would wait forever"),
        "unexpected message: {err}"
    );
    assert!(
        started.elapsed().as_secs() < 5,
        "the wait was entered rather than refused"
    );

    // The same when the only pending work cannot write the variable: the timer
    // runs, and then there is nothing left that could end the wait.
    let err =
        tclrs::eval("after 0 {set ::other 1}\nvwait neverset").expect_err("the wait cannot end");
    assert!(
        err.contains("would wait forever"),
        "unexpected message: {err}"
    );
}

/// What this frontend refuses, and the reason it gives.
///
/// **Seven entries left here when the slot-name table landed, and three more when
/// `uplevel` and `apply` were merged with the published line's**, and each is
/// recorded with what it answers now rather than dropped, because a refusal that
/// quietly became an answer is the one thing a refusal test cannot notice on its
/// own:
///
/// * `uplevel 1`, `uplevel #1` and `uplevel 0` into a procedure activation, all
///   three `"uplevel" to level 1 is not supported` — now answers, through
///   `cmd_scope::in_frame_context`.
/// * `upvar 1 x y` (`"upvar 1" is not supported`) and `upvar x y` (`"upvar" with
///   no level is not supported`) — now answers, through the run-time link.
/// * `upvar #0 a b` outside a procedure (`"upvar" outside a procedure is not
///   supported`) — now makes the two globals one variable, which is
///   `runtime::alias_global`.
/// * `upvar #0 $n l` (`variable name must be a literal in this phase`) — now
///   answers; a computed name is what the run-time link exists for.
///
/// * `uplevel 1 {set neverused 1}` (`the procedure running there never names it`)
///   — now answers, and so does `upvar 1 neverused z`. A frame grows the name a
///   slot when the script asks for one, `cmd_scope::runtime_slot_alloc`.
/// * `apply $f 1` (`"apply" of a computed lambda is not supported`) — now
///   answers; `apply` runs the lambda as a synthesised procedure, so its text
///   need not be in the chunk.
/// * `info library` (`info library is not supported yet`) — now raises tclsh's
///   own `no library has been specified for Tcl`.
///
/// All eleven are byte-compared against tclsh in `PROGRAMS` above or in
/// `frame_differential.rs`. What stands here is what is still out of reach for a
/// reason no slot can supply.
#[test]
fn unreachable_scopes_are_refused() {
    for (src, expected) in [
        // `upvar 1 neverused z` and `upvar 1 arr(k) e` into a caller that names
        // neither stood here — the target frame had no slot to address, because
        // no op in the already-built body could have addressed one. A frame can
        // grow a slot at run time now, so both answer, and answer what tclsh
        // answers. Byte-compared against tclsh in `frame_differential.rs`.
        //
        // Level 0 is the script's own, and *that* is name-addressed, so a level
        // past the end of the stack is the ordinary bad-level report rather than
        // this frontend's own refusal.
        //
        // A `return` in the projected script no longer stands here either: it is
        // a return code leaving the chunk, and the level it belongs to absorbs
        // it. Byte-compared against tclsh below.
        (
            "proc o {} {i}\nproc i {} {upvar 3 x y}\no",
            "bad level \"3\"",
        ),
        // An `upvar` outside a procedure is an alias between two entries of the
        // interpreter's variable table, and one element of an array is not an
        // entry of it.
        (
            "upvar #0 arr(k) e",
            "outside a procedure to the array element",
        ),
        // A local name carrying `::` is refused by `ObjMakeUpvar` itself
        // (`generic/tclVar.c:4544-4558`): the link would outlive the frame it
        // points into.
        (
            "proc p {n} {upvar 1 $n ::q}",
            "can't create namespace variable that refers to procedure variable",
        ),
        // A lambda that is a value used to stand here, refused because its body
        // would have been compiled as a chunk of its own. `apply` runs a lambda as
        // the procedure it is — `runtime::apply_op` synthesises `proc` and calls
        // it — so a computed lambda is now the same answer tclsh gives, compared
        // against it in `PROGRAMS` above.
        //
        // The namespace is what is left: a lambda's third element names the
        // namespace its body runs in, and this frontend has one.
        (
            "puts [apply {{x} {expr {$x}} ::ns} 1]",
            "the namespace \"::ns\" of a lambda is not supported yet",
        ),
        // Computed as well as written out, since neither can reach a second
        // namespace.
        (
            "set L {{x} {expr {$x}} ::ns}\nputs [apply $L 1]",
            "the namespace \"::ns\" of a lambda is not supported yet",
        ),
        // A coroutine cannot suspend out of a script run in another frame: the
        // script is a machine of its own, and `yield` would have to unwind the
        // one that started it.
        (
            "proc g {} {uplevel 1 {yield a}}\ncoroutine c g\nputs [c]",
            "yield inside a script run by \"eval\", \"uplevel\" or \"apply\" is not supported",
        ),
        (
            "proc p {} {apply {{} {yield q}}}\ncoroutine c p\nputs [c]",
            "yield inside a script run by \"eval\", \"uplevel\" or \"apply\" is not supported",
        ),
        // The `info` subcommands that need machinery this frontend has none of.
        ("puts [info frame]", "info frame is not supported yet"),
        (
            "puts [info level 1]",
            "\"info level\" with a level number is not supported",
        ),
        // `info library` compiles and runs, then raises tclsh's own message for an
        // interpreter whose `tcl_library` is gone — which this one permanently is,
        // because nothing here reads an `init.tcl`. A raise rather than a
        // refusal, so a `catch` around it behaves as it does under tclsh.
        (
            "puts [info library]",
            "no library has been specified for Tcl",
        ),
        ("puts [info loaded]", "info loaded is not supported yet"),
        // `vwait` on more than one variable needs the `-all` machinery.
        (
            "vwait a b",
            "\"vwait\" takes at most one variable name in this phase",
        ),
    ] {
        let err = tclrs::eval(src).expect_err(&format!("{src:?} should fail"));
        assert!(
            err.contains(expected),
            "{src:?}: expected an error mentioning {expected:?}, got {err:?}"
        );
    }

    // A `return` inside a script `uplevel` projected into a caller's frame
    // returns from *that* frame's procedure, two levels of chunk down. tclsh
    // 9.0.3 prints `x`.
    let outcome = tclrs::eval("proc o {} {i}\nproc i {} {return [uplevel 1 {return x}]}\nputs [o]")
        .expect("the return reaches the level it belongs to");
    assert_eq!(outcome.output, "x\n");
}

/// `after ms` with no script blocks, which no byte-for-byte comparison can
/// measure. Asserted as a bound rather than a value: the sleep is at least the
/// delay, and the command answers with the empty string.
#[test]
fn a_bare_after_blocks_for_at_least_the_delay() {
    let started = Instant::now();
    let outcome = tclrs::eval("after 40\nputs done").expect("after blocks and returns");
    let elapsed = started.elapsed();
    assert_eq!(outcome.output, "done\n");
    assert!(
        elapsed.as_millis() >= 40,
        "after 40 returned after {elapsed:?}, which is less than the delay"
    );
    // A negative delay is clamped to zero rather than refused
    // (`generic/tclTimer.c:837-839`), so it must not block at all.
    let started = Instant::now();
    tclrs::eval("after -100\nputs done").expect("a negative delay is zero");
    assert!(
        started.elapsed().as_millis() < 1000,
        "a negative delay blocked"
    );
}

/// `vwait` blocks until the timer fires, rather than spinning or returning
/// early. The value is compared against tclsh above; what is measured here is
/// that the wait really waited.
#[test]
fn vwait_waits_for_the_timer_it_is_waiting_on() {
    let started = Instant::now();
    let outcome = tclrs::eval("set ::d 0\nafter 60 {set ::d 1}\nvwait ::d\nputs $::d")
        .expect("the wait ends when the timer fires");
    assert_eq!(outcome.output, "1\n");
    assert!(
        started.elapsed().as_millis() >= 60,
        "vwait returned before its timer was due"
    );
}

/// The `after` registry belongs to the interpreter, not to the process, so two
/// interpreters number their handles independently — which is what makes a
/// handle comparable against a fresh tclsh at all.
#[test]
fn the_after_registry_is_per_interpreter() {
    let first = tclrs::eval("puts [after 100000 {}]").expect("registers");
    let second = tclrs::eval("puts [after 100000 {}]").expect("registers");
    assert_eq!(first.output, "after#0\n");
    assert_eq!(
        second.output, first.output,
        "a second interpreter continued the first one's numbering"
    );

    // And within one interpreter the numbering does continue.
    let both = tclrs::eval("puts [after 100000 {}]\nputs [after 100000 {}]").expect("registers");
    assert_eq!(both.output, "after#0\nafter#1\n");
}
