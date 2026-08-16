//! Differential execution of procedures and the remaining control flow:
//! `proc`, `return`, `global`, `for`, `switch`, `catch` and `error`.
//!
//! Same contract as `execution_differential.rs` — no expected value is written
//! by hand. Every program below is run by tclsh and by tclrs and the two
//! stdouts are compared byte for byte, so an argument-defaulting rule, a
//! `switch` fall-through, a glob class or the quoting of a variadic `args`
//! list is checked against the reference implementation rather than against a
//! reading of the manual page.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const PROGRAMS: &[&str] = &[
    // ── proc: the shapes of a definition ──
    "proc f {} {puts hi}\nf",
    "proc f {} {}\nputs \"<[f]>\"",
    "proc f {x} {puts $x}\nf hello",
    "proc f {a b} {puts [expr {$a+$b}]}\nf 2 3",
    "proc f {a b c d e} {puts \"$a$b$c$d$e\"}\nf 1 2 3 4 5",
    // The body's last command is the value when no return runs.
    "proc f {} {set q 3\nexpr {$q*2}}\nputs [f]",
    "proc f {x} {expr {$x*$x}}\nputs [f 9]",
    // ── proc: local scope ──
    "set v outer\nproc f {} {set v inner\nreturn $v}\nputs [f]\nputs $v",
    "proc f {} {set a 1\nset b 2\nexpr {$a+$b}}\nputs [f]\nputs [f]",
    "set n 5\nproc f {n} {return [expr {$n*2}]}\nputs [f 7]\nputs $n",
    // ── global ──
    "set g 100\nproc f {} {global g\nreturn $g}\nputs [f]",
    "set g 1\nproc f {} {global g\nset g 2\nset l 3\nreturn $l}\nputs [f]\nputs $g",
    "set a 1\nset b 2\nproc f {} {global a b\nreturn [expr {$a+$b}]}\nputs [f]",
    "set g 0\nproc bump {} {global g\nincr g}\nbump\nbump\nbump\nputs $g",
    // `global` has no effect outside a procedure body.
    "global x\nset x 4\nputs $x",
    // ── proc: defaults ──
    "proc f {a {b B}} {puts \"$a-$b\"}\nf 1\nf 1 2",
    "proc f {a {b B} {c C}} {puts \"$a-$b-$c\"}\nf 1\nf 1 2\nf 1 2 3",
    "proc f {{a 7}} {puts $a}\nf\nf 9",
    "proc f {{a 1} b} {puts \"$a$b\"}\nf 8 9",
    // A default is the literal text of the specifier's second field.
    "proc f {{a {x y}}} {puts \"<$a>\"}\nf",
    "proc f {{a {}}} {puts \"<$a>\"}\nf",
    // ── proc: variadic args ──
    "proc f args {puts \"<$args>\"}\nf\nf 1\nf 1 2 3",
    "proc f {a args} {puts \"$a|$args\"}\nf 1\nf 1 2\nf 1 2 3",
    "proc f {a {b 2} args} {puts \"a=$a b=$b args=<$args>\"}\nf 1\nf 1 9\nf 1 9 x y",
    // The collected list is quoted as the `list` command would quote it.
    "proc f args {puts \"<$args>\"}\nf {x y} z",
    "proc f args {puts \"<$args>\"}\nf {} a",
    "proc f args {puts \"<$args>\"}\nf \"a b\" c",
    "proc f args {puts \"<$args>\"}\nf a #",
    "proc f args {puts \"<$args>\"}\nf # a",
    "proc f args {puts \"<$args>\"}\nf {a{b}c}",
    "proc f args {puts \"<$args>\"}\nf a\\{b",
    "proc f args {puts \"<$args>\"}\nf \"a\\\\b\"",
    "proc f args {puts \"<$args>\"}\nf {[foo]} {$x} {a;b}",
    "proc f args {puts \"<$args>\"}\nf {a\"b} {a]b}",
    "proc f args {puts \"<$args>\"}\nf \"x\\ty\" \"p\\nq\"",
    // ── proc: recursion ──
    "proc fact {n} {if {$n <= 1} {return 1}\nreturn [expr {$n * [fact [expr {$n-1}]]}]}\nputs [fact 10]",
    "proc fact {n} {if {$n <= 1} {return 1}\nexpr {$n * [fact [expr {$n-1}]]}}\nputs [fact 20]",
    "proc fib {n} {if {$n < 2} {return $n}\nreturn [expr {[fib [expr {$n-1}]] + [fib [expr {$n-2}]]}]}\nputs [fib 20]",
    "proc gcd {a b} {if {$b == 0} {return $a}\nreturn [gcd $b [expr {$a % $b}]]}\nputs [gcd 1071 462]",
    "proc down {n} {if {$n == 0} {return done}\nputs $n\nreturn [down [expr {$n-1}]]}\nputs [down 3]",
    // A recursive call must not disturb the caller's locals.
    "proc walk {n} {set here $n\nif {$n > 0} {walk [expr {$n-1}]}\nreturn $here}\nputs [walk 4]",
    // ── proc: nested and forward calls ──
    "proc inner {} {return 42}\nproc outer {} {return [inner]}\nputs [outer]",
    "proc outer {} {return [inner]}\nproc inner {} {return 42}\nputs [outer]",
    "proc add {a b} {expr {$a+$b}}\nproc triple {x} {add $x [add $x $x]}\nputs [triple 5]",
    "proc even {n} {if {$n == 0} {return 1}\nreturn [odd [expr {$n-1}]]}\nproc odd {n} {if {$n == 0} {return 0}\nreturn [even [expr {$n-1}]]}\nputs [even 10]\nputs [even 7]",
    // ── proc: away from the top level ──
    // The definition is an event at run time, so what these check is *when* the
    // name starts answering. Everything a top-level definition supports has to
    // survive the move: defaults, a variadic tail, recursion, a forward call
    // from a procedure compiled above the definition, and the body's own locals.
    "if {1} {proc f {} {return hit}}\nputs A[f]",
    "puts [proc f {} {return hit}]|\nputs B[f]",
    "if {0} {proc f {} {}}\nputs [catch {f} m]\nputs $m",
    "proc f {} {return one}\nif {1} {proc f {} {return two}}\nputs [f]",
    "set x 2\nif {$x == 1} {proc f {} {return a}} else {proc f {} {return b}}\nputs [f]",
    "if {1} {proc f {a {b B} args} {puts \"$a-$b-<$args>\"}}\nf 1\nf 1 2\nf 1 2 3 4",
    "if {1} {proc fact {n} {if {$n<2} {return 1}\nexpr {$n*[fact [expr {$n-1}]]}}}\nputs [fact 10]",
    "proc caller {} {return [helper 3]}\nif {1} {proc helper {n} {expr {$n*2}}}\nputs [caller]",
    "set v outer\nif {1} {proc f {} {set v inner\nreturn $v}}\nputs [f]\nputs $v",
    "set g 5\nif {1} {proc f {} {global g\nincr g}}\nf\nf\nputs $g",
    "while {1} {proc f {} {return loop}\nbreak}\nputs [f]",
    "for {set i 0} {$i < 3} {incr i} {proc f {} {return v}\nputs [f]}",
    "foreach x {1 2} {proc f {} {return e}\nputs [f]}",
    // A procedure defined by another procedure's body, which is the shape
    // `Tk_Init`'s last statement is made of.
    "proc outer {} {proc inner {} {return in}\nreturn out}\nputs [catch {inner} m]\nputs $m\nputs [outer]\nputs [inner]",
    "if {1} {proc f {} {error boom}}\nputs [catch {f} m]\nputs $m",
    "if {1} {proc f {} {return early}}\nputs [f]",
    // A body that will not parse is still a definition, and the failure waits
    // for the call — the same rule a top-level definition follows.
    "if {0} {proc f {} {puts \"unterminated}}\nputs done",
    // The `args` tail is collected and quoted at run time here rather than by
    // the call site, so the canonical list quoting is worth checking again on
    // this path: a braced element, an empty one, an embedded space, a tab and a
    // newline, and a double that has to reach its Tcl string form.
    "if {1} {proc f args {puts \"<$args>\"}}\nf {x y} z\nf {} a\nf \"a b\" c\nf {a{b}c}",
    "if {1} {proc f args {puts \"<$args>\"}}\nf {[foo]} {$x} {a;b}\nf \"x\\ty\" \"p\\nq\"\nf 1.50 [expr {3.0/2}]",
    // A coroutine's body calling a procedure a conditional `proc` defined, and
    // a conditional `proc` reached from inside a coroutine defining one the
    // main context then calls. Both run on a VM of their own over the same
    // chunk, which is what the run-time table's entry points are indexed
    // against.
    "proc gen {n} {for {set i 0} {$i < $n} {incr i} {yield [helper $i]}\nreturn done}\nif {1} {proc helper {x} {return v$x}}\ncoroutine c gen 3\nputs [c]\nputs [c]\nputs [c]",
    "proc mk {} {proc made {x} {return m$x}\nreturn ok}\ncoroutine c mk\nputs [made 7]",
    // ── return ──
    "proc f {} {return}\nputs \"<[f]>\"",
    "proc f {} {return 7\nputs unreached}\nputs [f]",
    "proc f {} {return -code ok 9}\nputs [f]",
    "proc f {} {return -code 0 9}\nputs [f]",
    "proc f {x} {if {$x} {return yes}\nreturn no}\nputs [f 1]\nputs [f 0]",
    "proc f {} {while {1} {return early}}\nputs [f]",
    "proc f {} {for {set i 0} {$i < 10} {incr i} {if {$i == 3} {return $i}}\nreturn -1}\nputs [f]",
    "proc f {} {return -code error \"bad thing\"}\nputs [catch {f} m]\nputs $m",
    "proc f {} {return -code error}\nputs [catch {f} m]\nputs \"<$m>\"",
    // ── for ──
    "for {set i 0} {$i < 3} {incr i} {puts $i}",
    "puts \"<[for {set i 0} {$i < 3} {incr i} {}]>\"",
    "for {} {0} {} {puts never}\nputs done",
    "for {set i 0} {$i < 5} {incr i} {if {$i == 2} {continue}\nputs c$i}",
    "for {set i 0} {$i < 5} {incr i} {if {$i == 2} {break}\nputs b$i}",
    "set n 0\nfor {set i 0; set j 10} {$i < $j} {incr i; incr j -1} {incr n}\nputs $n",
    "for {set i 0} {$i < 4} {incr i; if {$i == 3} {break}} {puts s$i}",
    "set s 0\nfor {set i 1} {$i <= 10} {incr i} {for {set j 1} {$j <= 10} {incr j} {if {$j > $i} break\nincr s}}\nputs $s",
    "set t 0\nfor {set i 1} {$i <= 100} {incr i} {set t [expr {$t+$i}]}\nputs $t",
    "proc sum {n} {set t 0\nfor {set i 1} {$i <= $n} {incr i} {set t [expr {$t+$i}]}\nreturn $t}\nputs [sum 50]",
    // ── switch: the two syntaxes ──
    "set x b\nswitch $x {a {puts A} b {puts B} default {puts D}}",
    "set x b\nswitch $x a {puts A} b {puts B} default {puts D}",
    "switch zz {a {puts A} default {puts D}}",
    "puts \"<[switch zz {a {puts A}}]>\"",
    "puts \"<[switch a {a {expr 1+1}}]>\"",
    "switch -exact -- b {a {puts eA} b {puts eB}}",
    "switch -- -glob {-glob {puts dashdash}}",
    // Exactly two arguments: a leading dash is the subject, not an option.
    "puts \"<[switch -glob {-glob {puts two}}]>\"",
    "set x -glob\nswitch $x {-glob {puts sub}}",
    // `default` is only the catch-all as the final pattern.
    "switch d {default {puts D1} d {puts D2}}",
    "switch default {default {puts literaldefault}}",
    "puts \"<[switch zz {default {puts D1} q {puts Q}}]>\"",
    // Fall-through bodies.
    "switch -glob aaab {a*b - b {puts G1} a* {puts G2} default {puts G3}}",
    "set foo abc\nswitch abc a - b {puts one} $foo {puts two} default {puts three}",
    "switch xyz {\n    a -\n    b {\n        # comment\n        puts AB\n    }\n    c {\n        puts C\n    }\n    default {\n        puts DEF\n    }\n}",
    "switch a {a - b - c {puts shared} default {puts other}}",
    "switch c {a - b - c {puts shared} default {puts other}}",
    // ── switch -glob ──
    "switch -glob abc {a?c {puts q}}",
    "switch -glob abc {a*  {puts star}}",
    "switch -glob abc {*c {puts tail}}",
    "switch -glob abc {*b* {puts mid}}",
    "switch -glob abc {ABC {puts upper} default {puts nomatch}}",
    "switch -glob abc {{a[bx]c} {puts class}}",
    "switch -glob abc {{a[^b]c} {puts caret} default {puts nocaret}}",
    "switch -glob abc {{a[a-z]c} {puts range} default {puts norange}}",
    "switch -glob \"a\\nc\" {a?c {puts nl}}",
    "switch -glob {a*b} {a\\*b {puts escaped} default {puts plain}}",
    "switch -glob {} {* {puts star} default {puts none}}",
    "switch -glob abcdef {a**f {puts twostars}}",
    "switch -glob x {{[wxy]} {puts inset} default {puts notinset}}",
    "switch -glob z {{[wxy]} {puts inset} default {puts notinset}}",
    // ── switch: value and side effects ──
    "puts [switch b {a {expr 1} b {expr 2} default {expr 3}}]",
    "set r [switch q {a {expr 1} default {expr 9}}]\nputs $r",
    "proc classify {n} {switch $n {0 {return zero} 1 {return one} default {return many}}}\nputs [classify 0]\nputs [classify 1]\nputs [classify 5]",
    "for {set i 0} {$i < 4} {incr i} {switch $i {0 {puts a} 1 {puts b} default {puts z}}}",
    "for {set i 0} {$i < 4} {incr i} {switch $i {2 {continue} 3 {break}}\nputs i$i}",
    // ── catch ──
    "puts [catch {expr {1/0}} m]\nputs $m",
    "puts [catch {error boom} m]\nputs $m",
    "puts [catch {set x 1} m]\nputs $m",
    "puts [catch {puts hi} m]\nputs <$m>",
    "puts [catch {error a}]",
    "puts \"<[catch {expr {1/0}}]>\"",
    "proc f {} {error inner}\nputs [catch {f} m]\nputs $m",
    "puts [catch {catch {error deep} inner} m]\nputs \"inner=$inner m=$m\"",
    "proc deep {n} {if {$n == 0} {error bottom}\nreturn [deep [expr {$n-1}]]}\nputs [catch {deep 5} m]\nputs $m",
    "proc guard {x} {if {[catch {expr {1/$x}} r]} {return \"err:$r\"}\nreturn $r}\nputs [guard 2]\nputs [guard 0]",
    "set acc {}\nfor {set i 0} {$i < 5} {incr i} {if {[catch {expr {10/(2-$i)}} r]} {set r X}\nset acc \"$acc $r\"}\nputs $acc",
    "if {[catch {error nope} m]} {puts \"caught $m\"} else {puts fine}",
    "puts [catch {catch {expr {1/0}} a} b]\nputs \"a=$a b=$b\"",
    "set i 0\nwhile {$i < 3} {puts [catch {expr {1/(1-$i)}} r]\nincr i}",
    // An error raised inside a loop inside a procedure unwinds to the caller.
    "proc f {} {for {set i 0} {$i < 5} {incr i} {if {$i == 2} {error \"stopped at $i\"}}\nreturn ok}\nputs [catch {f} m]\nputs $m",
    // Unwinding out of a procedure several calls deep.
    "proc deep {n} {if {$n == 0} {return [expr {1/0}]}\nreturn [deep [expr {$n-1}]]}\nputs [catch {deep 3} m]\nputs $m",
    // A `catch` partway through a word must restore the stack exactly, so
    // what the word already built survives.
    "puts \"[catch {expr {1/0}} m]-[catch {error x} n]-$m-$n\"",
    "puts \"A[catch {expr {1/0}}]B[catch {puts -nonewline C}]D\"",
    "set z \"pre[catch {error e1} q]post$q\"\nputs $z",
    "puts \"[catch {catch {catch {error deepest} a} b} c] $a $b $c\"",
    "proc nest {} {return [catch {expr {1/0}} e]:$e}\nputs [nest]",
    "set t 0\nfor {set i 0} {$i < 3} {incr i} {set t [expr {$t + [catch {expr {$i/0}}]}]}\nputs $t",
    // The numeric hook's errors are catchable too, and it leaves its operands
    // on the stack where the extension ops do not.
    "puts [catch {expr {\"a\"*2}}]\nputs [catch {expr {2*2}}]",
    // A procedure result feeding another procedure's variadic list.
    "proc echo args {return $args}\nputs \"<[echo]>\"\nputs \"<[echo 1]>\"\nputs \"<[echo [echo a b] c]>\"",
    "proc pick {n} {return [switch $n {1 {expr 10} 2 {expr 20} default {expr 0}}]}\nputs \"[pick 1] [pick 2] [pick 9]\"",
    // ── error ──
    "puts [catch {error {a b}} m]\nputs $m",
    "puts [catch {error {}} m]\nputs \"<$m>\"",
    "set what disk\nputs [catch {error \"no $what\"} m]\nputs $m",
    // The `errorInfo` and `errorCode` arguments. What they *set* is two options
    // this frontend does not carry, so only the message and the arity are
    // compared here — asking the options dictionary for either key fails, which
    // is the gap BUGS.md records rather than a wrong answer.
    "puts [catch {error boom myinfo} m]\nputs $m",
    "puts [catch {error boom myinfo mycode} m]\nputs $m",
    "puts [catch {error boom {} mycode} m]\nputs $m",
    "puts [catch {error a b c d} m]\nputs $m",
    "puts [catch {error} m]\nputs $m",
    // All three words are substituted, and in the order written: a command
    // substitution in the second or the third has run before the error leaves.
    "proc n {x} {puts \"ran $x\"\nreturn $x}\nputs [catch {error [n one] [n two] [n three]} m]\nputs $m",
    // The shape that made this reachable without anyone writing it: a
    // comparison script is called with the two elements appended.
    "puts [catch {lsort -command {error boom} {a b}} m]\nputs $m",
    // ── the pieces together ──
    "proc fizz {n} {\n    for {set i 1} {$i <= $n} {incr i} {\n        switch [expr {$i % 15}] {\n            0 {puts FizzBuzz}\n            3 - 6 - 9 - 12 {puts Fizz}\n            5 - 10 {puts Buzz}\n            default {puts $i}\n        }\n    }\n}\nfizz 20",
    "proc safeDiv {a b} {\n    if {[catch {expr {$a/$b}} r]} {\n        return NaN\n    }\n    return $r\n}\nfor {set i -2} {$i <= 2} {incr i} {puts [safeDiv 10 $i]}",
    "proc collatz {n} {\n    set steps 0\n    while {$n != 1} {\n        if {$n % 2 == 0} {set n [expr {$n/2}]} else {set n [expr {3*$n+1}]}\n        incr steps\n    }\n    return $steps\n}\nfor {set i 1} {$i <= 10} {incr i} {puts \"$i [collatz $i]\"}",
    "proc ack {m n} {\n    if {$m == 0} {return [expr {$n+1}]}\n    if {$n == 0} {return [ack [expr {$m-1}] 1]}\n    return [ack [expr {$m-1}] [ack $m [expr {$n-1}]]]\n}\nputs [ack 2 3]",
    // ── return codes ──
    // Every one of these is a *code* leaving a command rather than a value:
    // what `catch` reports, what a loop absorbs, and what a level spends.
    "puts [catch {break} m]:<$m>",
    "puts [catch {continue} m]:<$m>",
    "puts [catch {return 7} m]:<$m>",
    "puts [catch {return -code break} m]:<$m>",
    "puts [catch {return -code 42 hi} m]:<$m>",
    "puts [catch {return -level 0 -code error zap} m]:<$m>",
    "catch {break} m o\nputs $o",
    // `catch {error boom} m o` is *not* here: tclsh's options dictionary for
    // an error also carries `-errorstack`, `-errorcode`, `-errorinfo` and
    // `-errorline`, none of which this frontend models. Its two options are
    // asserted by `the_options_variable_carries_the_code_and_level` below,
    // and the gap is recorded in BUGS.md.
    "catch {return -code break} m o\nputs $o",
    "catch {expr {1+1}} m o\nputs $m/$o",
    // A procedure that returns a code makes a control structure of itself.
    "proc stop {} {return -code break}\nset n 0\nwhile {1} {incr n\nstop}\nputs $n",
    "proc skip {} {return -code continue}\nset n 0\nfor {set i 0} {$i < 5} {incr i} {if {$i == 2} {skip}\nincr n}\nputs $n",
    // A code raised in a script another level ran reaches that level.
    "set n 0\nwhile {1} {incr n\nif {$n > 3} {eval {break}}}\nputs $n",
    "set n 0\nfor {set i 0} {$i < 5} {incr i} {if {$i == 2} {eval {continue}}\nincr n}\nputs $n",
    // A `catch` inside the loop takes the code first, so the loop does not.
    "set n 0\nwhile {1} {incr n\nputs [catch {break} c]\nif {$n > 2} break}\nputs $n",
    // `return` inside a `catch` is code 2 to that `catch`, and the procedure
    // carries on.
    "proc q {} {catch {return 7} c\nreturn c=$c}\nputs [q]",
    "proc r {} {set v [catch {return -code break} c o]\nreturn $v/$c/$o}\nputs [r]",
    // A `-level` past one keeps travelling.
    "proc a {} {b}\nproc b {} {return -level 2 -code break}\nset n 0\nwhile {1} {incr n\na}\nputs $n",
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
        "tclrs-proc-{}-{}.tcl",
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
fn procedures_and_control_flow_match_tclsh() {
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

/// A procedure call is a value like any other, so a script's value can come
/// out of one.
#[test]
fn script_value_can_come_from_a_procedure() {
    assert_eq!(tclrs::eval("proc f {} {return 5}\nf").unwrap().result, "5");
    assert_eq!(tclrs::eval("proc f {} {}\nf").unwrap().result, "");
    // `proc` itself evaluates to the empty string.
    assert_eq!(tclrs::eval("proc f {} {}").unwrap().result, "");
    assert_eq!(
        tclrs::eval("proc f {a} {expr {$a*2}}\nf 21")
            .unwrap()
            .result,
        "42"
    );
    assert_eq!(
        tclrs::eval("for {set i 0} {$i<3} {incr i} {}")
            .unwrap()
            .result,
        ""
    );
    assert_eq!(
        tclrs::eval("switch a {a {expr 1} b {expr 2}}")
            .unwrap()
            .result,
        "1"
    );
    assert_eq!(tclrs::eval("catch {error x}").unwrap().result, "1");
    assert_eq!(tclrs::eval("catch {set y 1}").unwrap().result, "0");
}

/// An error nobody catches reaches the caller of `eval`, and one that a
/// `catch` traps does not.
#[test]
fn an_uncaught_error_escapes_and_a_caught_one_does_not() {
    let err = tclrs::eval("proc f {} {error boom}\nf").expect_err("should fail");
    assert!(err.contains("boom"), "got {err:?}");

    let outcome = tclrs::eval("proc f {} {error boom}\nputs [catch {f} m]\nputs $m").unwrap();
    assert_eq!(outcome.output, "1\nboom\n");

    // The VM keeps running after a trapped error.
    let outcome = tclrs::eval("catch {expr {1/0}}\nputs still-here").unwrap();
    assert_eq!(outcome.output, "still-here\n");
}

/// Constructs whose Tcl semantics this frontend does not model are refused at
/// compile time. Silently doing something close would be worse than failing.
#[test]
fn unsupported_procedure_constructs_are_refused() {
    for (src, expected) in [
        // A `proc` away from the top level is no longer refused — see
        // `a_proc_away_from_the_top_level_binds_its_name_when_it_runs` below,
        // which pins what it does instead. What a *conditional* definition
        // still may not do is take a built-in command's name, because the
        // built-in lowering would go on winning at every call site: the same
        // refusal the top-level form gets, from the same place.
        (
            "if {1} {proc set {a b} {}}",
            "redefining the built-in command \"set\"",
        ),
        (
            "puts [proc while {a b} {}]",
            "redefining the built-in command \"while\"",
        ),
        ("proc f {} {}\nproc f {} {}", "procedure \"f\" is redefined"),
        (
            "proc set {a b} {}",
            "redefining the built-in command \"set\"",
        ),
        ("proc f {a a} {}", "defined twice"),
        (
            "proc f {{a b c}} {}",
            "too many fields in argument specifier",
        ),
        ("proc f {} {}\nf 1", "wrong # args: should be \"f\""),
        ("proc f {a b} {}\nf 1", "wrong # args: should be \"f a b\""),
        (
            "proc f {a {b 1}} {}\nf",
            "wrong # args: should be \"f a ?b?\"",
        ),
        (
            "proc f {a args} {}\nf",
            "wrong # args: should be \"f a ?arg ...?\"",
        ),
        // Every option `switch` names is implemented now, so what is left here
        // is a genuinely unknown one — and the two `-matchvar`/`-indexvar`
        // shapes that are still refused, which are the ones that need
        // `-regexp` and did not say it.
        ("switch -bogus a b {a {}}", "bad option \"-bogus\""),
        (
            "switch -matchvar m -glob a {a {}}",
            "-matchvar option requires -regexp option",
        ),
        (
            "switch -indexvar i -exact a {a {}}",
            "-indexvar option requires -regexp option",
        ),
        ("switch a {a}", "extra switch pattern with no body"),
        ("switch a {}", "wrong # args"),
        ("switch a", "wrong # args"),
        ("switch a {a -}", "no body specified for pattern \"a\""),
    ] {
        let err = tclrs::eval(src).expect_err(&format!("{src:?} should fail"));
        assert!(
            err.contains(expected),
            "{src:?}: expected an error mentioning {expected:?}, got {err:?}"
        );
    }

    // `-regexp`, and then `-matchvar`/`-indexvar`, were on that list until each
    // landed. Pinned rather than dropped, so the day one starts refusing again
    // this test says so; what they *match* and *report* is compared against
    // tclsh by `tests/regexp_differential.rs`.
    for (src, expected) in [
        (
            "puts [switch -regexp abc {^a {list one} default {list none}}]",
            "one\n",
        ),
        (
            "switch -matchvar m -regexp abc {{a(.)c} {puts $m}}",
            "abc b\n",
        ),
        (
            "switch -indexvar i -regexp abc {{a(.)c} {puts $i}}",
            "{0 2} {1 1}\n",
        ),
    ] {
        assert_eq!(
            tclrs::eval(src)
                .unwrap_or_else(|e| panic!("{src} is implemented: {e}"))
                .output,
            expected,
            "{src}"
        );
    }
}

/// The run-time table is the fallback, never the rule: a name the compiler can
/// resolve keeps its direct `Op::Call`.
///
/// This is the half of the run-time-`proc` work that a differential run against
/// tclsh cannot see, because both engines give the same answer either way. What
/// it costs is the thing to pin: an `Op::Extended` in a loop body is what stops
/// fusevm's tracing tier taking it (`is_trace_op_allowed_at` rejects
/// `Op::Extended`), so a call that quietly became dynamic would show up as a
/// benchmark regression and nowhere else. `tclrs --tiers
/// bench/counted_loop_proc.tcl` is the measurement; this is the same check on
/// every `cargo test`.
#[test]
fn a_name_the_compiler_resolves_keeps_its_direct_call() {
    use tclrs::compiler::ext;
    let dynamic = |src: &str| {
        let chunk = tclrs::runtime::compile(src).expect("lowers");
        let calls = chunk
            .ops
            .iter()
            .filter(|op| matches!(op, fusevm::Op::Call(_, _)))
            .count();
        let dyn_calls = chunk
            .ops
            .iter()
            .filter(|op| matches!(op, fusevm::Op::Extended(ext::DYN_CALL, _)))
            .count();
        let defines = chunk
            .ops
            .iter()
            .filter(|op| matches!(op, fusevm::Op::Extended(ext::PROC_DEFINE, _)))
            .count();
        (calls, dyn_calls, defines)
    };

    // The benchmark whose trace eligibility the tiers report measures: one
    // direct call, no run-time lookup, and the one registration every `proc`
    // now makes.
    //
    // The registration count moved from 0 to 1 when procedures became callable
    // across chunks: a chunk's own address book answers only inside that chunk,
    // so a `proc` at a script's top level binds its name in the interpreter's
    // run-time table as well, which is what lets a `source`d file, an `eval` or
    // a Tk binding script call it. The op runs once, where the definition
    // stands — never on a call path, which is what this test is really pinning.
    // The `--tiers` report still says `traced=true` and `reaches native code
    // true` for this file, because the loop body is unchanged: `Op::Call` is
    // still the call, and the loop still contains no `Op::Extended`.
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/bench/counted_loop_proc.tcl"
    ))
    .expect("read the benchmark");
    assert_eq!(dynamic(&src), (1, 0, 1), "the benchmark's lowering moved");

    // A script whose procedures are all top-level pays nothing on a call: three
    // direct calls and no lookup. One registration per definition is what makes
    // two names reachable from another chunk — one per definition, not one per call.
    assert_eq!(
        dynamic("proc a {} {return 1}\nproc b {} {return [a]}\nputs [b][a]"),
        (3, 0, 2)
    );

    // One conditional definition makes that name — and only that name —
    // dynamic. `a` keeps its two direct calls; `b` has two lookups, one of them
    // written above the definition. Both definitions register, as every `proc`
    // does now.
    assert_eq!(
        dynamic("proc a {} {return 1}\nputs [b]\nif {1} {proc b {} {return [a]}}\nputs [b][a]"),
        (2, 2, 2)
    );

    // A top-level definition that some conditional one also claims registers
    // itself in the table as well, because every call to the name is a lookup
    // now and the table has to be able to answer with this body too.
    assert_eq!(
        dynamic("proc f {} {return one}\nif {1} {proc f {} {return two}}\nputs [f]"),
        (0, 1, 2)
    );
}

