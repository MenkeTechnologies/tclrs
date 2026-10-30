//! Differential execution for the return-options dictionary: what `catch`
//! hands its options variable, `-errorstack`, `-errorline`, `try`'s `-during`,
//! `info errorstack`, and the first line of `errorInfo`.
//!
//! No expected value is written by hand. Every program is run by tclsh 9.0.4
//! and by tclrs and the two outputs compared byte for byte. The dictionaries
//! are printed whole, so the ORDER of their keys is compared too: tclsh
//! builds one from the options the exception stated, then `-code` and
//! `-level`, then the error's record (`Tcl_GetReturnOptions`), and a script
//! that prints or compares one sees that order.
//!
//! Left out on purpose, because the answer depends on interpreter state
//! tclsh does not reset rather than on the program: an `error` given an
//! `errorInfo` argument, and a `return -errorinfo`, leave tclsh's error stack
//! as the previous error left it (`ERR_ALREADY_LOGGED`). And `-errorstack`'s
//! `INNER` element for a failure inside a bytecode instruction (`loadStk`,
//! `incrStk`) names tclsh's instruction set.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const PROGRAMS: &[&str] = &[
    // ── what `return` states ──
    // Every word but an odd last one is an option, whatever it is spelt, and
    // one `return` gives no meaning to is kept, first, in the dictionary.
    r#"puts [list [catch {return a b c} m o] $m $o]
puts [list [catch {return a b} m o] $m $o]
puts [list [catch {return -code error -x} m o] $m $o]
puts [list [catch {return -options {-code 3 -zz 1} -code 4 r} m o] $m $o]
proc p {} {return -k v r}
puts [list [catch p m o] $m $o]"#,
    r#"catch {return -code error -errorcode X msg} m o; puts $o
catch {return -code break} m o; puts $o
catch {return -x y -code 5 z} m o; puts $o
proc p {} {return -code error -bar 2 -errorcode {E F} hi}
catch p m o; puts $o
proc q {} {return -level 2 -k v r}
proc q2 {} {q; return no}
catch q2 m o; puts $o"#,
    // `error` and `throw` state their options too, in the order
    // `Tcl_ErrorObjCmd` and `Tcl_ThrowObjCmd` write them.
    r#"catch {error msg} m o; puts $o
catch {error msg {} cod} m o; puts $o
catch {throw {A B} m} m o; puts $o
catch {return -code error -level 0 -errorcode X msg} m o; puts $o"#,
    // A completion code or level is read as a C int: any integer spelling,
    // wrapped to 32 bits.
    r#"puts [catch {return -level 0 -code 0x10 x} m]$m
puts [catch {return -level 0 -code " 5 " x} m]$m
puts [catch {return -level 0 -code 0b11 x} m]$m
puts [catch {return -level 0 -code 1_0 x} m]$m
puts [catch {return -level 0 -code 2147483648 x} m]$m
puts [catch {return -level 0 -code -1 x} m]$m
puts [try { return -level 0 -code 0x10 } on 16 {} { list d e f }]
puts [try { return -level 0 -code 7 } on 0o7 {} { list g }]
puts [catch {try {} on 99999999999 {} {}} m]$m"#,
    // ── try ──
    // A failing handler or `finally` carries what it was handling as
    // `-during`; a `try` with handlers outside a procedure is not compiled,
    // so its body is labelled and its lines count from it.
    r#"catch {try { error x } on error {e o} { error y }} m o; puts $o
catch {try { error x } on error {} { error y } finally {error z}} m o; puts $o
catch {try { set a 1 } finally {error z}} m o; puts $o
catch {try { error x } finally {error z}} m o; puts $o
catch {try { error x } on error {} { return -code break y }} m o; puts $o
catch {try { error x } trap {} {} { error y }} m o; puts $o
catch {try { error x } on error {} { error y } finally {set q 1}} m o; puts $o
proc p {} { try { error x } on error {} { error y } }
catch p m o; puts $o"#,
    r#"catch {try {error x} on break {} {}} m o; puts $o
catch {try {error x} finally {}} m o; puts $o
catch {try {return -code error -foo 1 x} finally {}} m o; puts $o
proc p6 {} { try { error boom } on error {} { error b2 } }
catch p6 m o; puts $o"#,
    // `dict update` and `dict with` hand an outcome on as they found it at
    // the top level, and through `returnStk` — merged — in a procedure,
    // where tclsh compiles them.
    r#"set d {a 1}
catch {dict update d a x { error boom }} m o; puts $o
catch {dict with d { error boom2 }} m o; puts $o
proc p {} { set d {a 1}; dict update d a x { error boom } }
catch p m o; puts $o
proc p2 {} { set d {a 1}; dict with d { error boom } }
catch p2 m o; puts $o
proc p3 {} { foreach x 1 { error boom } }
catch p3 m o; puts $o"#,
    // ── -errorline ──
    // A `catch` that names a variable outside a procedure is not compiled,
    // and its body's lines count from the body.
    r#"proc d {} {
  catch {
    set y 1
    error x
  } m o
  return [dict get $o -errorline]
}
puts [d]
catch {
  error top
} m o
puts [dict get $o -errorline]
catch {error x} m o; puts [dict get $o -errorline]
set s "set a 1\nerror q"
catch {eval $s} m o; puts [dict get $o -errorline]
catch {if 1 {
  error w
}} m o; puts [dict get $o -errorline]"#,
    // ── -errorstack ──
    // One element per logged script: `CALL` and the call's substituted words
    // in a procedure, `UP n` under an `uplevel`, nothing at the top level.
    r#"catch {eval {error x}} m o; puts [dict get $o -errorstack]
