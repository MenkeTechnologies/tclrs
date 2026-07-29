//! Differential execution for Tcl's list commands.
//!
//! Same contract as `execution_differential.rs`: every program is run by both
//! tclsh and tclrs and the two outputs are compared byte for byte, so no
//! expectation about list quoting, index arithmetic or sort order is written by
//! hand here. That matters more for lists than for arithmetic, because the
//! quoting rules are not the ones a reading of the manual would suggest — an
//! element needing protection only because of `]` or an internal `"` gets
//! backslashes while its braces are left alone — and only the reference
//! implementation settles such questions.
//!
//! `errors_match_tclsh` covers the other half: a program tclsh refuses must
//! fail here too, with the interpreter's own wording.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const PROGRAMS: &[&str] = &[
    // list: the quoting rules, element by element.
    "puts [list]",
    "puts [list a b c]",
    "puts [list {}]",
    "puts [list {} {}]",
    "puts [list a {} b]",
    "puts [list \"a b\"]",
    "puts [list \"a\tb\"]",
    "puts [list \"a\nb\"]",
    "puts [list \"a\\rb\"]",
    "puts [list \"a\\vb\"]",
    "puts [list \"a\\fb\"]",
    "puts [list a\\\\b]",
    "puts [list {a$b}]",
    "puts [list {a;b}]",
    "puts [list {a[b}]",
    "puts [list {a]b}]",
    "puts [list {a\"b}]",
    "puts [list a\\{b]",
    "puts [list a\\}b]",
    "puts [list a{b}c]",
    "puts [list \\{ab]",
    "puts [list {\"ab}]",
    "puts [list #a]",
    "puts [list x #a]",
    "puts [list #]",
    "puts [list \" #a\"]",
    "puts [list \\{]",
    "puts [list \\}]",
    "puts [list ab\\\\]",
    "puts [list a\\\\\\ b]",
    "puts [list [list a b] [list c d]]",
    "puts [list [list [list a b]]]",
    "puts [list {a{b]c}d}]",
    "puts [list 1 2.5 {} x]",
    // llength across the parsing rules.
    "puts [llength {a b c d e}]",
    "puts [llength {a b {c d} e}]",
    "puts [llength {a b { } c d e}]",
    "puts [llength {}]",
    "puts [llength { }]",
    "puts [llength \"  a   b  \"]",
    "puts [llength \"a\\tb\\nc\\rd\\ve\\ff\"]",
    "puts [llength \"a\\\\ b c\"]",
    "puts [llength \"a }b\"]",
    "set v { }\nputs [llength $v]",
    // lindex, including index arithmetic and nesting.
    "puts [lindex {a b c} 0]",
    "puts [lindex {a b c} 2]",
    "puts [lindex {a b c} end]",
    "puts [lindex {a b c} end-1]",
    "puts [lindex {a b c} end+1]",
    "puts <[lindex {a b c} -1]>",
    "puts <[lindex {a b c} 99]>",
    "puts [lindex {a b c}]",
    "puts [lindex {a b c} {}]",
    "puts [lindex {{a b c} {d e f} {g h i}} 2 1]",
    "puts [lindex {{a b c} {d e f} {g h i}} {2 1}]",
    "puts [lindex {{{a b} {c d}} {{e f} {g h}}} 1 1 0]",
    "set idx 1\nputs [lindex {a b c d e f} $idx+2]",
    "puts [lindex {a b c d e} \" 1 \"]",
    "puts [lindex {a b c d e} 0x2]",
    "puts [lindex {a b c d e f g h i j k} 1_0]",
    "puts [lindex {a b c d e} end+-1]",
    "puts [lindex {a b {c d}} 2 0]",
    "puts <[lindex {} 0]>",
    // lappend.
    "lappend fresh a b c\nputs $fresh",
    "set v 1\nlappend v 2\nlappend v 3 4 5\nputs $v",
    "set v \"a  b\"\nlappend v c\nputs $v",
    "set v \"  x  \"\nputs <[lappend v]>",
    "puts <[lappend never]>",
    "set v {}\nlappend v {a b} {}\nputs $v",
    // lrange.
    "puts [lrange {a b c d e} 0 1]",
    "puts [lrange {a b c d e} end-2 end]",
    "puts [lrange {a b c d e} 1 end-1]",
    "puts [lrange {some {elements to} select} 1 1]",
    "puts <[lrange {a b c} 3 1]>",
    "puts [lrange {a b c} -5 99]",
    "puts [lrange \"a  b  c\" 0 end]",
    "puts <[lrange {} 0 end]>",
    // lreverse.
    "puts [lreverse {a a b c}]",
    "puts [lreverse {a b {c d} e f}]",
    "puts <[lreverse {}]>",
    // linsert.
    "puts [linsert {the fox jumps over the dog} 1 quick]",
    "puts [linsert {a b c} end x]",
    "puts [linsert {a b c} end-1 x]",
    "puts [linsert {a b c} 0 x y]",
    "puts [linsert {a b c} -5 x]",
    "puts [linsert {a b c} 99 x]",
    "puts [linsert {a b c} 1]",
    "puts [linsert \"a  b\" 1 x]",
    "puts [linsert {} 0 a]",
    // lreplace.
    "puts [lreplace {a b c d e} 1 1 foo]",
    "puts [lreplace {a b c d e} 1 2 three more elements]",
    "puts [lreplace {a b c d e} end end]",
    "puts [lreplace {a b c d e} 12345 end+2 f g h i]",
    "puts [lreplace {a b c} 2 1 X]",
    "puts [lreplace {a b c} -3 -2 X]",
    "puts [lreplace {a b c} 0 0]",
    "puts [lreplace \"a  b  c\" 1 1]",
    "puts [lreplace {a b c} 0 end]",
    // lsearch.
    "puts [lsearch {a b c d e} c]",
    "puts [lsearch {a20 b35 c47} b*]",
    "puts [lsearch {a b c} z]",
    "puts [lsearch -all {a b c a b c} c]",
    "puts <[lsearch -all {a b c} z]>",
    "puts [lsearch -exact {a b c a b c} c]",
    "puts [lsearch -inline {a20 b35 c47} b*]",
    "puts <[lsearch -inline {a b c} z]>",
    "puts [lsearch -inline -not {a20 b35 c47} b*]",
    "puts [lsearch -all -inline -not {a20 b35 c47} b*]",
    "puts [lsearch -all -not {a20 b35 c47} b*]",
    "puts [lsearch -all -inline -not -exact {a b c a d e a f g a} a]",
    "puts [lsearch -start 3 {a b c a b c} c]",
    "puts [lsearch -start end {a b c} c]",
    "puts [lsearch -start -5 {a b c} c]",
    "puts [lsearch -start 99 {a b c} c]",
    "puts [lsearch -exact -integer {1 2 3} 0x2]",
    "puts [lsearch -exact -integer {1 2 03 4} 3]",
    "puts [lsearch -integer {1 2 03 4} 3]",
    "puts [lsearch -exact -real {1.0 2.5 3} 2.50]",
    "puts [lsearch -exa {a b} b]",
    "puts [lsearch -all -inline {ab cd ef} {[ac]*}]",
    "puts [lsearch -all -inline {ab abc a} {a?}]",
    "puts [lsearch -all -inline {a* ab} {a\\*}]",
    "puts [lsearch -all -inline {abc xbc} {[a-c]bc}]",
    "puts [lsearch -all -inline {abc a*c} {a[*]c}]",
    "puts [lsearch {a b c} *]",
    "puts [lsearch -all -inline {aXb ab aXXb} a*b]",
    // lsort.
    "puts [lsort {a10 B2 b1 a1 a2}]",
    "puts [lsort -integer {5 3 1 2 11 4}]",
    "puts [lsort -integer {1 2 0x5 7 0 4 -1}]",
    "puts [lsort -real {.5 0.07e1 0.4 6e-1}]",
    "puts [lsort -real {5 3 1 2 11 4}]",
    "puts [lsort -decreasing {a b c}]",
    "puts [lsort -unique {a b c a b c a b c}]",
    "puts [lsort -unique -integer {1 0x1 01 2}]",
    "puts [lsort -unique -decreasing {a b c a b c}]",
    "puts [lsort -indices {c a b}]",
    "puts [lsort -indices -unique {b a b a}]",
    "puts <[lsort {}]>",
    "puts [lsort {x}]",
    "puts [lsort {{b 1} {a 2}}]",
    "puts [lsort {{a 5} { c 3} {b 4} {e 1} {d 2}}]",
    "puts [lsort -increasing -decreasing {a b c}]",
    "puts [lsort {é a z}]",
    "puts [lsort {B a A b}]",
    "puts [lsort -integer -unique {3 1 3 1 2}]",
    "puts [lsort -unique {{a b} {a  b} {a b}}]",
    // join.
    "puts [join {a b c}]",
    "puts [join {a b c} ,]",
    "puts [join {1 2 3 4 5} \", \"]",
    "puts <[join {} ,]>",
    "puts [join {a} ,]",
    "puts [join {1 {2 3} 4 {5 {6 7} 8}}]",
    "puts [join {a b} {}]",
    "puts [join \"a  b\" -]",
    // split.
    "puts [split comp.lang.tcl .]",
    "puts [split \"alpha beta gamma\" temp]",
    "puts [split \"Example with {unbalanced brace character\"]",
    "puts [split \"Hello world\" {}]",
    "puts <[split {} ,]>",
    "puts [split a,,b ,]",
    "puts [split ,a, ,]",
    "puts [split \"a b\tc\nd\"]",
    "puts [split \"a\\vb\"]",
    "puts [split abc {}]",
    "puts [split \"a b\" {}]",
    // concat.
    "puts <[concat]>",
    "puts <[concat {}]>",
    "puts [concat a b {c d e} {f {g h}}]",
    "puts [concat \" a b {c   \" d \"  e} f\"]",
    "puts [concat \"a   b   c\" { d e f }]",
    "puts [concat a {} b]",
    "puts [concat \"x\\\\ \" b]",
    "puts [concat \"  \" a]",
    // foreach.
    "foreach x {1 3 5} {puts $x}",
    "set x {}\nforeach {i j} {a b c d e f} {lappend x $j $i}\nputs $x",
    "set x {}\nforeach i {a b c} j {d e f g} {lappend x $i $j}\nputs $x",
    "set x {}\nforeach i {a b c} {j k} {d e f g} {lappend x $i $j $k}\nputs $x",
    "set n 0\nforeach x {} {incr n}\nputs $n",
    "puts <[foreach x {1 2} {set y $x}]>",
    "set r {}\nforeach x {1 2 3} {if {$x == 2} break\nlappend r $x}\nputs $r",
    "set r {}\nforeach x {1 2 3} {if {$x == 2} continue\nlappend r $x}\nputs $r",
    "foreach {i j} {a b c} {}\nputs [list $i $j]",
    "foreach x {1 2 3} {}\nputs $x",
    "set r {}\nforeach x \"a  b\" {lappend r <$x>}\nputs $r",
    "set t 0\nforeach n {1 2 3 4 5} {set t [expr {$t + $n}]}\nputs $t",
    "foreach a {1 2} b {x y z} {puts \"$a-$b\"}",
    "set r {}\nforeach x {a b} {foreach y {1 2} {lappend r $x$y}}\nputs $r",
    "set r {}\nforeach x {a b c d} {if {$x eq \"b\"} continue\nif {$x eq \"d\"} break\nlappend r $x}\nputs $r",
    "foreach x [list [list a b] [list c d]] {puts [lindex $x 1]}",
    // expr's membership operators.
    "puts [expr {\"b\" in {a b c}}]",
    "puts [expr {\"z\" in {a b c}}]",
    "puts [expr {\"z\" ni {a b c}}]",
    "puts [expr {1 in {1 2 3}}]",
    "puts [expr {1 in {01 2 3}}]",
    "puts [expr {1.0 in {1 2}}]",
    "puts [expr {\"\" in {}}]",
    "puts [expr {\"\" in {{} a}}]",
    "set L {a b}\nputs [expr {\"a\" in $L}]",
    "puts [expr {\"a b\" in {{a b} c}}]",
    "puts [expr {1 in {1 2} == 1}]",
    "puts [expr {[llength {a b c}] in {2 3 4}}]",
    // Commands feeding each other.
    "puts [llength [list a {b c} d]]",
    "puts [lindex [lsort {c a b}] 0]",
    "puts [join [lsort -integer {10 9 8}] +]",
    "puts [lsearch [split a:b:c :] b]",
    "puts [lreverse [split abc {}]]",
    "set v [list a b]\nlappend v [list c d]\nputs [llength $v]",
    "puts [concat [list a b] [list c d]]",
    "puts [lrange [lsearch -all {a b a b} a] 0 end]",
    "set i 0\nset acc {}\nwhile {$i < 3} {lappend acc [lindex {x y z} $i]\nincr i}\nputs $acc",
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
/// error it reported.
fn reference(tclsh: &PathBuf, program: &str) -> (String, Option<String>) {
    // The tests here run concurrently, so each program needs its own file.
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "tclrs-list-{}-{}.tcl",
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
fn list_execution_matches_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };

    let mut failures = Vec::new();
    for program in PROGRAMS {
        let (expected, error) = reference(&tclsh, program);
        assert!(
            error.is_none(),
            "tclsh rejected program:\n{program}\n{}",
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

/// Element values that need quoting, driven through the whole cycle: build a
/// list, count it, read the elements back, and nest one list inside another.
///
/// Written as one generated program rather than one per case so the whole
/// matrix costs a single tclsh run. Every awkward element is exercised against
/// every part of the cycle, which is what catches a mode-selection mistake in
/// the formatter: choosing braces where the reference escapes still round-trips
/// through `lindex`, so only the printed form gives it away.
#[test]
fn quoting_round_trips_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };

    // Each entry is Tcl source for one word, chosen so the resulting value hits
    // a different branch of the element scanner.
    const ELEMENTS: &[&str] = &[
        "{}",
        "plain",
        "{a b}",
        "{a  b}",
        "{  }",
        "\"a\\tb\"",
        "\"a\\nb\"",
        "\"a\\vb\"",
        "\"a\\rb\"",
        "\"a\\fb\"",
        "a\\\\b",
        "{a$b}",
        "{a;b}",
        "{a[b}",
        "{a]b}",
        "{a\"b}",
        "a\\{b",
        "a\\}b",
        "{a{b}c}",
        "\\{ab",
        "{\"ab}",
        "#a",
        "{#}",
        "{ #a}",
        "\\{",
        "\\}",
        "ab\\\\",
        "{a\\ b}",
        "{a{b]c}d}",
        "{[a]}",
        "{$}",
        "{]]}",
        "{\"\"}",
        "{{}}",
        "0",
        "1.5",
    ];

    let mut program = String::new();
    for element in ELEMENTS {
        program.push_str(&format!("puts <[list {element}]>\n"));
        program.push_str(&format!("puts [llength [list {element}]]\n"));
        program.push_str(&format!("puts <[lindex [list {element}] 0]>\n"));
        program.push_str(&format!("puts <[list x {element} y]>\n"));
        program.push_str(&format!("puts <[lindex [list x {element} y] 1]>\n"));
        program.push_str(&format!("puts <[list [list {element}] {element}]>\n"));
        program.push_str(&format!(
            "puts <[lindex [lindex [list [list {element}] {element}] 0] 0]>\n"
        ));
        program.push_str(&format!("puts <[join [list {element} z] |]>\n"));
        program.push_str(&format!("puts <[lreverse [list {element} z]]>\n"));
        program.push_str(&format!("puts <[lsort [list {element} z]]>\n"));
        program.push_str(&format!("puts <[concat [list {element}] z]>\n"));
        program.push_str(&format!("puts <[lrange [list a {element} b] 1 1]>\n"));
        program.push_str(&format!("puts <[linsert [list a b] 1 {element}]>\n"));
        program.push_str(&format!("puts <[lreplace [list a b] 0 0 {element}]>\n"));
        program.push_str(&format!(
            "set v {{}}\nlappend v {element}\nputs <$v>\nputs [llength $v]\n"
        ));
        program.push_str(&format!(
            "set n 0\nforeach e [list {element} z] {{incr n}}\nputs $n\n"
        ));
    }

    let (expected, error) = reference(&tclsh, &program);
    assert!(error.is_none(), "tclsh rejected the generated program");
    let outcome = tclrs::eval(&program).expect("tclrs runs the generated program");

    let mismatches: Vec<String> = outcome
        .output
        .lines()
        .zip(expected.lines())
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(i, (a, b))| format!("line {}: tclsh {b:?}, tclrs {a:?}", i + 1))
        .collect();
    assert!(
        mismatches.is_empty() && outcome.output == expected,
        "{} of {} lines diverge:\n{}",
        mismatches.len(),
        expected.lines().count(),
        mismatches.join("\n")
    );
}