/// `proc` away from a script's top level: the command exists once the defining
/// code has **run**, and not before.
///
/// This is the assertion the two entries above used to make, repointed at what
/// tclsh 9.0.4 actually does. The old refusal existed because this compiler
/// resolves a command name while compiling, so a procedure defined in a branch
/// that is never taken would have answered anyway; the run-time command table
/// is what removed the reason for it, and this is the behaviour that replaced
/// it. Every expectation below was measured against
/// `/usr/local/bin/tclsh 9.0.4` first, and `PROGRAMS` compares the same shapes
/// against it on every run.
#[test]
fn a_proc_away_from_the_top_level_binds_its_name_when_it_runs() {
    // A taken branch defines it.
    assert_eq!(
        tclrs::eval("if {1} {proc f {} {return hit}}\nputs A[f]")
            .expect("the definition ran")
            .output,
        "Ahit\n"
    );
    // An untaken one does not, and the call site says so at run time — the
    // wording and the exit status tclsh gives, not a compile-time verdict on
    // the script.
    let err = tclrs::eval("if {0} {proc f {} {return hit}}\nputs C[f]")
        .expect_err("the definition never ran");
    assert!(err.contains("invalid command name \"f\""), "got {err:?}");
    // So a `catch` around it traps, which is what makes the failure a run-time
    // one rather than a refusal to compile.
    assert_eq!(
        tclrs::eval("puts [catch {if {0} {proc f {} {}}; f} m]\nputs $m")
            .expect("the script itself is fine")
            .output,
        "1\ninvalid command name \"f\"\n"
    );
    // The definition is a command like any other: it runs inside a command
    // substitution, and evaluates to the empty string there too.
    assert_eq!(
        tclrs::eval("puts [proc f {} {return hit}]|\nputs B[f]")
            .expect("the definition ran")
            .output,
        "|\nBhit\n"
    );
    // The later definition wins, because it is the one that ran last.
    assert_eq!(
        tclrs::eval("proc f {} {return one}\nif {1} {proc f {} {return two}}\nputs [f]")
            .expect("the second definition ran")
            .output,
        "two\n"
    );
    // The signature travels with the body: the argument count is checked when
    // the call runs, in the usage wording tclsh reports.
    let err = tclrs::eval("if {1} {proc f {a b} {}}\nf 1").expect_err("too few arguments");
    assert!(err.contains("wrong # args: should be \"f a b\""), "{err:?}");
}

