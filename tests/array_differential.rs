//! Differential execution for associative data: array variables, `array`,
//! `dict`, and the list syntax they are written in.
//!
//! Same rule as `execution_differential`: no expected value is written by hand.
//! Every program is run by tclsh and by tclrs and the two outputs compared byte
//! for byte, so a misreading of Tcl's list quoting, its dict ordering, or its
//! element-lookup errors fails here rather than becoming a baked-in bug.
//!
//! Two orderings in this area are deliberately undefined by `array(n)` — the
//! order of `array names` and of `array get` — so no program below prints more
//! than one array element name directly. Multi-element arrays are checked
//! through order-independent operations (`array size`, `dict get` on the result
//! of `array get`) and, in `array_get_sorts_through_dict_operations`, through a
//! selection sort written in Tcl that turns the undefined order into a defined
//! one.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Programs whose output must match tclsh exactly.
fn programs() -> Vec<String> {
    let mut programs: Vec<String> = FIXED.iter().map(|s| s.to_string()).collect();
    programs.push(quoting_matrix("puts [dict create {} 1]"));
    programs.push(quoting_matrix("puts [dict get [dict create {} v] {}]"));
    programs.push(quoting_matrix(
        "puts [dict create outer [dict create {} 1]]",
    ));
    programs.push(quoting_matrix("puts [dict create a {}]"));
    programs
}

/// One program that exercises list-element quoting over every ASCII character,
/// in leading, interior and trailing position. `{}` in `template` is replaced by
/// the quoted-word form of the element under test.
///
/// The characters are written as `\xNN` escapes so the program text itself stays
/// plain, and so that the parser's escape handling is exercised alongside.
fn quoting_matrix(template: &str) -> String {
    let mut out = String::new();
    for code in 1u8..=126 {
        for shape in ["a\\x{:02x}b", "\\x{:02x}b", "a\\x{:02x}", "\\x{:02x}"] {
            let element = format!("\"{}\"", shape.replace("{:02x}", &format!("{code:02x}")));
            out.push_str(&template.replace("{}", &element));
            out.push('\n');
        }
    }
    out
}