/// `foreach` across the shapes its argument grammar allows: one to three
/// variables per list, one or two lists, and lists whose lengths do not divide
/// evenly by the variable count, where the last iteration is padded with empty
/// values. The iteration count is what the longest list demands, so the two
/// lists disagreeing about how many iterations they can fill is the case worth
/// generating rather than hand-picking.
/// `lappend` extends the list in the variable's own string when nothing else
/// holds it, which is what keeps building one linear rather than quadratic. The
/// cases below are the ones that path can get wrong and the ordinary
/// `lappend` programs cannot reach, because each needs a *sequence* of appends:
///
/// * quoting at a position other than the first, once the fast path is warm —
///   only a list's first element quotes a leading `#`;
/// * a value another variable is holding, which must not change under it;
/// * a procedure's local list, which lives in a frame slot rather than in the
///   global table, and a `global` one inside a procedure, which does not;
/// * two lists grown in turn, so the fast path cannot assume the value it
///   extended last is the one in front of it;
/// * a coroutine growing its own list while the script grows another.
#[test]
fn lappend_in_place_matches_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };

    let program = concat!(
        // Quoting stays right at every position, not just the first.
        "set s {}\n",
        "foreach e {plain #hash {} {a b} {a]b} {a\"b} {a$b} {a;b} {a[b} {a{b}c}} {\n",
        "    lappend s $e\n",
        "    puts \"[llength $s] <$s>\"\n",
        "}\n",
        "foreach e $s { puts \"e=<$e>\" }\n",
        // A value someone else is holding must not change under them.
        "set l {}\n",
        "lappend l a b\n",
        "set held $l\n",
        "lappend l c\n",
        "puts \"$l | $held\"\n",
        "lappend held z\n",
        "puts \"$l | $held\"\n",
        // A frame slot, and a global reached from inside a procedure.
        "proc local {n} {\n",
        "    set out {}\n",
        "    for {set i 0} {$i < $n} {incr i} { lappend out $i }\n",
        "    return $out\n",
        "}\n",
        "puts [local 6]\n",
        "puts [llength [local 40]]\n",
        "set acc {}\n",
        "proc add {x} {\n",
        "    global acc\n",
        "    lappend acc $x\n",
        "    return $acc\n",
        "}\n",
        "add p\n",
        "puts \"[add q] / $acc\"\n",
        // Two lists grown in turn.
        "set a {}\n",
        "set b {}\n",
        "foreach x {1 2 3} { lappend a $x ; lappend b [expr {$x * 10}] }\n",
        "puts \"$a | $b\"\n",
        // A coroutine's own list, grown between resumptions.
        "proc gen {} {\n",
        "    set inner {}\n",
        "    foreach x {x y z} { lappend inner $x ; yield $inner }\n",
        "    return $inner\n",
        "}\n",
        "set outer {}\n",
        "lappend outer [coroutine g gen]\n",
        "lappend outer [g]\n",
        "lappend outer [g]\n",
        "puts \"$outer\"\n",
    );

    let (expected, error) = reference(&tclsh, program);
    assert!(
        error.is_none(),
        "tclsh rejected the program:\n{}",
        error.unwrap_or_default()
    );
    let outcome = tclrs::eval(program).expect("tclrs runs the program");
    assert_eq!(outcome.output, expected, "lappend diverges from tclsh");
}