/// `catch`'s options variable carries the code and the level for every outcome.
///
/// The values are tclsh 9.0.3's, taken from it directly; the byte-for-byte
/// corpus above compares the whole dictionary for the outcomes where tclsh's
/// has nothing else in it. An *error* is the one that does — `-errorstack`,
/// `-errorcode`, `-errorinfo` and `-errorline` are in tclsh's and not in this
/// one — so its two modelled options are asserted here instead of pretending
/// the dictionaries match.
#[test]
fn the_options_variable_carries_the_code_and_level() {
    for (src, expected) in [
        ("catch {expr {1+1}} m o\nputs $o", "-code 0 -level 0\n"),
        ("catch {break} m o\nputs $o", "-code 3 -level 0\n"),
        ("catch {continue} m o\nputs $o", "-code 4 -level 0\n"),
        ("catch {return 7} m o\nputs $o", "-code 0 -level 1\n"),
        (
            "catch {return -code break} m o\nputs $o",
            "-code 3 -level 1\n",
        ),
        (
            "catch {return -code 42 hi} m o\nputs $o",
            "-code 42 -level 1\n",
        ),
        (
            "catch {error boom} m o\nputs \"[dict get $o -code] [dict get $o -level]\"",
            "1 0\n",
        ),
    ] {
        let outcome = tclrs::eval(src).unwrap_or_else(|e| panic!("{src:?} failed: {e}"));
        assert_eq!(outcome.output, expected, "{src:?}");
    }
}
