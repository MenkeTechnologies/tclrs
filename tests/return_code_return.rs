//! `return -code return`, which `TclMergeReturnOptions`
//! (`generic/tclResult.c`) rewrites to `return -code ok` one level further
//! out: `-level 0` is a plain `return` from the procedure it is written in,
//! the default `-level 1` returns from that procedure's caller as well, and
//! the options a `catch` sees carry code `0`. Every expectation here was
//! produced by running the same script under tclsh 9.1.

fn run(script: &str) -> String {
    tclrs::eval(script)
        .unwrap_or_else(|e| panic!("script failed: {e}\n{script}"))
        .output
}

#[test]
fn level_zero_is_a_plain_return_from_the_procedure() {
    let out = run("proc a {} {return -code return -level 0 seven; return no}\n\
                   puts [a]\n\
                   proc i {} {foreach x {1 2 3} {if {$x == 2} {return -code return -level 0 found$x}}; return none}\n\
                   puts [i]\n\
                   proc j {} {eval {return -code return -level 0 ev}; return no}\n\
                   puts [j]");
    assert_eq!(out, "seven\nfound2\nev\n");
}

#[test]
fn the_default_level_returns_from_the_caller_too() {
    let out = run("proc b {} {return -code return eight}\n\
                   proc c {} {b; return no}\n\
                   puts [c]\n\
                   proc g {} {return -code return -level 1 ten}\n\
                   proc h {} {g; return no}\n\
                   puts [catch h r o]|$r|$o");
    assert_eq!(out, "eight\n0|ten|-code 0 -level 0\n");
}

#[test]
fn a_catch_sees_code_ok_one_level_further_out() {
    let out = run("proc b {} {return -code return eight}\n\
                   puts [catch b r o]|$r|$o\n\
                   puts [catch {return -code return -level 0 z} r o]|$r|$o");
    assert_eq!(out, "2|eight|-code 0 -level 1\n2|z|-code 0 -level 1\n");
}

#[test]
fn the_rewrite_applies_to_merged_and_computed_options() {
    let out = run("proc d {} {catch {return -code return 1} r o; return -options $o $r}\n\
                   proc e {} {d; return no}\n\
                   puts [e]\n\
                   proc f {} {return -options {-code return -level 0} nine; return no}\n\
                   puts [f]\n\
                   proc k {} {set c return; return -code $c -level 0 dyn; return no}\n\
                   puts [catch k r]|$r");
    assert_eq!(out, "1\nnine\n0|dyn\n");
}
