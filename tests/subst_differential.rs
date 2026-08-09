//! Differential execution for `subst`, `throw` and `lsort -command`.
//!
//! The three commands this file covers have one thing in common: each reaches
//! back into the interpreter from inside a running op. `subst` reads the calling
//! frame's variables and runs the commands its value spells; `lsort -command`
//! calls a comparison command once per compared pair; `throw` is the small one,
//! and is here because it is the third command the same change added.
//!
//! No expected output is written by hand. Every program below is run by tclsh
//! and by tclrs and the two outputs compared byte for byte, so a misreading of
//! `subst`'s token semantics — which failures a command substitution's `catch`
//! range absorbs, what a `break` inside one keeps, where a syntax error stops
//! the substitution *after* the side effects before it — fails here rather than
//! becoming a baked-in bug.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A driver that reports each program's completion code and result the way the
/// probes against tclsh were written, so an error is compared as data rather
/// than aborting the run.
const DRIVER: &str = "proc t {args} { set c [catch {uplevel 1 $args} m]; puts \"[list $c $m]\" }\n";

const PROGRAMS: &[&str] = &[
    // ── subst: the three substitutions ──
    "set b B\nt subst {a$b c}",
    "set b B\nt subst {x[set b]y}",
    "t subst {a\\nb}",
    "t subst {a\\x41b}",
    "set b B\nt subst -nobackslashes {a\\nb}",
    "t subst -nocommands {x[foo]y}",
    "set b B\nt subst -novariables {x$b y}",
    "set b B\nt subst -nobackslashes -nocommands -novariables {a$b[c]\\d}",
    // An option's unambiguous prefix, which `Tcl_GetIndexFromObj` accepts.
    "set b B\nt subst -nob -noc -nov {a$b[c]\\d}",
    "t subst -foo x",
    "t subst",
    "t subst {}",
    // A backslash is text with `-nobackslashes`, and the `[` after it still
    // opens a substitution.
    "set b B\nt subst -nobackslashes {a\\[set b]c}",
    // ── subst: what is *not* special in a value ──
    "t subst {a\"b c;d]e}",
    "set b B\nt subst {${b}}",
    "set b B\nt subst -novariables {${b}}",
    "t subst \"a\\\\\\nb\"",
    // ── subst: variables and array elements ──
    "set a(k) V\nt subst {$a(k)}",
    "set a(k) V\nset b k\nt subst {$a($b)}",
    "set a(k) V\nset b k\nt subst {$a([set b])}",
    "t subst {$nosuch}",
    "t subst -novariables {$nosuch}",
    "set a(k) V\nt subst {$a}",
    "set b B\nt subst {$b(x)}",
    "set a(k) V\nt subst {$a(zz)}",
    // ── subst: the return codes a command substitution can raise ──
    "t subst {[break]abc}",
    "t subst {x[break]abc}",
    "t subst {x[continue]abc}",
    "t subst {x[return -level 0 Q]y}",
    "t subst {x[return Q]y}",
    "t subst {x[error boom]y}",
    "t subst {x[return -code 7 Z]y}",
    // ── subst: a parse failure stops it, but only after what came before ──
    "t subst {[puts hi][}",
    "t subst {a[}",
    "t subst {[}",
    "t subst -nocommands {[}",
    "t subst {$}",
    "t subst -novariables {$}",
    "t subst {a[puts one; puts two}",
    "t subst {a[puts one; puts two; puts three}",
    "t subst {a[puts one;}",
    "t subst {a[puts \"unterminated}",
    "t subst \"a\\$\\{bcd\"",
    "t subst {$x(}",
    "set a(k) V\nt subst {$a(}",
    // A `break` jumps past the syntax error the substitution was going to
    // report, so the command succeeds.
    "t subst {x[break]a[}",
    // ── subst: the calling frame ──
    "proc p {} {set loc 42\nreturn [subst {v=$loc}]}\nt p",
    "proc q {} {set loc 7\nreturn [subst {c=[expr {$loc*2}]}]}\nt q",
    "proc w {} {set loc 1\nsubst {[set loc 9]}\nreturn $loc}\nt w",
    "set ::g 3\nproc pg {} {global g\nreturn [subst {g=$g}]}\nt pg",
    "set ::g 3\nproc pn {} {return [subst {g=$::g}]}\nt pn",
    "namespace eval n {set v 5\nputs [subst {v=$v}]}",
    "namespace eval n {set v 5}\nt subst {v=$::n::v}",
    // ── subst: the value is a value ──
    "set s {a$b c}\nset b B\nt subst $s",
    "set s {[format %s hi]}\nt subst $s",
    // ── throw ──
    "t throw {ARITH DIVZERO} {div by zero}",
    "t throw {} msg",
    "t throw A",
    "t throw A B C",
    "t throw \\{ x",
    "set ty {X Y}\nt throw $ty boom",
    "t catch {throw {A B} oops} m",
    // ── lsort -command ──
    "proc c {a b} {string compare $a $b}\nt lsort -command c {b a c}",
    "proc c {a b} {string compare $a $b}\nt lsort -command c -decreasing {b a c}",
    "proc c {a b} {string compare $a $b}\nt lsort -stride 2 -command c {b 1 a 2}",
    "proc c {a b} {string compare $a $b}\nt lsort -unique -command c {b a b}",
    "proc c {a b} {string compare $a $b}\nt lsort -index 1 -command c {{x 2} {y 1}}",
    "proc c {a b} {string compare $a $b}\nt lsort -command c {}",
    "t lsort -command {apply {{a b} {expr {[string length $a]-[string length $b]}}}} {aaa a aa}",
    "t lsort -command bogus {a b}",
    "t lsort -command",
    // A later mode option replaces `-command`, as it replaces any other mode.
    "proc c {a b} {string compare $b $a}\nt lsort -command c -integer {3 1 2}",
    // The comparison sees the globals, not the caller's locals — it is invoked
    // as a command, so a procedure it names has a frame of its own.
    "set ::gg 5\nproc pg {a b} {return [expr {$::gg ? [string compare $a $b] : 0}]}\nproc pp {} {set loc 1\nreturn [lsort -command pg {b a}]}\nt pp",
    // A global the running chunk assigned but has not flushed is still visible
    // to the comparison command.
    "proc pg {a b} {return [expr {$::flag ? [string compare $a $b] : 0}]}\nset ::flag 1\nt lsort -command pg {b a}",
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

fn reference(tclsh: &PathBuf, program: &str) -> Result<String, String> {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "tclrs-subst-{}-{}.tcl",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path, program).expect("write program");
    let out = Command::new(tclsh).arg(&path).output().expect("run tclsh");
    let _ = std::fs::remove_file(&path);
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[test]
fn subst_throw_and_lsort_command_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("no tclsh on PATH; skipping the differential");
        return;
    };
    let mut failures = Vec::new();
    for case in PROGRAMS {
        let program = format!("{DRIVER}{case}\n");
        let expected = match reference(&tclsh, &program) {
            Ok(out) => out,
            Err(e) => {
                failures.push(format!("tclsh rejected program:\n{program}\n{e}"));
                continue;
            }
        };
        match tclrs::eval(&program) {
            Ok(outcome) if outcome.output == expected => {}
            Ok(outcome) => failures.push(format!(
                "program:\n{program}  tclsh: {expected:?}\n  tclrs: {:?}",
                outcome.output
            )),
            Err(e) => failures.push(format!(
                "program:\n{program}  tclsh: {expected:?}\n  tclrs failed: {e}"
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