#[test]
fn foreach_shapes_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };

    const VALUES: [&str; 6] = ["", "a", "a b", "a b c", "a b c d", "a b c d e"];
    let mut program = String::new();
    for vars in 1..=3 {
        let names: Vec<String> = (0..vars).map(|i| format!("v{i}")).collect();
        let varlist = names.join(" ");
        let show = names
            .iter()
            .map(|n| format!("${n}"))
            .collect::<Vec<_>>()
            .join(",");
        for values in VALUES {
            program.push_str(&format!(
                "set trace {{}}\nforeach {{{varlist}}} {{{values}}} {{lappend trace \"{show}\"}}\nputs <$trace>\n"
            ));
            for other in [1usize, 2] {
                let second: Vec<String> = (0..other).map(|i| format!("w{i}")).collect();
                let second_show = second
                    .iter()
                    .map(|n| format!("${n}"))
                    .collect::<Vec<_>>()
                    .join(",");
                program.push_str(&format!(
                    "set trace {{}}\nforeach {{{varlist}}} {{{values}}} {{{}}} {{p q r}} {{lappend trace \"{show}/{second_show}\"}}\nputs <$trace>\n",
                    second.join(" ")
                ));
            }
        }
    }

    let (expected, error) = reference(&tclsh, &program);
    assert!(error.is_none(), "tclsh rejected the generated program");
    let outcome = tclrs::eval(&program).expect("tclrs runs the generated program");
    assert_eq!(outcome.output, expected);
}

