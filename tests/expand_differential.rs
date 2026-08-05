//! Differential execution of `{*}` argument expansion and of a procedure called
//! from a chunk other than the one that defined it.
//!
//! Same contract as the other differential suites: no expected output is written
//! by hand. Every program is run by tclsh and by tclrs and the two stdouts are
//! compared byte for byte, so what `{*}` does to an empty list, to a braced
//! element, to a command's *name* and to a builtin's argument count is taken from
//! the reference implementation rather than from a reading of `Tcl(n)` rule 5.
//!
//! The cases that a reading of the manual page gets wrong, all measured against
//! tclsh 9.0.4 first:
//!
//! * `{*}{}` contributes no arguments, and a command whose words all expand to
//!   nothing runs nothing at all and answers the empty string — `catch {{*}{}}`
//!   is 0, not an "invalid command name" for the empty name.
//! * The command's name may itself be expanded: `{*}{n x} y` calls `n` with `x y`.
//! * A `{*}` word is spliced by *list* rules, so `{*}{a {b c} d}` is three
//!   arguments and the middle one keeps its spaces without its braces.
//! * Expansion applies to a command this frontend compiles as well as to a
//!   procedure: `set {*}{a b}` assigns, `incr {*}{i 5}` increments by five, and
//!   `if {*}{1 {puts yes}}` runs its body.
//! * A word whose text is not a well-formed list fails when the command is
//!   assembled, *after* the earlier words have been substituted: `n [puts before]
//!   {*}{x "y}` prints `before` and then reports `unmatched open quote in list`.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const PROGRAMS: &[&str] = &[
    // ── what a `{*}` word contributes ──
    "proc n {args} {puts \"argc=[llength $args] <$args>\"}\nn {*}{}\nn {*}[list]\nn a {*}{b c} d",
    "proc n {args} {puts \"argc=[llength $args] <$args>\"}\nn {*}{a {b c} d}\nn {*}{{} x}",
    "proc n {args} {puts \"argc=[llength $args] <$args>\"}\nset x {p q}\nn {*}$x\nn {*}\"a b\"\nn {*}[list a b]",
    // Rule 5 needs a non-space follower, so `{*}` alone is the braced word `*`.
    "proc n {args} {puts \"argc=[llength $args] <$args>\"}\nn {*} x\nn x{*}y",
    // Two expansions in one command, and one either side of a plain word.
    "puts [list {*}{a b} {*}{c d}]\nputs [list {*}{a b} c {*}{d e}]",
    // List quoting survives the round trip: an element with a space, a tab, a
    // newline, a brace, a backslash and a `$` all stay one argument each.
    "proc n {args} {puts \"argc=[llength $args] <$args>\"}\nn {*}{{a b} {x\ty} {p\nq}}",
    "proc n {args} {puts \"argc=[llength $args] <$args>\"}\nn {*}{a\\ b {$x} {[foo]} {a;b}}",
    // ── the command's own name ──
    "proc n {args} {puts \"argc=[llength $args] <$args>\"}\nset cmd {n x}\n{*}$cmd y\nset c2 n\n{*}$c2 q",
    // A command whose every word expands to nothing runs nothing.
    "puts [catch {{*}{}} m]|<$m>\nset e {}\nputs [catch {{*}$e} m]|<$m>\nputs done",
    // ── `{*}$args` inside a procedure, which is what tk.tcl is made of ──
    "proc n {args} {puts \"argc=[llength $args] <$args>\"}\nproc outer {args} {n {*}$args}\nouter\nouter 1\nouter 1 2 3\nouter {a b} c",
    "proc fmt {spec args} {return [format $spec {*}$args]}\nputs [fmt %s-%s a b]\nputs [fmt {%d/%d} 3 4]\nputs [fmt plain]",
    "proc chain {args} {return [join [lsort {*}$args] ,]}\nputs [chain {c a b}]\nputs [chain -decreasing {c a b}]",
    // ── expansion into a command the compiler lowers itself ──
    "set {*}{a b}\nputs $a\nset i 0\nincr {*}{i 5}\nputs $i\nputs [expr {*}{1 + 2}]",
    "if {*}{1 {puts yes} else {puts no}}\nif {*}{0 {puts yes} else {puts no}}",
    "puts [llength [list {*}{a b} c]]\nset l {}\nlappend l {*}{x y}\nputs $l\nputs [string {*}{length abcd}]",
    "set s {}\nappend s {*}{a b c}\nputs $s\nputs [join {*}{{a b c} -}]",
    "foreach {*}{v {1 2 3}} {puts $v}",
    "puts [lindex {*}{{a b c} 1}]\nputs [lrange {*}{{a b c d} 1 2}]",
    // ── argument counts, which only run time can check ──
    "proc d {a {b B}} {puts \"$a/$b\"}\nd {*}{1}\nd {*}{1 2}\nputs [catch {d {*}{}} m]|$m\nputs [catch {d {*}{1 2 3}} m]|$m",
    "proc two {a b} {puts \"$a-$b\"}\ntwo {*}{1 2}\nputs [catch {two {*}{1 2 3}} m]|$m",
    "puts [catch {set {*}{a b c}} m]|$m",
    "puts [catch {nosuchcommand {*}{a b}} m]|$m",
    // A word that is not a list fails when the command is assembled, after the
    // words before it have run.
    "proc n {args} {puts ran}\nputs [catch {n [puts before] {*}{x \"y}} m]|$m",
    "puts [catch {n {*}{\"}} m]|$m",
    // ── a procedure called from another chunk ──
    // Each `eval` is a chunk of its own, so every call below crosses a boundary
    // the entry point in the run-time command table cannot be a jump across.
    "proc f {x} {return \"f($x)\"}\neval {puts [f 3]}\nputs [f 4]",
    "proc add {a b} {expr {$a+$b}}\neval {puts [add 2 [add 3 4]]}",
    "proc d {a {b B} args} {return \"$a|$b|<$args>\"}\neval {puts [d 1]}\neval {puts [d 1 2 3 4]}",
    "set g 0\nproc bump {} {global g\nincr g}\neval {bump}\neval {bump}\nputs $g\nputs [bump]",
    "proc fact {n} {if {$n < 2} {return 1}\nexpr {$n * [fact [expr {$n-1}]]}}\neval {puts [fact 10]}",
    "proc walk {n} {set here $n\nif {$n > 0} {walk [expr {$n-1}]}\nreturn $here}\neval {puts [walk 4]}",
    "proc boom {} {error \"from a procedure\"}\neval {puts [catch {boom} m]|$m}",
    // The other direction: the chunk that defines it is the nested one.
    "eval {proc inner {} {return in}}\nputs [inner]\neval {puts [inner]}",
    "eval {proc mk {x} {return m$x}}\nproc use {} {return [mk 7]}\nputs [use]",
    // Mutual recursion *across* the boundary: every step alternates chunks, so
    // each one is a nested run positioned at the other chunk's entry point.
    //
    // Six deep, because a nested run costs native stack the way `eval` does and
    // these tests run on a test harness thread rather than on the binary's own.
    // Measured on a 2 MB stack in a debug build: this chain survives 8 levels and
    // not 12, and a chain of nested `eval`s survives 16 and not 24. The binary
    // runs on `tclrs::runtime::RECOMMENDED_STACK` and stops at the recursion
    // limit instead — `f 100000` there is `too many nested evaluations (infinite
    // loop?)`, which is what tclsh answers too (measured).
    "proc f {n} {return [g $n]}\neval {proc g {n} {if {$n <= 0} {return 0}\nreturn [expr {1 + [f [expr {$n-1}]]}]}}\nputs [f 6]",
    // A `proc` defined by an expanded command, which reaches the definition
    // through the same evaluation the expanded builtins go through.
    "proc {*}{made {x} {return m$x}}\nputs [made 1]\neval {puts [made 2]}",
    // `{*}` and the chunk boundary at once, which is `tk.tcl`'s shape:
    // `[::tk::MessageBox {*}$args]` reached from a script Tk evaluates.
    "proc target {args} {return \"got <$args>\"}\neval {puts [target {*}{a b c}]}",
    "proc relay {args} {return [target {*}$args]}\nproc target {args} {return \"T<$args>\"}\neval {puts [relay 1 2]}",
    // A name taken away by `rename` stops answering, in every chunk.
    "proc f {} {return one}\nrename f g\nputs [g]\nputs [catch {f} m]|$m\neval {puts [catch {f} m]|$m}\neval {puts [g]}",
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
        "tclrs-expand-{}-{}.tcl",
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
fn expansion_and_cross_chunk_calls_match_tclsh() {
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

/// A procedure defined by a `source`d file, and one defined *before* the
/// `source` and called from inside it.
///
/// `source` runs a chunk of its own, so both directions cross a chunk boundary —
/// the case the run-time command table's chunk identity used to turn into
/// `invalid command name`. Compared against tclsh over the same two files.
#[test]
fn a_sourced_file_and_its_caller_share_their_procedures() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    let dir = std::env::temp_dir().join(format!("tclrs-expand-src-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create the directory");
    let library = dir.join("lib.tcl");
    std::fs::write(
        &library,
        "proc fromLibrary {x} {return \"lib($x)\"}\nputs [fromCaller 1]\n",
    )
    .expect("write the library");
    let main = dir.join("main.tcl");
    std::fs::write(
        &main,
        format!(
            "proc fromCaller {{x}} {{return \"caller($x)\"}}\n\
             source {}\n\
             puts [fromLibrary 2]\n\
             puts [fromCaller 3]\n",
            library.display()
        ),
    )
    .expect("write the main script");

    let out = Command::new(&tclsh).arg(&main).output().expect("run tclsh");
    let expected = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "tclsh rejected the pair: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(expected, "caller(1)\nlib(2)\ncaller(3)\n");

    let script = std::fs::read_to_string(&main).expect("read the main script");
    let outcome = tclrs::eval(&script).expect("the pair runs");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(outcome.output, expected);
}

/// What a `{*}` costs the command that has one, and what it costs every other
/// command: nothing.
///
/// The lowering is the point of the op. A command with an expanded word cannot be
/// resolved while the script is read — neither its callee nor its argument count
/// is known — so it becomes one `EXPAND_CALL` over the line, a flag per word and
/// the words themselves. A command *without* one is untouched, which is what keeps
/// `Op::Call` on every call this compiler can resolve.
#[test]
fn only_a_command_with_an_expanded_word_pays_for_one() {
    use tclrs::compiler::ext;
    let count = |src: &str, id: u16| {
        tclrs::runtime::compile(src)
            .expect("lowers")
            .ops
            .iter()
            .filter(|op| matches!(op, fusevm::Op::Extended(op_id, _) if *op_id == id))
            .count()
    };

    // One op for the command, whatever it expands into.
    assert_eq!(count("proc n {args} {}\nn {*}{a b}", ext::EXPAND_CALL), 1);
    assert_eq!(count("puts [list {*}{a b} {*}{c d}]", ext::EXPAND_CALL), 1);
    // A statically resolvable call keeps `Op::Call` and gains nothing.
    let plain = tclrs::runtime::compile("proc n {a} {return $a}\nputs [n 1]").expect("lowers");
    assert_eq!(
        plain
            .ops
            .iter()
            .filter(|op| matches!(op, fusevm::Op::Call(_, _)))
            .count(),
        1
    );
    assert_eq!(
        count("proc n {a} {return $a}\nputs [n 1]", ext::EXPAND_CALL),
        0
    );
    // And the words of an expanded command are still lowered as words: the
    // command substitution inside one is compiled in place, not deferred.
    assert_eq!(
        tclrs::eval("proc n {args} {return <$args>}\nputs [n {*}[list a b] [string toupper c]]")
            .expect("runs")
            .output,
        "<a b C>\n"
    );
}

/// A `{*}` word is not a word a value can be taken from: the expansion is a
/// number of arguments, which only the command assembling them can act on.
///
/// The refusal used to cover every use of `{*}`. It is now the one place that
/// cannot mean anything — a word offered to something that is not a command's
/// argument list — and the wording says so.
#[test]
fn expansion_outside_a_commands_words_is_refused() {
    // `{*}` is only recognised at the start of a command's word, so the parser
    // never records one anywhere else and the refusal is unreachable from Tcl
    // source. Asserting the *reachable* half instead: every position a `{*}` can
    // be written in is a position that now runs.
    for src in [
        "proc n {args} {return <$args>}\nn {*}{a b}",
        "proc n {args} {return <$args>}\nn {*}{a b} c",
        "proc n {args} {return <$args>}\nproc p {} {return [n {*}{a}]}\nputs [p]",
        "set l {a b}\nllength {*}[list $l]",
    ] {
        assert!(
            tclrs::eval(src).is_ok(),
            "{src:?} should run, not be refused"
        );
    }
}