const FIXED: &[&str] = &[
    // ── array elements ──
    "set a(1) x\nputs $a(1)",
    "set a(x) 1\nset a(y) 2\nputs [expr {$a(x)+$a(y)}]",
    "set i 3\nset a($i) hit\nputs $a(3)",
    "set i 3\nset a(3) hit\nputs $a($i)",
    "set i k\nset a(pre$i.post) v\nputs $a(prek.post)",
    "set a(v) 4\nputs [expr {$a(v)*$a(v)}]",
    "puts [set a(only) written]",
    "set a(k) {x y}\nputs [array get a]",
    "set a() empty\nputs [array names a]\nputs $a()",
    // `$a(x(y))` is a parse error in tclsh — only the parsed form constrains
    // the index text — so the element is read back through `array get`.
    "set a(x(y)) 1\nputs [array names a]\nputs [array get a]",
    "set a(1) one\nset a(1) two\nputs $a(1)\nputs [array size a]",
    // `q(x)y` does not end in `)`, so it names a scalar, not an element.
    "set q(x)y scalar\nputs [set q(x)y]\nputs [array exists q]",
    // ── incr on elements ──
    "set a(n) 5\nputs [incr a(n)]\nputs [incr a(n) 3]\nputs [incr a(n) -10]",
    "puts [incr a(new)]\nputs [array names a]",
    "set a(n) { 5 }\nputs [incr a(n)]",
    "set a(n) 1\nputs [incr a(n) 0x10]",
    "set i 0\nwhile {$i < 5} {set sq($i) [expr {$i*$i}]; incr i}\nputs [array size sq]\nputs $sq(4)",
    // ── unset ──
    "set a(k) v\nunset a(k)\nputs [array size a]\nputs [array exists a]",
    "unset -nocomplain nosuchthing\nputs survived",
    "set v 1\nunset v\nset v 2\nputs $v",
    "set p 1\nset q 2\nunset p q\nputs [array exists p]",
    "set a(k) v\nunset -nocomplain a(nope)\nputs [array size a]",
    "puts [unset -nocomplain nothing]x",
    // ── array subcommands ──
    "puts [array exists nope]",
    "set b 5\nputs [array exists b]",
    "array set a {}\nputs [array exists a]\nputs [array size a]",
    "array set a {p 1 q 2}\nputs [array size a]",
    "puts [array set a {p 1}]done",
    "array set a {solo 9}\nputs [array names a]\nputs [array get a]",
    "array set a {a 1}\narray set a {a 9 b 2}\nputs $a(a)\nputs [array size a]",
    "array set a {a 1 b 2}\nunset a\nputs [array exists a]",
    "array set a {ax 1 ay 2 b 3}\narray unset a a*\nputs [array size a]\nputs [array names a]",
    "array set a {a 1}\narray unset a\nputs [array exists a]",
    "set s scalar\narray unset s\nputs $s",
    "array set a {ax 1 ay 2 b 3}\nputs [array names a b*]\nputs [array get a b*]",
    "array set a {a* 1 ay 2}\nputs [array names a -exact a*]",
    // Three arguments: the last is the pattern, never a mode.
    "array set a {-exact 1 b 2}\nputs [array names a -exact]",
    "array set a {x 1}\nputs [array names a -glob x]",
    "array set a {ab 1 b 2}\nputs [array names a {[ab]b}]",
    "array set a {ab 1 zc 2 zz 3}\nputs [array size a]\nputs [array names a a?]x",
    "array set a {x 1}\nputs [array si a]",
    "puts [array size never]\nputs [array names never]\nputs [array get never]",
    // ── dict values ──
    "puts [dict create]x",
    "puts [dict create a 1 b 2]",
    "puts [dict create a 1 a 2]",
    "puts [dict create b 1 a 2 b 3]",
    "puts [dict create {a b} {c d} e {}]",
    "puts [dict create 1 one 2 two]",
    "puts [dict get {a 1 b 2} b]",
    "puts [dict get {a  1   b  2}]",
    "puts [dict get {a {b {c 7}}} a b c]",
    "puts [dict exists {a 1} a]\nputs [dict exists {a 1} z]",
    "puts [dict exists {a {b 1}} a b]\nputs [dict exists {a 1 b} a]",
    "puts [dict exists {a 1} a b]",
    "puts [dict keys {b 2 a 1 c 3}]",
    "puts [dict keys {bx 2 ax 1 by 3} b*]",
    "puts [dict keys {ab 1 ac 2 b 3} a?]",
    "puts [dict keys {a*b 1 axb 2} {a\\*b}]",
    "puts [dict keys {a 1 b 2} zzz]x",
    "puts [dict values {b 2 a 1 c 3}]",
    "puts [dict values {b 2 a 1 c 3} 2]",
    "puts [dict size {a 1 b 2}]\nputs [dict size {}]",
    "puts [dict remove {a 1 b 2 c 3} b]",
    "puts [dict remove {a 1 b 2 c 3} a c]",
    "puts [dict remove {a 1}]\nputs [dict remove {a 1} zz]",
    "puts [dict merge {a 1 b 2} {b 9 c 3}]",
    "puts [dict merge]x",
    "puts [dict merge {a  1}]",
    "puts [dict merge {a  1} {}]",
    "puts [dict merge {} {a  1}]",
    "puts [dict merge {a  1} {a 1}]",
    "puts [dict si {a 1 b 2}]",
    // ── dict variables ──
    "dict set d a 1\nputs $d",
    "set d {a 1 b 2}\nputs [dict set d b 9]\nputs $d",
    "set d {a 1}\ndict set d z 26\nputs $d",
    "dict set d a b 1\nputs $d",
    "dict set d a b c d 1\nputs $d",
    "set d {a 1 b 2 c 3}\ndict set d b 9\nputs $d",
    // ── dict for ──
    "dict for {k v} {b 2 a 1} {puts \"$k=$v\"}",
    "dict for {k v} {} {puts never}\nputs done",
    "puts [dict for {k v} {a 1} {expr 1}]x",
    "dict for {k v} {a 1 b 2 c 3} {if {$k eq \"b\"} break; puts $k}",
    "dict for {k v} {a 1 b 2 c 3} {if {$k eq \"b\"} continue; puts $k}",
    "dict for {k v} {a 1} {}\nputs \"$k $v\"",
    "set t 0\ndict for {k v} {a 1 b 2 c 3} {incr t $v}\nputs $t",
    "dict for {k v} {a 1 b 2} {dict for {i j} {x 8} {puts \"$k$i$v$j\"}}",
    "set n 0\nwhile {$n < 2} {dict for {k v} {a 1 b 2} {puts \"$n$k$v\"}; incr n}",
    // ── list quoting through dict ──
    "puts [dict create k {a b}]",
    "puts [dict create k {}]",
    "puts [dict create \"a\nb\" 1]",
    "puts [dict create {a$b} 1]",
    "puts [dict create {a[b]} 1]",
    "puts [dict create {a\"b} 1]",
    "puts [dict create {a;b} 1]",
    "puts [dict create \"a\\\\b\" 1]",
    "puts [dict create \"a\\\\\" 1]",
    "puts [dict create \"a\\{b\" 1]",
    "puts [dict create {a{b}c} 1]",
    "puts [dict create # 1]\nputs [dict create a #]\nputs [dict create a 1 # 2]",
    "puts [dict keys [dict create # 1 b 2]]\nputs [dict keys [dict create b 2 # 1]]",
    "puts [dict get {a\\nb 1} \"a\nb\"]",
    "puts [dict get {{a\\nb} 1} {a\\nb}]",
    "puts [dict get {\"a b\" 1} {a b}]",
    "puts [dict get \"a\tb\nc\td\" c]",
    // ── the two together ──
    "array set a {x 1 y 2}\nputs [dict get [array get a] y]\nputs [dict size [array get a]]",
    "array set a {x 1 y 2 z 3}\nset d [array get a]\nputs [dict exists $d z]\nputs [dict exists $d w]",
    "dict for {k v} {alpha 1 beta 2} {set counts($k) $v}\nputs [array size counts]\nputs $counts(beta)",
    "set d {}\nset i 0\nwhile {$i < 4} {dict set d k$i $i; incr i}\nputs $d\nputs [dict size $d]",
];