/// The glob matcher, over every pattern shape `string match` supports crossed
/// with subjects that exercise them.
#[test]
fn glob_matching_matches_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };

    const PATTERNS: &[&str] = &[
        "*",
        "a*",
        "*b",
        "a*b",
        "**b",
        "?",
        "a?",
        "?b",
        "a?c",
        "[abc]",
        "[a-c]",
        "[c-a]",
        "[^a]",
        "[]a]",
        "[a",
        "a[bc]d",
        "\\*",
        "a\\*b",
        "\\\\",
        "abc",
        "",
        "[a-c]*[x-z]",
        "*a*a*",
    ];
    const SUBJECTS: &[&str] = &[
        "", "a", "b", "ab", "abc", "aXb", "aXXb", "a*b", "a?b", "a\\b", "[abc]", "azz", "aaa",
        "ABC", "]",
    ];

    let mut program = String::new();
    for pattern in PATTERNS {
        let subjects = SUBJECTS
            .iter()
            .map(|s| format!("{{{s}}}"))
            .collect::<Vec<_>>()
            .join(" ");
        program.push_str(&format!(
            "puts <[lsearch -all [list {subjects}] {{{pattern}}}]>\n"
        ));
    }

    let (expected, error) = reference(&tclsh, &program);
    assert!(error.is_none(), "tclsh rejected the generated program");
    let outcome = tclrs::eval(&program).expect("tclrs runs the generated program");
    assert_eq!(outcome.output, expected);
}

