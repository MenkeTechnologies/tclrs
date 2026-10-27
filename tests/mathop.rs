//! The `tcl::mathop` commands. Every expectation here was produced by running
//! the same script under tclsh, so the identities, the argument shapes and the
//! wording of each refusal are the reference's: the four `objProc`s of
//! `generic/tclBasic.c`'s `mathOpCmds` table, as described in
//! `src/cmd_mathop.rs`.
//!
//! One wording is pinned to tclsh 9.0.4's source rather than to the 9.1 binary
//! this was measured against: which operand `+` names when it refuses one. In
//! 9.0.4 `INST_ADD` (`generic/tclExecute.c`) reports the operand under the top
//! of the stack as `left` and the top as `right`, so `+ 1 abc` is a *right*
//! operand refusal, as `expr {1 + "abc"}` is in both releases and as `- 1 abc`
//! is in both; tclsh 9.1 says `left` for `+` alone.

fn run(script: &str) -> String {
    tclrs::eval(script)
        .unwrap_or_else(|e| panic!("script failed: {e}\n{script}"))
        .output
}

/// Run each `command` under `catch` and print `code <result>` for it.
fn outcomes(commands: &[&str]) -> String {
    let mut script = String::new();
    for command in commands {
        script.push_str(&format!(
            "set c [catch {{{command}}} m]\nputs \"$c <$m>\"\n"
        ));
    }
    run(&script)
}

#[test]
fn variadic_operators_answer_their_identity_with_no_argument() {
    let out = outcomes(&[
        "::tcl::mathop::+",
        "::tcl::mathop::*",
        "::tcl::mathop::&",
        "::tcl::mathop::|",
        "::tcl::mathop::^",
        "::tcl::mathop::**",
    ]);
    assert_eq!(out, "0 <0>\n0 <1>\n0 <-1>\n0 <0>\n0 <0>\n0 <1>\n");
}

#[test]
fn a_lone_argument_is_combined_with_the_identity() {
    let out = outcomes(&[
        // `0 + 0x10` reads the operand as a number.
        "::tcl::mathop::+ 0x10",
        "::tcl::mathop::+ 1.50",
        "::tcl::mathop::+ 1e3",
        "::tcl::mathop::* 1e3",
        "::tcl::mathop::& 12",
        // `**`'s identity is on the exponent side, and `x ** 1` keeps an
        // integer's spelling exactly as `expr {007 ** 1}` does.
        "::tcl::mathop::** 007",
        "::tcl::mathop::** 2",
        // `-` negates and `/` is one over, with the double 1.0.
        "::tcl::mathop::- 5",
        "::tcl::mathop::/ 2",
    ]);
    assert_eq!(
        out,
        "0 <16>\n0 <1.5>\n0 <1000.0>\n0 <1000.0>\n0 <12>\n0 <007>\n0 <2>\n0 <-5>\n0 <0.5>\n"
    );
}

#[test]
fn folds_group_from_the_left_except_exponentiation() {
    let out = outcomes(&[
        "::tcl::mathop::+ 1 2 3 4 5 6 7 8 9 10",
        "::tcl::mathop::* 2 3 4",
        "::tcl::mathop::- 10 3 2",
        "::tcl::mathop::/ 7 2",
        "::tcl::mathop::/ 7.0 2",
        "::tcl::mathop::/ 1 2 3.0",
        "::tcl::mathop::& 12 10",
        "::tcl::mathop::| 1 2 4",
        "::tcl::mathop::^ 1 3",
        // 2 ** (3 ** 2), not (2 ** 3) ** 2.
        "::tcl::mathop::** 2 3 2",
        "::tcl::mathop::** 2 -1",
        "::tcl::mathop::** 2.0 -1",
        // A double anywhere makes the answer a double.
        "::tcl::mathop::+ 1 2 3.0",
        "::tcl::mathop::- 1 2 3.0",
        "::tcl::mathop::+ 1 1e3",
    ]);
    assert_eq!(
        out,
        "0 <55>\n0 <24>\n0 <5>\n0 <3>\n0 <3.5>\n0 <0.0>\n0 <8>\n0 <7>\n0 <2>\n\
         0 <512>\n0 <0>\n0 <0.5>\n0 <6.0>\n0 <-4.0>\n0 <1001.0>\n"
    );
}

