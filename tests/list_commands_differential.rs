//! Differential execution for the list commands that name or build a variable:
//! `lassign`, `lset`, `lpop`, `ledit`, `lrepeat`, `lremove`, `lseq` and `lmap`.
//!
//! Same contract as `list_differential.rs`: every program is run by both tclsh
//! and tclrs and the two outputs are compared byte for byte, so no expectation
//! here is written by hand. That matters especially for these eight, because
//! almost none of them behaves the way the manual reads:
//!
//! * `lset` grows a list by exactly one element at the end and refuses the
//!   index after that, so `lset l 3 X` on three elements appends and
//!   `lset l 4 X` is an error.
//! * `lset l X` with no index at all — and `lset l {} X` — replace the whole
//!   variable rather than doing nothing.
//! * `lseq 1 10 0` yields one element instead of looping forever, and a step
//!   pointing away from the end yields none.
//! * `lseq` decides int-versus-float from the start and step but *not* from a
//!   count: `lseq 3.0` is `0 1 2` while `lseq 1.5 count 3` is `1.5 2.5 3.5`.
//! * `lremove` never errors on an index outside the list.
//! * `lrepeat` refuses a negative count in its own wording, not the integer
//!   parser's.
//! * `lmap` omits an iteration that `continue`d rather than collecting an
//!   empty element for it, and an empty body collects one empty element per
//!   iteration.
//!
//! Each program is complete on its own. They are not concatenated into one
//! file, because this frontend compiles a whole script before running any of
//! it: one program whose *shape* is refused — a wrong argument count — would
//! take the rest of the file down with it, which is a divergence of its own and
//! is recorded in `parity_fuzz_findings.rs` rather than here.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Programs whose output must agree, byte for byte.
const PROGRAMS: &[&str] = &[
    // ── lassign ──────────────────────────────────────────────────────────
    "set l {a b c}; set rem [lassign $l x y z]; puts \"$x $y $z <$rem>\"",
    "set rem [lassign {a b c d e} p q]; puts \"$p $q <$rem>\"",
    // More variables than elements: the extra ones get the empty string.
    "set rem [lassign {a b} m n o w]; puts \"$m $n <$o> <$w> <$rem>\"",
    "set rem [lassign {} aa bb]; puts \"<$aa> <$bb> <$rem>\"",
    // No variables at all is legal, and yields the whole list.
    "puts [lassign {a b c}]",
    "set rem [lassign {{a b} c} v1]; puts \"<$v1> <$rem>\"",
    // The remainder comes back canonically quoted.
    "puts [lassign {a {b c} {} d} v2]",
    "puts [lassign {a b\\ c {d e}} v3]",
    // Inside a procedure the targets are frame slots instead.
    "proc f {} {set r [lassign {1 2 3} a b]; return \"$a $b <$r>\"}\nputs [f]",
    // ── lset ─────────────────────────────────────────────────────────────
    "set l {a b c}; lset l 1 X; puts $l",
    "set l {a b c}; puts [lset l 1 X]",
    "set l {a b c}; lset l end X; puts $l",
    "set l {a b c}; lset l end-1 X; puts $l",
    // Growing: one past the end appends.
    "set l {a b c}; lset l 3 X; puts $l",
    "set l {a b c}; lset l end+1 X; puts $l",
    "set l {}; lset l 0 A; puts $l",
    // No index, and an empty index list, replace the value entirely.
    "set l {a b c}; lset l X; puts $l",
    "set l {a b}; lset l {} Z; puts $l",
    // Index paths, as separate arguments and as one list.
    "set l {{a b} {c d}}; lset l 1 0 X; puts $l",
    "set l {{a b} {c d}}; lset l 0 1 Z; puts $l",
    "set l {{a b} {c d}}; lset l {1 1} Q; puts $l",
    // Descending into a scalar treats it as a one-element list.
    "set l {a b c}; lset l 0 0 X; puts $l",
    "set l {a b}; lset l 1 0 Z; puts $l",
    // A nested list grows by one at its own end.
    "set l {{a b}}; lset l 0 2 Z; puts $l",
    "set l {a b}; lset l 0 \"x y\"; puts $l",
    "proc f {} {set l {a b c}; lset l 1 Q; return $l}\nputs [f]",
    // ── lpop ─────────────────────────────────────────────────────────────
    "set l {a b c}; set v [lpop l]; puts \"$v <$l>\"",
    "set l {a b c}; set v [lpop l 0]; puts \"$v <$l>\"",
    "set l {a b c}; set v [lpop l end]; puts \"$v <$l>\"",
    "set l {{a b} c}; set v [lpop l 0 1]; puts \"$v <$l>\"",
    "set l {a}; set v [lpop l]; puts \"$v <$l>\"",
    "proc f {} {set l {a b c}; set v [lpop l]; return \"$v <$l>\"}\nputs [f]",
    // ── ledit ────────────────────────────────────────────────────────────
    "set l {a b c d}; set r [ledit l 1 2 X]; puts \"<$r> <$l>\"",
    "set l {a b c d}; ledit l 1 2; puts $l",
    // first > last inserts rather than replacing.
    "set l {a b c d}; ledit l 1 0 X Y; puts $l",
    "set l {a b c d}; ledit l 2 1 Q; puts $l",
    "set l {a b c d}; ledit l end end Z; puts $l",
    // Both ends clamp instead of refusing.
    "set l {a b}; ledit l 9 9 Z; puts $l",
    "set l {a b c}; ledit l -1 0 Z; puts $l",
    "set l {a b c}; ledit l end end; puts $l",
    "set l {}; ledit l 0 0 Z; puts $l",
    // ── lrepeat ──────────────────────────────────────────────────────────
    "puts [lrepeat 3 a]",
    "puts [lrepeat 2 a b]",
    "puts <[lrepeat 0 a]>",
    "puts <[lrepeat 3]>",
    "puts [lrepeat 2 \"a b\" {}]",
    "puts [llength [lrepeat 3 a b c]]",
    // ── lremove ──────────────────────────────────────────────────────────
    "puts [lremove {a b c d} 1]",
    "puts [lremove {a b c d} 0 2]",
    // Unordered and repeated indices each remove their element once.
    "puts [lremove {a b c d} 2 0]",
    "puts [lremove {a b c d} 1 1]",
    "puts [lremove {a b c d} end]",
    "puts [lremove {a b c d} end-1]",
    "puts [lremove {a b c}]",
    // An index outside the list is ignored rather than refused.
    "puts [lremove {a b c} 5]",
    "puts [lremove {a b c} -1]",
    // ── lseq ─────────────────────────────────────────────────────────────
    "puts [lseq 5]",
    "puts <[lseq 0]>",
    "puts <[lseq -3]>",
    "puts [lseq 1 5]",
    // The direction is inferred when no step is given.
    "puts [lseq 5 1]",
    "puts [lseq 1 10 2]",
    "puts [lseq 10 1 -1]",
    // A zero step is one element; a step away from the end is none.
    "puts [lseq 1 10 0]",
    "puts <[lseq 1 10 -2]>",
    "puts [lseq 1 2 0.5]",
    "puts [lseq 1 to 5]",
    "puts [lseq 1 to 10 by 3]",
    "puts [lseq 1 .. 10 by 2]",
    "puts [lseq 5 count 3]",
    "puts [lseq 1 count 4 by 2]",
    "puts <[lseq 1 count 0]>",
    "puts [lseq 10 to 1 by -3]",
    "puts [lseq 1 2 by 4]",
    "puts [lseq 1 2 3]",
    // Int or float is decided by the start and the step, never by a count.
    "puts [lseq 0 1 0.25]",
    "puts [lseq 3.0]",
    "puts [lseq 1.5 count 3]",
    "puts [lseq 1 count 3 by 0.5]",
    "puts [lseq 1 count 3.0]",
    "puts [lseq 1 3.0]",
    "puts [lseq 1.0 3]",
    "puts [lseq 1 3 1.0]",
    // ── lmap ─────────────────────────────────────────────────────────────
    "puts [lmap x {1 2 3} {expr {$x * 2}}]",
    // An empty body still collects one empty element per iteration.
    "puts <[lmap x {1 2 3} {}]>",
    "puts <[lmap x {} {expr {1}}]>",
    // Uneven lists: the longest fixes the count, the shorter supplies empty.
    "puts [lmap a {1 2 3} b {x y} {list $a $b}]",
    "puts [lmap {a b} {1 2 3 4} {list $a $b}]",
    "puts [lmap x {1 2} y {3 4} z {5 6} {list $x $y $z}]",
    // `break` returns what was collected; `continue` collects nothing.
    "puts [lmap x {1 2 3 4} {if {$x == 3} break; expr {$x}}]",
    "puts [lmap x {1 2 3 4} {if {$x == 2} continue; expr {$x}}]",
    "puts [lmap x {1 2 3} {if {$x == 2} continue; set x}]",
    // The result is a list, so each element is quoted as one.
    "puts [lmap x {1 2} {list a b}]",
    "puts [lmap x {1 2} {list \"a b\" {}}]",
    "puts [lmap x {{a b} {c d}} {llength $x}]",
    // The loop variable survives the loop, as `foreach`'s does.
    "set r [lmap x {1 2} {set x}]; puts \"<$r> $x\"",
    // Nested, and inside a procedure, where the variables are frame slots.
    "puts [lmap x {1 2} {lmap y {3 4} {expr {$x * $y}}}]",
    "proc f {} {return [lmap i {1 2 3} {expr {$i + 1}}]}\nputs [f]",
    // A recursive procedure: each activation needs its own accumulator.
    "proc f {n} {if {$n <= 0} {return {}}\nreturn [lmap i [list $n] {list $i [f [expr {$n - 1}]]}]}\nputs [f 3]",
];

