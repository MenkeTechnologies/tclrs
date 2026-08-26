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
    // lsort -dictionary: numbers inside a string compare as numbers, letters
    // compare case-insensitively, and leading zeros and case break ties.
    "puts [lsort -dictionary {a10 a9 a1}]",
    "puts [lsort -dictionary {x007 x7 x07}]",
    "puts [lsort -dictionary {A a B b}]",
    "puts [lsort -dictionary {abc ABC Abc}]",
    "puts [lsort -dictionary {a1b2 a1b10 a2b1}]",
    "puts [lsort -dictionary {0 00 000}]",
    "puts [lsort -dictionary {a0b a00b}]",
    "puts [lsort -dictionary {foo10bar foo9bar foo10Bar}]",
    "puts [lsort -dictionary -decreasing {a10 a9}]",
    "puts [lsort -dictionary -unique {A a}]",
    "puts [lsort -dictionary {10 9 1x 1}]",
    // lsort -nocase, which is only a mode of the ascii sort.
    "puts [lsort -nocase {B a C}]",
    "puts [lsort -nocase {aB Ab}]",
    "puts [lsort -nocase -unique {a A b}]",
    "puts [lsort -nocase -integer {10 9}]",
    // lsort -index, including into a nested sublist and alongside -stride.
    "puts [lsort -index 1 {{a 2} {b 1}}]",
    "puts [lsort -index end {{a 2} {b 1}}]",
    "puts [lsort -index 0 -integer {{10 x} {9 y}}]",
    "puts [lsort -index {0 1} {{{a b}} {{a a}}}]",
    "puts [lsort -indices -index 1 {{a 2} {b 1}}]",
    "puts [lsort -stride 2 -index 1 {b 1 a 2}]",
    "puts [lsort -stride 2 -index 0 {b 1 a 2}]",
    "puts [catch {lsort -index 5 {{a b}}} m]\nputs $m",
    "puts [catch {lsort -index end-5 {{a b}}} m]\nputs $m",
    "puts [catch {lsort -stride 2 -index 3 {b 1 a 2}} m]\nputs $m",
    // lsearch -sorted / -bisect: the binary search, in both orders, and the
    // leftmost-of-equals rule that separates the two.
    "puts [lsearch -sorted {1 3 5} 3]",
    "puts [lsearch -sorted {1 3 5} 4]",
    "puts [lsearch -sorted {1 3 3 3 5} 3]",
    "puts [lsearch -bisect {1 3 3 3 5} 3]",
    "puts [lsearch -bisect {1 3 5} 4]",
    "puts [lsearch -bisect {1 3 5} 0]",
    "puts [lsearch -sorted -integer {1 3 5} 5]",
    "puts [lsearch -sorted -decreasing {5 3 1} 3]",
    "puts [lsearch -bisect -decreasing {5 3 1} 4]",
    "puts [lsearch -sorted -all {1 3 3 5} 3]",
    "puts [lsearch -sorted -inline {1 3 5} 3]",
    "puts [lsearch -sorted -dictionary {a1 a9 a10} a10]",
    "puts [lsearch -sorted -glob {ab cd} c*]",
    "puts [catch {lsearch -bisect -all {1 3} 1} m]\nputs $m",
    // lsearch -nocase, -dictionary and -index.
    "puts [lsearch -nocase {A b} a]",
    "puts [lsearch -nocase -exact {A b} a]",
    "puts [lsearch -nocase -glob {AB cd} a*]",
    "puts [lsearch -nocase -regexp {AB cd} {^a}]",
    "puts [lsearch -dictionary {a10 a9} a9]",
    "puts [lsearch -index 0 {{a 1} {b 2}} b]",
    "puts [lsearch -index end {{a 1}} 1]",
    "puts [lsearch -index {0 1} {{{a b}}} b]",
    "puts [lsearch -all -index 0 {{a 1} {a 2}} a]",
    "puts [lsearch -inline -index 0 {{a 1}} a]",
    "puts [lsearch -stride 2 -index 1 {a 1 b 2} 2]",
    "puts [catch {lsearch -index 5 {{a b}} x} m]\nputs $m",
    // lsearch -subindices, which answers where the key is rather than where
    // the element is.
    "puts [lsearch -subindices -index 0 {{a 1}} a]",
    "puts [lsearch -subindices -index 0 -inline {{a 1}} a]",
    "puts [lsearch -subindices -index {0 1} -inline {{{a b}}} b]",
    "puts [lsearch -subindices -all -index 0 {{a 1} {a 2}} a]",
    "puts [catch {lsearch -subindices {a} a} m]\nputs $m",
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
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
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
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
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
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
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
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
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
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
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
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
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
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
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
    // Neither command refuses an option any more: `-command` was the last one,
    // and it landed. What it *answers* is compared against tclsh by
    // `sort_and_search_options_match_tclsh`; what is pinned here is that a
    // comparison command the script does not define is the reference
    // implementation's own diagnostic and not a refusal to compile.
    assert_eq!(
        tclrs::eval("proc c {a b} {string compare $a $b}\nputs [lsort -command c {b a}]")
            .expect("lsort -command is implemented")
            .output,
        "a b\n"
    );
    assert!(tclrs::eval("puts [lsort -command x {a b}]")
        .expect_err("an undefined comparison command fails when the sort runs")
        .contains("invalid command name \"x\""));

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

    // The same pinning for the options that were on that list until the
    // sorting and searching keys landed. What each *answers* is compared
    // against tclsh by `sort_and_search_options_match_tclsh`.
    for (src, want) in [
        ("puts [lsearch -sorted {a b} a]", "0\n"),
        ("puts [lsearch -bisect {1 3 5} 4]", "1\n"),
        ("puts [lsearch -nocase {a b} A]", "0\n"),
        ("puts [lsearch -index 0 {{a 1} {b 2}} b]", "1\n"),
        ("puts [lsearch -subindices -index 0 {{a 1}} a]", "0 0\n"),
        ("puts [lsort -index 0 {{b x} {a y}}]", "{a y} {b x}\n"),
        ("puts [lsort -dictionary {a10 a9}]", "a9 a10\n"),
        ("puts [lsort -nocase {B a}]", "a B\n"),
    ] {
        assert_eq!(
            tclrs::eval(src)
                .unwrap_or_else(|e| panic!("{src:?} is implemented: {e}"))
                .output,
            want,
            "{src:?}"
        );
    }
}

