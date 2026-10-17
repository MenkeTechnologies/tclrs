//! `array for {key value} arrayName script`. Expectations are what tclsh 9.0.4
//! answered; the walk's order is sorted here because the order of an array is
//! not something a script may rely on.

fn run(script: &str) -> String {
    tclrs::eval(script)
        .unwrap_or_else(|e| panic!("script failed: {e}\n{script}"))
        .output
}

#[test]
fn every_pair_is_visited_once() {
    let out = run("array set a {x 1 y 2 z 3}\n\
         set seen {}\n\
         array for {k v} a {lappend seen $k=$v}\n\
         puts [lsort $seen]");
    assert_eq!(out, "x=1 y=2 z=3\n");
}

#[test]
fn continue_break_and_return_act_on_the_walk() {
    let out = run("array set a {x 1 y 2 z 3}\n\
         set seen {}\n\
         array for {k v} a {if {$k eq {y}} continue; lappend seen $k}\n\
         puts [lsort $seen]\n\
         set n 0\n\
         array for {k v} a {incr n; break}\n\
         puts $n\n\
         proc first {} {array set b {only 1}; array for {k v} b {return $k$v}}\n\
         puts [first]");
    assert_eq!(out, "x z\n1\nonly1\n");
}

#[test]
fn the_variables_outlive_the_walk() {
    let out = run("array set a {x 1}\narray for {k v} a {}\nputs $k$v");
    assert_eq!(out, "x1\n");
}

#[test]
fn refusals_are_worded_as_tclsh_words_them() {
    let out = run("array set a {x 1}\n\
         set s 5\n\
         foreach cmd {{array for {k} a {}} {array for {k v w} a {}} {array for {k v} nosuch {}} \
                      {array for {k v} s {}} {array for {k v} a} {array for {k v} a(x) {}}} {\n\
           catch $cmd m o\n\
           puts \"$m | [dict get $o -errorcode]\"\n\
         }");
    assert_eq!(
        out,
        "must have two variable names | TCL SYNTAX array for\n\
         must have two variable names | TCL SYNTAX array for\n\
         \"nosuch\" isn't an array | TCL LOOKUP ARRAY nosuch\n\
         \"s\" isn't an array | TCL LOOKUP ARRAY s\n\
         wrong # args: should be \"array for {key value} arrayName script\" | TCL WRONGARGS\n\
         \"a(x)\" isn't an array | TCL LOOKUP ARRAY a(x)\n"
    );
}

#[test]
fn an_element_added_or_removed_during_the_walk_is_refused() {
    let out = run("array set a {x 1 y 2}\n\
         catch {array for {k v} a {unset a($k)}} m o\n\
         puts \"$m | [dict get $o -errorcode]\"\n\
         array set b {x 1}\n\
         catch {array for {k v} b {set b(new) 1}} m o\n\
         puts \"$m | [dict get $o -errorcode]\"\n\
         array set c {x 1 y 2}\n\
         array for {k v} c {set c($k) changed}\n\
         puts [lsort [array get c]]");
    assert_eq!(
        out,
        "array changed during iteration | TCL READ array for\n\
         array changed during iteration | TCL READ array for\n\
         changed changed x y\n"
    );
}