/// The selection sort that turns `array get`'s undefined order into a defined
/// one, so a multi-element array can be compared against tclsh at all.
const SORTED_ARRAY_WALK: &str = "\
array set a {delta 4 alpha 1 charlie 3 bravo 2}
set pairs [array get a]
set n [dict size $pairs]
set prev {}
set i 0
while {$i < $n} {
    set best {}
    dict for {k v} $pairs {
        if {($i == 0 || $k gt $prev) && ($best eq {} || $k lt $best)} {
            set best $k
        }
    }
    puts \"$best=[dict get $pairs $best] $a($best)\"
    set prev $best
    incr i
}
";

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
    // The test functions run in parallel, so the scratch file has to be unique
    // per call and not merely per process.
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "tclrs-assoc-{}-{}.tcl",
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

fn compare(tclsh: &PathBuf, programs: &[String]) {
    let mut failures = Vec::new();
    for program in programs {
        let expected = match reference(tclsh, program) {
            Ok(out) => out,
            Err(e) => {
                failures.push(format!("tclsh rejected program:\n{program}\n{e}"));
                continue;
            }
        };
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

#[test]
fn associative_execution_matches_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    compare(&tclsh, &programs());
}

/// A multi-element array printed in an order both implementations agree on.
#[test]
fn array_get_sorts_through_dict_operations() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    compare(&tclsh, &[SORTED_ARRAY_WALK.to_string()]);
}