/// Programs whose *error* must agree, first line for first line.
const ERRORS: &[&str] = &[
    "lassign \"a \\{b\" v",
    "set l {a b c}; lset l 4 X",
    "set l {a b c}; lset l -1 X",
    "set l {}; lset l 1 A",
    "set l {a b}; lset l x Z",
    "set l {a b}; lset l end+2 Z",
    "set l {{a b} c}; lset l 0 5 Z",
    "lset nosuchvar 0 X",
    "lrepeat -1 a",
    "lrepeat x a",
    "lremove {a b c} x",
    "lremove \"\\{a\" 0",
    "set l {}; lpop l",
    "set l {a b}; lpop l 5",
    "set l {a b}; lpop l x",
    "set l {a b}; lpop l -1",
    "lpop nosuchvar2",
    "lseq a",
    "lseq 1 zz 5",
    "lseq 1 2 3 4",
    "lseq 1 2 to 4",
    "lseq 1 to 4 by",
    "lseq 1 zz 4 by 2",
    "lseq 1 to 10 zz 2",
    "lseq 1 2 3 4 5",
    "lseq 1 2 3 4 5 6",
    "lseq",
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

/// Run a program through tclsh, returning its stdout and the first line of any
/// error it reported. tclsh follows an error with a stack trace and tclrs does
/// not, so only the first line is comparable.
fn reference(tclsh: &PathBuf, program: &str) -> (String, Option<String>) {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "tclrs-listcmd-{}-{}.tcl",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path, program).expect("write program");
    let out = Command::new(tclsh).arg(&path).output().expect("run tclsh");
    let _ = std::fs::remove_file(&path);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let error = (!out.status.success()).then(|| {
        String::from_utf8_lossy(&out.stderr)
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_string()
    });
    (stdout, error)
}

#[test]
fn list_commands_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };

    let mut failures = Vec::new();
    for program in PROGRAMS {
        let (expected, error) = reference(&tclsh, program);
        assert!(
            error.is_none(),
            "tclsh rejected a program that should run:\n{program}\n{}",
            error.unwrap_or_default()
        );
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

/// A program tclsh refuses must be refused here too, in the same wording.
#[test]
fn list_command_errors_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };

    let mut failures = Vec::new();
    for program in ERRORS {
        let (_, error) = reference(&tclsh, program);
        let Some(expected) = error else {
            panic!("tclsh accepted a program the test expects it to refuse:\n{program}");
        };
        match tclrs::eval(program) {
            Ok(outcome) => failures.push(format!(
                "program:\n{program}\n  tclsh refused: {expected:?}\n  tclrs ran it: {:?}",
                outcome.output
            )),
            Err(e) if e.to_string().lines().next().unwrap_or_default().trim() == expected => {}
            Err(e) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs: {:?}",
                e.to_string()
            )),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} errors diverge:\n\n{}",
        failures.len(),
        ERRORS.len(),
        failures.join("\n\n")
    );
}