#[test]
fn integers_promote_past_the_machine_word_and_doubles_overflow_to_inf() {
    let out = outcomes(&[
        "::tcl::mathop::+ 9223372036854775807 1",
        "::tcl::mathop::* 4294967296 4294967296",
        "::tcl::mathop::- -9223372036854775808 1",
        "::tcl::mathop::* 1e308 10",
        "::tcl::mathop::** 2 100",
    ]);
    assert_eq!(
        out,
        "0 <9223372036854775808>\n0 <18446744073709551616>\n0 <-9223372036854775809>\n\
         0 <Inf>\n0 <1267650600228229401496703205376>\n"
    );
}

#[test]
fn sorting_operators_chain_over_adjacent_pairs_and_stop_at_the_deciding_one() {
    let out = outcomes(&[
        "::tcl::mathop::<",
        "::tcl::mathop::< 1",
        "::tcl::mathop::< 1 2 3",
        "::tcl::mathop::< 1 3 2",
        // The first pair decides, so `x` is never read.
        "::tcl::mathop::< 2 1 x",
        "::tcl::mathop::> 1 2 abc",
        // `2 < x` is a string comparison once a side is not a number.
        "::tcl::mathop::< 1 2 x",
        "::tcl::mathop::> 3 2 abc",
        "::tcl::mathop::< a b",
        "::tcl::mathop::== 1 1.0 1",
        "::tcl::mathop::== 1 abc 1",
        "::tcl::mathop::ge 3 2 2",
        "::tcl::mathop::<= 1 1 2",
        "::tcl::mathop::>= 2 2 3",
    ]);
    assert_eq!(
        out,
        "0 <1>\n0 <1>\n0 <1>\n0 <0>\n0 <0>\n0 <0>\n0 <1>\n0 <0>\n0 <1>\n0 <1>\n0 <0>\n0 <1>\n0 <1>\n0 <0>\n"
    );
}

#[test]
fn the_string_operators_compare_strings_and_the_numeric_ones_numbers() {
    let out = outcomes(&[
        "::tcl::mathop::eq 1 1.0",
        "::tcl::mathop::eq a a a",
        "::tcl::mathop::eq 1e3 1000.0",
        "::tcl::mathop::lt a b c",
        "::tcl::mathop::!= 1 1.0",
        "::tcl::mathop::ne 1 1.0",
        "::tcl::mathop::in a {a b}",
        "::tcl::mathop::ni a {a b}",
        // Membership is a string test: `1` is not `01`.
        "::tcl::mathop::in 1 {01}",
        "::tcl::mathop::in 1.0 {1.0}",
        "::tcl::mathop::in a \"\\{\"",
    ]);
    assert_eq!(
        out,
        "0 <0>\n0 <1>\n0 <0>\n0 <1>\n0 <0>\n0 <1>\n0 <1>\n0 <0>\n0 <0>\n0 <1>\n\
         1 <unmatched open brace in list>\n"
    );
}