/// Failures have to match too: the message tclsh produces is the specification
/// for the message tclrs produces.
#[test]
fn associative_errors_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };

    let programs = [
        // Reading and writing elements.
        "puts $a(1)",
        "set b 1\nputs $b(1)",
        "array set c {}\nputs $c(1)",
        "set d(1) x\nunset d(1)\nputs $d(1)",
        "set e 3\nset e(1) x",
        "set f(1) x\nset g $f",
        "set h(1) x\nset h 3",
        "set i(x) q\nincr i(x)",
        "set j(x) 1\nincr j(x) 2.5",
        // unset.
        "unset nosuchvar",
        "unset -- nosuchvar",
        "array set k {}\nunset k(1)",
        "set l 1\nunset l(1)",
        // The array command.
        "set m 1\narray set m {a 1}",
        "set n 1\narray set n {}",
        "array set o {a 1 b}",
        "array size p q",
        "array bogus q",
        "array s q",
        "array set q {a 1}\narray names q -bogus a",
        "array exists q x",
        "array set q",
        // The dict command.
        "puts [dict get {a 1} z]",
        "puts [dict get {a 1 b} a]",
        "puts [dict get x a]",
        "puts [dict size {a 1 b}]",
        "puts [dict create a]",
        "puts [dict bogus]",
        "puts [dict s {a 1}]",
        "dict for {k} {a 1} {}",
        "set r(1) x\ndict set r k v",
    ];

    let mut failures = Vec::new();
    for program in programs {
        let Err(expected) = reference(&tclsh, program) else {
            failures.push(format!(
                "tclsh accepted a program meant to fail:\n{program}"
            ));
            continue;
        };
        // tclsh writes the message followed by a stack trace; only the first
        // line is the message itself.
        let expected = expected.lines().next().unwrap_or_default().to_string();
        match tclrs::eval(program) {
            Err(actual) if actual.starts_with(&expected) => {}
            Err(actual) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs: {actual:?}"
            )),
            Ok(outcome) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs succeeded: {outcome:?}"
            )),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} error programs diverge:\n\n{}",
        failures.len(),
        programs.len(),
        failures.join("\n\n")
    );
}

/// The undefined orderings are at least stable here, which keeps tclrs's own
/// output reproducible from run to run even where tclsh's is not.
#[test]
fn array_names_and_get_are_sorted() {
    let names = tclrs::eval("array set a {delta 4 alpha 1 charlie 3}\nputs [array names a]")
        .expect("runs")
        .output;
    assert_eq!(names, "alpha charlie delta\n");
    let pairs = tclrs::eval("array set a {delta 4 alpha 1 charlie 3}\nputs [array get a]")
        .expect("runs")
        .output;
    assert_eq!(pairs, "alpha 1 charlie 3 delta 4\n");
}

/// Subcommands that exist in tclsh but not here must say so rather than do
/// something else.
#[test]
fn unimplemented_subcommands_are_refused() {
    for (src, expected) in [
        (
            "array startsearch a",
            "array startsearch is not supported yet",
        ),
        ("array for {k v} a {}", "array for is not supported yet"),
        ("array names a -regexp x", "needs regexp support"),
        (
            "dict filter {a 1} key a",
            "dict filter is not supported yet",
        ),
        ("dict incr d k", "dict incr is not supported yet"),
        ("dict unset d k", "dict unset is not supported yet"),
        ("dict with d {}", "dict with is not supported yet"),
        (
            "set a(1) x\ndict set a(1) k v",
            "array element is not supported yet",
        ),
    ] {
        let err = tclrs::eval(src).expect_err(&format!("{src:?} should fail"));
        assert!(
            err.contains(expected),
            "{src:?}: expected an error mentioning {expected:?}, got {err:?}"
        );
    }
}