catch {if 1 {error x}} m o; puts [dict get $o -errorstack]
set d {a 1}
catch {dict update d a x { error boom }} m o; puts [dict get $o -errorstack]
catch {dict with d { error boom }} m o; puts [dict get $o -errorstack]
set b {error boom}
catch {dict with d $b} m o; puts [dict get $o -errorstack]
catch {foreach x 1 {error y}} m o; puts [dict get $o -errorstack]
catch {foreach x 1 $b} m o; puts [dict get $o -errorstack]"#,
    r#"proc p4 {} {eval {error boom}}
catch p4 m o; puts [dict get $o -errorstack]
proc p5 {} {set b {error boom}; eval $b}
catch p5 m o; puts [dict get $o -errorstack]
proc p6 {} {set b {error boom}; foreach x 1 $b}
catch p6 m o; puts [dict get $o -errorstack]
proc p7 {} {uplevel 1 {error boom}}
catch p7 m o; puts [dict get $o -errorstack]
proc p8 {} {p7}
catch p8 m o; puts [dict get $o -errorstack]
catch {uplevel #0 {error boom}} m o; puts [dict get $o -errorstack]
proc p9 {} {if 1 {error boom}}
catch p9 m o; puts [dict get $o -errorstack]
proc p10 {} {catch {error x} m o; return -options $o $m}
catch p10 m o; puts [dict get $o -errorstack]"#,
    r#"proc p {} {catch {eval {error x}} m o; return [dict get $o -errorstack]}
puts [p]
proc q {} {catch {foreach x 1 {error x}} m o; return [dict get $o -errorstack]}
puts [q]
proc r {a} {set b {error x}; eval $b}
set v 5
catch {r $v} m o; puts [dict get $o -errorstack]
proc s {} {uplevel #0 {error x}}
proc s2 {} {s}
catch s2 m o; puts [dict get $o -errorstack]
proc t {} {uplevel 0 {error x}}
catch t m o; puts [dict get $o -errorstack]
catch {apply {{} {eval {error x}}}} m o; puts [dict get $o -errorstack]
namespace eval ns {proc u {} {eval {error x}}}
catch ns::u m o; puts [dict get $o -errorstack]"#,
    r#"proc p4 {} {uplevel 1 {eval {error x}}}
proc p5 {} {p4}
catch p5 m o; puts [dict get $o -errorstack]
proc p6 {} {uplevel 1 {p7}}
proc p7 {} {error x}
proc p8 {} {p6}
catch p8 m o; puts [dict get $o -errorstack]
proc f x {g $x$x}
proc g x {error G:$x}
catch {f 12} m d
puts [dict get $d -errorstack]
proc h {a {b B}} {error x}
set v 5
catch {h $v} m d; puts [dict get $d -errorstack]
catch {h {*}[list 1 2]} m d; puts [dict get $d -errorstack]
proc k {} {h [string repeat a 3]}
catch k m d; puts [dict get $d -errorstack]"#,
    // `info errorstack` is the stack of the error last taken.
    r#"proc f x {g $x$x}
proc g x {error G:$x}
catch {f 12}
set e1 [info errorstack]
catch {f 13}
set e2 [info errorstack]
puts [list $e1 $e2]
try {error x} on error {} {}
puts [info errorstack]
puts [catch {info errorstack a b} m]$m"#,
    // ── the first line of errorInfo ──
    // An operator tclsh folded while compiling — every operand a literal —
    // fails with its record begun ("invoked from within"); one failing on a
    // value read at run time does not ("while executing").
    r#"catch {expr {1/0}}; puts $errorInfo
catch {expr 1/0}; puts $errorInfo
set y 1
catch {expr {$y/0}}; puts $errorInfo
proc a {y} {expr {$y/0}}
catch {a 1}; puts $errorInfo
proc c {} {expr {sqrt(-1)}}
catch c; puts $errorInfo
proc d {} {expr {"a"+1}}
catch d; puts $errorInfo
proc e {} {set x [expr {1/0}]}
catch e; puts $errorInfo
proc f {} {if {1/0} {}}
catch f; puts $errorInfo
catch {if {1/0} {}}; puts $errorInfo"#,
    r#"proc g {} {set a a; expr {$a+1}}
catch g; puts $errorInfo
proc h {} {set a 0; expr {1/$a}}
catch h; puts $errorInfo
proc i {} {set a 0; expr {1/$a + (2/0)}}
catch i; puts $errorInfo
proc j {} {expr {1/[set a 0]}}
catch j; puts $errorInfo
set a 0
catch {expr {1%$a}}; puts $errorInfo
catch {expr {"x" * 2}}; puts $errorInfo
catch {expr {-"x"}}; puts $errorInfo
catch {expr {abs("x")}}; puts $errorInfo
catch {expr {2**-1 / 0}}; puts $errorInfo
catch {expr {int(1/0)}}; puts $errorInfo
catch {set x [expr {1/0}]}; puts $errorInfo
catch {format %d abc}; puts $errorInfo"#,
];

#[test]
fn error_options_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    let mut failures = Vec::new();
    for program in PROGRAMS {
        let expected = reference(&tclsh, program);
        match tclrs::eval(program) {
            Ok(got) if got.output == expected => {}
            Ok(got) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs: {:?}",
                got.output
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

/// The reference interpreter, when it is the release this port targets: see
/// `tests/info_differential.rs` for why no other release is an oracle.
fn tclsh() -> Option<PathBuf> {
    for name in ["tclsh9.0", "tclsh"] {
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

/// What tclsh printed. Every program catches its own errors, so a failure of
/// the program as a whole is a broken test and says so.
fn reference(tclsh: &PathBuf, program: &str) -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "tclrs-erropts-{}-{}.tcl",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path, program).expect("write program");
    let out = Command::new(tclsh).arg(&path).output().expect("run tclsh");
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "tclsh failed the program itself:\n{program}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}