#[test]
fn the_single_operators_take_exactly_their_count() {
    let out = outcomes(&[
        "::tcl::mathop::% 7 -3",
        "::tcl::mathop::<< 1 4",
        "::tcl::mathop::>> 16 2",
        "::tcl::mathop::<< 1 -1",
        "::tcl::mathop::~ 5",
        "::tcl::mathop::! 0",
        "::tcl::mathop::! yes",
        "::tcl::mathop::! 0.0",
        "::tcl::mathop::~",
        "::tcl::mathop::!",
        "::tcl::mathop::% 1",
        "::tcl::mathop::<< 1",
        "::tcl::mathop::!= 1",
        "::tcl::mathop::ne 1",
        "::tcl::mathop::in 1",
        "::tcl::mathop::ni 1 2 3",
        "::tcl::mathop::-",
        "::tcl::mathop::/",
    ]);
    assert_eq!(
        out,
        "0 <-2>\n0 <16>\n0 <4>\n1 <negative shift argument>\n0 <-6>\n0 <1>\n0 <0>\n0 <1>\n\
         1 <wrong # args: should be \"::tcl::mathop::~ integer\">\n\
         1 <wrong # args: should be \"::tcl::mathop::! boolean\">\n\
         1 <wrong # args: should be \"::tcl::mathop::% integer integer\">\n\
         1 <wrong # args: should be \"::tcl::mathop::<< integer shift\">\n\
         1 <wrong # args: should be \"::tcl::mathop::!= value value\">\n\
         1 <wrong # args: should be \"::tcl::mathop::ne value value\">\n\
         1 <wrong # args: should be \"::tcl::mathop::in value list\">\n\
         1 <wrong # args: should be \"::tcl::mathop::ni value list\">\n\
         1 <wrong # args: should be \"::tcl::mathop::- value ?value ...?\">\n\
         1 <wrong # args: should be \"::tcl::mathop::/ value ?value ...?\">\n"
    );
}

#[test]
fn operands_are_refused_in_exprs_words() {
    let out = outcomes(&[
        "::tcl::mathop::+ abc",
        "::tcl::mathop::+ 1 abc",
        "::tcl::mathop::+ \"1 2\" 3",
        "::tcl::mathop::- 1 abc",
        "::tcl::mathop::+ nan 1",
        "::tcl::mathop::% 7.5 2",
        "::tcl::mathop::& 1.5 1",
        "::tcl::mathop::~ 1.5",
        "::tcl::mathop::! abc",
        "::tcl::mathop::/ 1 0",
        "::tcl::mathop::** 2 abc",
    ]);
    assert_eq!(
        out,
        "1 <cannot use non-numeric string \"abc\" as right operand of \"+\">\n\
         1 <cannot use non-numeric string \"abc\" as right operand of \"+\">\n\
         1 <cannot use a list as left operand of \"+\">\n\
         1 <cannot use non-numeric string \"abc\" as right operand of \"-\">\n\
         1 <cannot use non-numeric floating-point value \"nan\" as left operand of \"+\">\n\
         1 <cannot use floating-point value \"7.5\" as left operand of \"%\">\n\
         1 <cannot use floating-point value \"1.5\" as left operand of \"&\">\n\
         1 <cannot use floating-point value \"1.5\" as operand of \"~\">\n\
         1 <cannot use non-numeric string \"abc\" as operand of \"!\">\n\
         1 <divide by zero>\n\
         1 <cannot use non-numeric string \"abc\" as right operand of \"**\">\n"
    );
}

#[test]
fn every_word_is_evaluated_before_any_arithmetic_runs() {
    // tclsh substitutes all of a command's words first, so the `puts` runs and
    // only then is `abc` refused.
    let out = run("puts [catch {::tcl::mathop::+ abc [puts hi]} m]\nputs $m\n\
                   puts [catch {::tcl::mathop::< [puts a; expr 2] [puts b; expr 1] [puts c; expr 3]} m]\nputs $m");
    assert_eq!(
        out,
        "hi\n1\ncannot use non-numeric string \"abc\" as left operand of \"+\"\na\nb\nc\n0\n0\n"
    );
}

#[test]
fn both_spellings_a_computed_name_and_expansion_reach_the_operator() {
    let out = run("puts [tcl::mathop::+ 1 2]\n\
                   puts [::tcl::mathop::* 2 3]\n\
                   puts [[list ::tcl::mathop::*] 3 4]\n\
                   set op tcl::mathop::-\n\
                   puts [$op 10 4]\n\
                   puts [::tcl::mathop::+ {*}[lseq 1 300]]\n\
                   puts [{*}{::tcl::mathop::** 2} 10]\n\
                   proc sum {args} {::tcl::mathop::+ {*}$args}\n\
                   puts [sum 1 2 3.5]\n\
                   puts [sum]");
    assert_eq!(out, "3\n6\n12\n6\n45150\n1024\n6.5\n0\n");
}