/// Index expressions, resolved against lists of every length from empty up.
#[test]
fn index_forms_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };

    const INDICES: &[&str] = &[
        "0", "1", "2", "-1", "-2", "99", "end", "end-0", "end-1", "end-2", "end+1", "end+2",
        "end+-1", "end--1", "0+1", "2-1", "1+2", "0x2", "007", "1_0", "+1", " 1 ", "0d3",
    ];

    let mut program = String::new();
    for len in 0..4 {
        let values: Vec<String> = (0..len).map(|i| format!("e{i}")).collect();
        let list = values.join(" ");
        for index in INDICES {
            program.push_str(&format!("puts <[lindex {{{list}}} {{{index}}}]>\n"));
            program.push_str(&format!("puts <[lrange {{{list}}} {{{index}}} end]>\n"));
            program.push_str(&format!("puts <[lrange {{{list}}} 0 {{{index}}}]>\n"));
            program.push_str(&format!("puts <[linsert {{{list}}} {{{index}}} X]>\n"));
            program.push_str(&format!(
                "puts <[lreplace {{{list}}} {{{index}}} {{{index}}} X]>\n"
            ));
            // `lsearch -start` is skipped on the empty list: tclsh 9.0.4
            // segfaults on `lsearch -start -1 {} e1`, so there is no reference
            // behavior to compare against for that combination.
            if len > 0 {
                program.push_str(&format!(
                    "puts <[lsearch -start {{{index}}} {{{list}}} e1]>\n"
                ));
            }
        }
    }

    let (expected, error) = reference(&tclsh, &program);
    assert!(error.is_none(), "tclsh rejected: {error:?}");
    let outcome = tclrs::eval(&program).expect("tclrs runs the generated program");
    assert_eq!(outcome.output, expected);
}

