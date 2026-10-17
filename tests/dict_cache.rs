//! The one-key `dict set` / `dict incr` and the `dict get` / `exists` / `size`
//! reads work on a parsed copy of the dictionary that is kept between calls
//! (`src/dict_cache.rs`). These pin what that must not change: the canonical
//! string every write produces, the independence of a copy the script kept, the
//! variable surviving a refused write, and the cost staying linear.
//!
//! Expectations are written out from the `dict(n)` rules, not from a run of
//! either interpreter, so they hold with no tclsh installed.

use std::time::Instant;

fn run(script: &str) -> String {
    tclrs::eval(script)
        .unwrap_or_else(|e| panic!("script failed: {e}\n{script}"))
        .output
}

#[test]
fn a_write_leaves_the_canonical_form_whatever_spelling_the_dict_had() {
    assert_eq!(
        run("set d {a   1    b 2}\ndict set d c 3\nputs $d\ndict set d a 9\nputs $d"),
        "a 1 b 2 c 3\na 9 b 2 c 3\n"
    );
}

#[test]
fn keys_and_values_are_quoted_as_list_elements_on_every_path() {
    let out = run("set d {}\n\
         dict set d {a b} {x y}\n\
         dict set d #h {}\n\
         dict set d {q\"r} {[v]}\n\
         dict set d {a b} {}\n\
         dict set d #h {{n}}\n\
         puts $d\n\
         puts [dict get $d {q\"r}]\n\
         puts [dict size $d]");
    assert_eq!(out, "{a b} {} #h {{n}} q\\\"r {[v]}\n[v]\n3\n");
}

#[test]
fn replacing_a_value_with_a_longer_or_shorter_one_keeps_later_entries_intact() {
    let out = run("set d {}\n\
         foreach k {a b c d} {dict set d $k $k$k}\n\
         dict set d b {a much longer value}\n\
         dict set d a x\n\
         dict set d d {}\n\
         dict set d c 123456789\n\
         puts $d\n\
         foreach k {a b c d} {puts [dict get $d $k]}");
    assert_eq!(
        out,
        "a x b {a much longer value} c 123456789 d {}\nx\na much longer value\n123456789\n\n"
    );
}

#[test]
fn a_copy_the_script_kept_does_not_change_when_the_original_does() {
    let out = run("set d {a 1}\n\
         dict set d b 2\n\
         set keep $d\n\
         dict set d b 3\n\
         dict set d c 4\n\
         dict incr d a\n\
         puts $keep\n\
         puts $d\n\
         puts [dict get $keep b]");
    assert_eq!(out, "a 1 b 2\na 2 b 3 c 4\n2\n");
}

#[test]
fn a_refused_write_leaves_the_variable_as_it_was() {
    let out = run("set bad {a b c}\n\
         puts [catch {dict set bad k v} m]\nputs $m\nputs $bad\n\
         set n {a 1 b x}\n\
         puts [catch {dict incr n b} m]\nputs $m\nputs $n\n\
         dict incr n a 41\nputs $n\n\
         array set arr {x 1}\n\
         puts [catch {dict set arr k v} m]\nputs $m");
    assert_eq!(
        out,
        "1\nmissing value to go with key\na b c\n\
         1\nexpected integer but got \"x\"\na 1 b x\n\
         a 42 b x\n\
         1\ncan't set \"arr\": variable is array\n"
    );
}

#[test]
fn the_variable_may_be_a_procedure_local_or_reached_through_upvar() {
    let out = run(
        "proc bump {name key} {upvar 1 $name d; dict incr d $key; dict set d last $key}\n\
         proc local {} {set d {}; dict set d a 1; dict incr d a 5; dict incr d n; return $d}\n\
         set c {a 1}\n\
         bump c a\n\
         puts $c\n\
         puts [local]",
    );
    assert_eq!(out, "a 2 last a\na 6 n 1\n");
}

#[test]
fn building_and_reading_a_large_dict_is_linear() {
    let started = Instant::now();
    let out = run("set d {}\n\
         for {set i 0} {$i < 20000} {incr i} {dict set d k$i $i}\n\
         set sum 0\n\
         for {set i 0} {$i < 20000} {incr i} {incr sum [dict get $d k$i]}\n\
         for {set i 0} {$i < 20000} {incr i} {dict incr d k$i}\n\
         puts \"$sum [dict size $d] [dict get $d k19999] [dict exists $d k20000]\"");
    assert_eq!(out, "199990000 20000 20000 0\n");
    // Quadratic parsing took minutes here; the bound is generous for a debug
    // build on a loaded machine and still orders of magnitude below that.
    assert!(
        started.elapsed().as_secs() < 60,
        "dict loop took {:?}",
        started.elapsed()
    );
}
