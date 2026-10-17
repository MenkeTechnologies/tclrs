//! `coroutine name apply {params body} ?arg ...?` with the lambda written out
//! (`Compiler::coroutine_lambda` in `src/coro.rs`). The lambda becomes a
//! procedure under a name no script can type, so everything a coroutine of a
//! procedure does has to hold for it. Expectations are written from
//! `coroutine(n)` and were checked against tclsh 9.0 by hand.

fn run(script: &str) -> String {
    tclrs::eval(script)
        .unwrap_or_else(|e| panic!("script failed: {e}\n{script}"))
        .output
}

#[test]
fn a_generator_written_as_a_lambda_runs_to_completion() {
    let out = run("coroutine gen apply {{n} {\n\
           for {set i 0} {$i < $n} {incr i} {yield $i}\n\
           return done\n\
         }} 3\n\
         puts [gen]\nputs [gen]\nputs [gen]\n\
         puts [catch {gen} m]:$m");
    assert_eq!(out, "1\n2\ndone\n1:invalid command name \"gen\"\n");
}

#[test]
fn defaults_and_args_of_the_lambda_are_arranged_like_a_procedure_call() {
    let out = run(
        "puts [coroutine c apply {{a {b 5} args} {yield \"$a|$b|$args\"}} 1]\n\
         puts [coroutine d apply {{a {b 5} args} {yield \"$a|$b|$args\"}} 1 2 3 4]",
    );
    assert_eq!(out, "1|5|\n1|2|3 4\n");
}

#[test]
fn a_value_sent_in_on_resume_is_the_result_of_yield() {
    let out = run(
        "coroutine c apply {{x} {set y [yield $x]; yield \"got $y\"}} first\n\
         puts [c reply]\n\
         puts [c]",
    );
    assert_eq!(out, "got reply\n\n");
}

#[test]
fn the_lambda_is_not_a_command_the_script_can_see() {
    let out = run("coroutine c apply {{} {yield 1; yield 2}}\n\
         puts [lsearch -glob [info commands] *lambda*]\n\
         puts [lsearch -glob [info procs] *lambda*]\n\
         puts [c]");
    assert_eq!(out, "-1\n-1\n2\n");
}

#[test]
fn a_lambda_with_the_wrong_argument_count_is_refused_like_a_procedure() {
    let err = tclrs::eval("coroutine c apply {{x} {yield}}").expect_err("one argument missing");
    assert!(
        err.starts_with("wrong # args: should be \"apply lambdaExpr x\""),
        "unexpected: {err}"
    );
}
