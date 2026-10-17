//! One-index `lset` edits the list's string where it sits (`src/list_spans.rs`)
//! and `lrange` reads through the split cache (`src/cmd_list.rs`). These pin
//! what that must not change, with expectations written from `lset(n)` and
//! `lrange(n)`: the canonical string a write leaves, the independence of a
//! copy the script kept, a refused write leaving the variable alone, and the
//! cost staying linear.

fn run(script: &str) -> String {
    tclrs::eval(script)
        .unwrap_or_else(|e| panic!("script failed: {e}\n{script}"))
        .output
}

#[test]
fn lset_leaves_the_canonical_form_whatever_spelling_the_list_had() {
    assert_eq!(
        run("set l {a    b   {c d}}\nlset l 1 X\nputs $l\nlset l end {p q}\nputs $l\nlset l 0 {}\nputs $l"),
        "a X {c d}\na X {p q}\n{} X {p q}\n"
    );
}

#[test]
fn replacing_an_element_with_a_longer_or_shorter_one_keeps_the_rest_intact() {
    let out = run("set l {}\n\
         foreach x {a b c d e} {lappend l $x$x}\n\
         lset l 1 {a much longer element}\n\
         lset l 0 z\n\
         lset l 4 {}\n\
         lset l 2 12345678\n\
         puts $l\n\
         puts [lindex $l 1]\n\
         puts [lindex $l 3]\n\
         puts [llength $l]");
    assert_eq!(
        out,
        "z {a much longer element} 12345678 dd {}\na much longer element\ndd\n5\n"
    );
}

#[test]
fn one_past_the_end_appends_and_two_past_is_refused() {
    let out = run("set l {a b}\n\
         lset l 2 c\n\
         puts $l\n\
         puts [catch {lset l 5 x} m]\nputs $m\n\
         puts [catch {lset l -1 x} m]\nputs $m\n\
         puts [catch {lset l foo x} m]\nputs $m\n\
         puts $l\n\
         set e {}\n\
         lset e 0 first\n\
         puts $e");
    assert_eq!(
        out,
        "a b c\n\
         1\nindex \"5\" out of range\n\
         1\nindex \"-1\" out of range\n\
         1\nbad index \"foo\": must be integer?[+-]integer? or end?[+-]integer?\n\
         a b c\n\
         first\n"
    );
}

#[test]
fn elements_that_need_quoting_are_quoted_as_list_elements() {
    let out = run("set l {a b c}\n\
         lset l 0 {x y}\n\
         lset l 1 #h\n\
         lset l 2 {\\{}\n\
         puts $l\n\
         lset l 0 #first\n\
         puts $l\n\
         puts [llength $l]");
    assert_eq!(out, "{x y} #h {\\{}\n{#first} #h {\\{}\n3\n");
}

#[test]
fn a_copy_the_script_kept_does_not_change_when_the_original_does() {
    let out = run("set l {a b c}\n\
         lset l 1 X\n\
         set keep $l\n\
         lset l 1 Y\n\
         lset l 3 Z\n\
         puts $keep\nputs $l");
    assert_eq!(out, "a X c\na Y c Z\n");
}

#[test]
fn lset_works_on_array_elements_locals_and_upvar_names() {
    let out = run("array set a {e {1 2 3}}\n\
         lset a(e) 1 two\n\
         puts $a(e)\n\
         proc local {} {set l {x y}; lset l 0 p; lset l 2 q; return $l}\n\
         puts [local]\n\
         proc via {name} {upvar 1 $name v; lset v end last}\n\
         set g {m n o}\n\
         via g\n\
         puts $g");
    assert_eq!(out, "1 two 3\np y q\nm n last\n");
}

#[test]
fn a_nested_index_list_still_takes_the_general_path() {
    let out = run("set l {{a b} {c d}}\nlset l {1 0} X\nputs $l\nlset l {0 end} Y\nputs $l");
    assert_eq!(out, "{a b} {X d}\n{a Y} {X d}\n");
}

#[test]
fn lrange_takes_slices_of_one_list_without_resplitting_it() {
    let out = run("set l {}\n\
         for {set i 0} {$i < 20000} {incr i} {lappend l $i}\n\
         set s 0\n\
         for {set i 0} {$i < 20000} {incr i} {incr s [lindex [lrange $l $i $i] 0]}\n\
         puts $s\n\
         puts [lrange $l end-2 end]\n\
         puts <[lrange $l 5 2]>\n\
         puts [lrange {a {b c} d} 1 1]");
    assert_eq!(out, "199990000\n19997 19998 19999\n<>\n{b c}\n");
}

#[test]
fn assigning_every_element_of_a_large_list_is_linear() {
    let started = std::time::Instant::now();
    let out = run("set l {}\n\
         for {set i 0} {$i < 5000} {incr i} {lappend l 0}\n\
         for {set i 0} {$i < 5000} {incr i} {lset l $i [expr {$i * 2}]}\n\
         puts \"[lindex $l 0] [lindex $l 4999] [llength $l]\"");
    assert_eq!(out, "0 9998 5000\n");
    // Splitting and joining the list per element took longer than this at
    // this size by an order of magnitude; the bound is generous for a debug
    // build on a loaded machine.
    assert!(
        started.elapsed().as_secs() < 30,
        "lset loop took {:?}",
        started.elapsed()
    );
}