/// Malformed lists, bad indices and unusable operands must be refused with the
/// interpreter's own diagnostic, not silently coerced into something.
#[test]
fn errors_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };

    let programs = [
        "puts [llength \"a {b\"]",
        "puts [llength \"a \\\"b\"]",
        "puts [lindex \"a {b c}x d\" 1]",
        "puts [lindex \"a \\\"b c\\\"x d\" 1]",
        "puts [lindex {a b c} foo]",
        "puts [lindex {a b c} end-]",
        "puts [lindex {a b c} 1.0]",
        "puts [lrange {a b c} \"1 2\" end]",
        "puts [lrange {a b c} \"end \" end]",
        "puts [lrange {a b c} \" end\" end]",
        "puts [lrange {a b c} 1+2+3 end]",
        "puts [lsort -integer {1 x}]",
        "puts [lsort -real {1 x}]",
        "puts [lsort -integer {1.5 2}]",
        "puts [lsort -integer {\"\" 2}]",
        "puts [lsort -zzz {a}]",
        "puts [lsearch -zzz {a b} a]",
        "puts [lsearch -in {a b} b]",
        "puts [lsearch -- {a b} b]",
        "puts [lsearch -exact -integer {1 2 x} 3]",
        "set v \"a {b\"\nlappend v c",
        "puts [expr {1 in \"a {b\"}]",
        "foreach x \"a {b\" {puts $x}",
        "foreach {} {a b} {puts x}",
        // Argument counts, whose wording the reference also fixes.
        "foreach x {a b}",
        "puts [llength]",
        "puts [llength a b]",
        "puts [lindex]",
        "puts [lrange {a b} 0]",
        "puts [lreplace {a b c} 0]",
        "puts [linsert {a b}]",
        "puts [lsearch {a}]",
        "puts [join]",
        "puts [join a b c]",
        "puts [split]",
        "puts [lsort]",
        "puts [lreverse]",
        "lappend",
        "nosuchcommand a b",
    ];

    let mut failures = Vec::new();
    for program in programs {
        let (_, error) = reference(&tclsh, program);
        let Some(expected) = error else {
            failures.push(format!("program:\n{program}\n  tclsh accepted it"));
            continue;
        };
        match tclrs::eval(program) {
            Ok(outcome) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs produced {:?}",
                outcome.output
            )),
            Err(e) if !e.starts_with(&expected) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs: {e:?}"
            )),
            Err(_) => {}
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} programs diverge:\n\n{}",
        failures.len(),
        programs.len(),
        failures.join("\n\n")
    );
}

/// Options that exist in the reference implementation but are not built here
/// must say so rather than being ignored, which would silently change a result.
#[test]
fn unimplemented_options_are_refused() {
    for (src, expected) in [
        ("puts [lsearch -sorted {a b} a]", "lsearch -sorted"),
        ("puts [lsearch -nocase {a b} A]", "lsearch -nocase"),
        ("puts [lsort -command x {a b}]", "lsort -command"),
        ("puts [lsort -index 0 {{a b}}]", "lsort -index"),
        ("puts [lsort -dictionary {a b}]", "lsort -dictionary"),
    ] {
        let err = tclrs::eval(src).expect_err(&format!("{src:?} should fail"));
        assert!(
            err.contains(expected) && err.contains("not supported yet"),
            "{src:?}: expected a refusal mentioning {expected:?}, got {err:?}"
        );
    }

    // `-regexp` was on that list until the regular-expression engine landed.
    // Pinned here rather than dropped, so that the day it starts refusing
    // again this test says so; what it *answers* is compared against tclsh by
    // `tests/regexp_differential.rs`.
    assert_eq!(
        tclrs::eval("puts [lsearch -regexp {abc bcd} {^b}]")
            .expect("lsearch -regexp is implemented")
            .output,
        "1\n"
    );
}