/// A list read by index must see every change made to it since the last read.
///
/// `src/cmd_list.rs` remembers the elements of the last few lists it split, so
/// that a loop reading one by index parses it once rather than once per turn —
/// which is what stops such a loop being quadratic in the list's length. That
/// cache is keyed on the value's *identity*, which is a pointer, and every
/// program below changes a list between two reads of it, through each of the
/// commands that can change one where it stands. What they pin is the property
/// the cache must not cost: that the second read answers from the list as it
/// now is, and that a copy taken before the change does not follow it.
///
/// It is worth saying what this does *not* prove. Simply removing the cache's
/// invalidation hook does not make these fail, because an entry holds a share
/// of the list's string and `Arc::get_mut` refuses to mutate a shared one — so
/// a cached list is copied rather than grown and the copy has an identity of
/// its own. These are a guard on the read path (the fast `lindex` that answers
/// from an entry, and the eviction order behind it), not on that invariant.
///
/// Each program is compared against tclsh rather than against a written
/// expectation, as everything else in this file is: a wrong answer here is a
/// plausible-looking value, not an error, and only the reference settles it.
#[test]
fn a_list_read_by_index_sees_every_change() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };

    let programs = [
        // lappend, which grows the variable's own string in place.
        "set l {a b c}\nputs [lindex $l 0][llength $l]\nlappend l d\nputs [lindex $l 3][llength $l]",
        // The same, read at the end rather than the start, so a stale length
        // shows as well as stale elements.
        "set l {a}\nputs [lindex $l end]\nlappend l b c\nputs [lindex $l end][llength $l]",
        // lset, which rewrites an element where it stands.
        "set l {a b c}\nputs [lindex $l 1]\nlset l 1 X\nputs [lindex $l 1][llength $l]",
        // lpop, which shortens it.
        "set l {a b c}\nputs [lindex $l 2][llength $l]\nlpop l\nputs [lindex $l end][llength $l]",
        // ledit, which replaces a range.
        "set l {a b c d}\nputs [lindex $l 1][llength $l]\nledit l 1 2 X Y Z\nputs [lindex $l 1][llength $l]",
        // A plain assignment, which replaces the value rather than changing it.
        "set l {a b c}\nputs [lindex $l 0]\nset l {x y}\nputs [lindex $l 0][llength $l]",
        // `append` to the same variable, which is a string operation on what a
        // list command had already split.
        "set l {a b}\nputs [lindex $l 1][llength $l]\nappend l { c}\nputs [lindex $l 2][llength $l]",
        // Two lists alive at once, read alternately: neither may answer with
        // the other's elements.
        "set a {1 2 3}\nset b {x y}\nputs [lindex $a 0][lindex $b 0][lindex $a 2][lindex $b 1]\nlappend a 4\nlappend b z\nputs [lindex $a 3][lindex $b 2][llength $a][llength $b]",
        // A copy taken before the change must not follow it — the aliasing case
        // that a cache keyed on identity has to get right in the other
        // direction.
        "set a {1 2}\nset b $a\nputs [lindex $b 1]\nlappend a 3\nputs \"[lindex $b end][llength $b] [lindex $a end][llength $a]\"",
        // The loop the cache exists for, with the list changing under it.
        "set l {}\nset out {}\nfor {set i 0} {$i < 20} {incr i} {\n  lappend l $i\n  append out [lindex $l end],[lindex $l 0],[llength $l] \n}\nputs $out",
        // A procedure's local list, so the value lives in a frame slot rather
        // than a VM global — a different `Place`, the same identity question.
        "proc p {} {\n  set l {a b}\n  set out [lindex $l 1]\n  lappend l c\n  return $out[lindex $l 2][llength $l]\n}\nputs [p]",
        // `upvar`, where two names reach one value.
        "proc p {name} {\n  upvar 1 $name l\n  set out [lindex $l 0]\n  lappend l z\n  return $out[lindex $l end]\n}\nset v {q r}\nputs [p v][lindex $v end][llength $v]",
        // A list an element of which is itself a list, read through a nested
        // index — the path that does not take the cache — after a change.
        "set l {{a b} {c d}}\nputs [lindex $l 1 0]\nlset l 1 0 X\nputs [lindex $l 1 0][lindex $l 0 1]",
        // More lists alive at once than the cache holds entries for, read in a
        // rotation, so that every one of them is evicted and re-split at least
        // once while the others are still being read.
        "set out {}\nforeach n {1 2 3 4 5 6} { set l$n [list $n a$n b$n] }\nfor {set turn 0} {$turn < 3} {incr turn} {\n  foreach n {1 2 3 4 5 6} {\n    append out [lindex [set l$n] 0][lindex [set l$n] end][llength [set l$n]]\n  }\n}\nputs $out",
        // The same, with one of them growing between the turns.
        "set out {}\nforeach n {1 2 3 4 5 6} { set l$n [list $n] }\nfor {set turn 0} {$turn < 3} {incr turn} {\n  lappend l3 $turn\n  foreach n {1 2 3 4 5 6} {\n    append out [lindex [set l$n] end][llength [set l$n]],\n  }\n}\nputs $out",
        // A list whose elements need quoting, so that the cached split and a
        // fresh one could differ in more than length.
        "set l [list {a b} {} {c\td}]\nputs [llength $l]|[lindex $l 0]|[lindex $l 1]|\nlappend l {e f}\nputs [llength $l]|[lindex $l 3]|[lindex $l 0]|",
    ];

    let mut failures = Vec::new();
    for program in programs {
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
        programs.len(),
        failures.join("\n\n")
    );
}