#[test]
fn the_namespace_exists_exports_everything_and_lists_its_commands() {
    let out = run("puts [namespace exists ::tcl::mathop]\n\
                   puts [namespace children ::tcl]\n\
                   puts [namespace eval ::tcl::mathop {namespace export}]\n\
                   puts [lsort [info commands ::tcl::mathop::*]]\n\
                   puts [info commands ::tcl::mathop::+]\n\
                   puts [llength [info commands ::tcl::mathop::*]]\n\
                   puts [lsearch [info commands] +]");
    assert_eq!(
        out,
        "1\n::tcl::mathop\n*\n\
         ::tcl::mathop::! ::tcl::mathop::!= ::tcl::mathop::% ::tcl::mathop::& ::tcl::mathop::* \
         ::tcl::mathop::** ::tcl::mathop::+ ::tcl::mathop::- ::tcl::mathop::/ ::tcl::mathop::< \
         ::tcl::mathop::<< ::tcl::mathop::<= ::tcl::mathop::== ::tcl::mathop::> ::tcl::mathop::>= \
         ::tcl::mathop::>> ::tcl::mathop::^ ::tcl::mathop::eq ::tcl::mathop::ge ::tcl::mathop::gt \
         ::tcl::mathop::in ::tcl::mathop::le ::tcl::mathop::lt ::tcl::mathop::ne ::tcl::mathop::ni \
         ::tcl::mathop::| ::tcl::mathop::~\n\
         ::tcl::mathop::+\n27\n-1\n"
    );
}

#[test]
fn namespace_import_brings_the_bare_operators_in() {
    let out = run("namespace import ::tcl::mathop::*\n\
                   puts [+ 1 2 3]\n\
                   puts [* {*}[lseq 1 10]]\n\
                   puts [lmap x {1 2 3} {* $x $x}]\n\
                   puts [== 1 1.0 0x1]\n\
                   puts [in b {a b c}]\n\
                   puts [namespace which +]\n\
                   puts [namespace origin +]\n\
                   puts [info commands +]\n\
                   puts [catch {-} m]\nputs $m\n\
                   puts [catch {!= 1} m]\nputs $m\n\
                   set op -\n\
                   puts [$op 10 4]\n\
                   puts [catch {$op} m]\nputs $m\n\
                   proc sum {args} {+ {*}$args}\n\
                   puts [sum 1 2 3.5]");
    assert_eq!(
        out,
        "6\n3628800\n1 4 9\n1\n1\n::+\n::tcl::mathop::+\n+\n\
         1\nwrong # args: should be \"- value ?value ...?\"\n\
         1\nwrong # args: should be \"!= value value\"\n\
         6\n1\nwrong # args: should be \"- value ?value ...?\"\n6.5\n"
    );
}

#[test]
fn an_import_into_a_namespace_and_a_forget_follow_the_registry() {
    let out = run("namespace eval ns {\n\
                       namespace import ::tcl::mathop::+\n\
                       puts [+ 2 2]\n\
                       puts [namespace which +]\n\
                   }\n\
                   puts [info commands ::ns::+]\n\
                   puts [catch {+ 1 2} m]\nputs $m\n\
                   namespace import ::tcl::mathop::*\n\
                   puts [+ 1 2]\n\
                   namespace forget ::tcl::mathop::*\n\
                   puts [catch {+ 1 2} m]\nputs $m");
    assert_eq!(
        out,
        "4\n::ns::+\n::ns::+\n1\ninvalid command name \"+\"\n3\n1\ninvalid command name \"+\"\n"
    );
}

#[test]
fn a_procedure_of_the_same_bare_name_is_not_shadowed() {
    // `+` is only an operator once imported; a script's own `+` is its own.
    let out =
        run("proc + {a b} {return \"$a plus $b\"}\nputs [+ 1 2]\nputs [::tcl::mathop::+ 1 2]");
    assert_eq!(out, "1 plus 2\n3\n");
}
