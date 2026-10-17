//! `tcl::prefix match`, `all` and `longest`. Every expectation was produced by
//! running the same script under tclsh 9.0, so the wording of each error and
//! the corner cases of abbreviation (an exact entry beating longer ones, the
//! empty string abbreviating nothing alone) are the reference's.

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
fn match_takes_an_exact_entry_or_a_unique_abbreviation() {
    let out = outcomes(&[
        "tcl::prefix match {apple apricot banana} ban",
        "tcl::prefix match {apple apricot banana} app",
        "tcl::prefix match {abc abcd} abc",
        "tcl::prefix match {ab ac a} a",
        "tcl::prefix match {a a} a",
        "tcl::prefix match {{a b} c} a",
        "tcl::prefix match -exact {abc abcd} abc",
    ]);
    assert_eq!(
        out,
        "0 <banana>\n0 <apple>\n0 <abc>\n0 <a>\n0 <a>\n0 <a b>\n0 <abc>\n"
    );
}

#[test]
fn match_failures_are_worded_like_get_index_from_obj() {
    let out = outcomes(&[
        "tcl::prefix match {apple apricot banana} x",
        "tcl::prefix match {apple apricot banana} ap",
        "tcl::prefix match {ab ac} a",
        "tcl::prefix match {ab ab} a",
        "tcl::prefix match {} x",
        "tcl::prefix match -exact {abc abcd} ab",
        "tcl::prefix match -message fruit {apple apricot} ap",
        "tcl::prefix match -message {an option} {foo} baz",
    ]);
    assert_eq!(
        out,
        "1 <bad option \"x\": must be apple, apricot, or banana>\n\
         1 <ambiguous option \"ap\": must be apple, apricot, or banana>\n\
         1 <ambiguous option \"a\": must be ab or ac>\n\
         1 <ambiguous option \"a\": must be ab or ab>\n\
         1 <bad option \"x\": no valid options>\n\
         1 <bad option \"ab\": must be abc or abcd>\n\
         1 <ambiguous fruit \"ap\": must be apple or apricot>\n\
         1 <bad an option \"baz\": must be foo>\n"
    );
}

#[test]
fn the_empty_string_is_never_an_abbreviation_of_a_single_entry() {
    let out = outcomes(&[
        "tcl::prefix match {a b c} {}",
        "tcl::prefix match {a} {}",
        "tcl::prefix match {a {}} {}",
        "tcl::prefix match {{} a b} {}",
    ]);
    assert_eq!(
        out,
        "1 <ambiguous option \"\": must be a, b, or c>\n\
         1 <bad option \"\": must be a>\n\
         0 <>\n\
         0 <>\n"
    );
}

#[test]
fn options_may_be_abbreviated_and_are_checked_before_the_table() {
    let out = outcomes(&[
        "tcl::prefix match -ex {a ab} a",
        "tcl::prefix match -m msg {a} x",
        "tcl::prefix match -e a x",
        "tcl::prefix match -- abc ab",
        "tcl::prefix match -message {a b} x",
        "tcl::prefix match {a b c}",
        "tcl::prefix match",
    ]);
    assert_eq!(
        out,
        "0 <a>\n\
         1 <bad msg \"x\": must be a>\n\
         1 <ambiguous option \"-e\": must be -error, -exact, or -message>\n\
         1 <bad option \"--\": must be -error, -exact, or -message>\n\
         1 <missing value for -message>\n\
         1 <wrong # args: should be \"tcl::prefix match ?options? table string\">\n\
         1 <wrong # args: should be \"tcl::prefix match ?options? table string\">\n"
    );
}

#[test]
fn error_option_chooses_what_a_failed_match_does() {
    let out = run(
        "puts \"[catch {tcl::prefix match -error {} {apple} x} m] <$m>\"\n\
         catch {tcl::prefix match -error {-level 1} {a} x} m o\n\
         puts [list $m [dict get $o -code] [dict get $o -level] [dict get $o -errorcode]]\n\
         catch {tcl::prefix match -error {-errorcode {A B} -level 2} {a} x} m o\n\
         puts [list [dict get $o -level] [dict get $o -errorcode]]\n\
         proc p {} {tcl::prefix match -error {-level 1} {a} x}\n\
         puts \"[catch p m] <$m>\"\n\
         puts \"[catch {tcl::prefix match -error -level {a} a} m] <$m>\"\n\
         puts \"[catch {tcl::prefix match -error {-level x} {a} a} m] <$m>\"\n\
         puts \"[catch {tcl::prefix match -error {-level x} {a} b} m] <$m>\"",
    );
    assert_eq!(
        out,
        "0 <>\n\
         {bad option \"x\": must be a} 1 1 NONE\n\
         2 {A B}\n\
         1 <bad option \"x\": must be a>\n\
         1 <error options must have an even number of elements>\n\
         0 <a>\n\
         1 <bad -level value: expected non-negative integer but got \"x\">\n"
    );
}

#[test]
fn all_and_longest_list_and_intersect_the_entries_a_string_begins() {
    let out = outcomes(&[
        "tcl::prefix all {a ab abc b} a",
        "tcl::prefix all {a b} {}",
        "tcl::prefix all {{a b} c} a",
        "tcl::prefix all {} a",
        "tcl::prefix longest {abc abd} a",
        "tcl::prefix longest {abc abd} abc",
        "tcl::prefix longest {abc abd} x",
        "tcl::prefix longest {a\\u00e9x a\\u00e9y} a",
        "tcl::prefix longest {abc ABD} a",
        "tcl::prefix all {a}",
        "tcl::prefix longest {a b} a b",
    ]);
    assert_eq!(
        out,
        "0 <a ab abc>\n\
         0 <a b>\n\
         0 <{a b}>\n\
         0 <>\n\
         0 <ab>\n\
         0 <abc>\n\
         0 <>\n\
         0 <a\u{e9}>\n\
         0 <abc>\n\
         1 <wrong # args: should be \"tcl::prefix all table string\">\n\
         1 <wrong # args: should be \"tcl::prefix longest table string\">\n"
    );
}

#[test]
fn subcommands_may_be_abbreviated_and_an_unknown_one_is_refused() {
    let out = outcomes(&[
        "tcl::prefix m {a} a",
        "tcl::prefix lo {a} a",
        "tcl::prefix bogus",
        "tcl::prefix",
    ]);
    assert_eq!(
        out,
        "0 <a>\n\
         0 <a>\n\
         1 <unknown or ambiguous subcommand \"bogus\": must be all, longest, or match>\n\
         1 <wrong # args: should be \"tcl::prefix subcommand ?arg ...?\">\n"
    );
}
