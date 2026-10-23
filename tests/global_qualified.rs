//! Built-in commands written with the global namespace's prefix — `::puts`,
//! `::string length`, `::tcl::prefix` — and the introspection form of
//! `namespace import`.
//!
//! `Tcl_FindCommand` (`generic/tclNamesp.c`) looks a name beginning with `::`
//! up in the global namespace, which is where every built-in command lives, so
//! `::puts hi` is `puts hi` in tclsh. Every expectation here was produced by
//! running the same script under tclsh 9.1; the one tclsh-order-dependent list
//! (`namespace import` walks a hash table) is `lsort`ed before it is compared.

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
fn a_builtin_is_reached_under_its_global_spelling() {
    let out = run("::puts hi\n\
                   ::puts [::string length abc]\n\
                   ::set x 3\n\
                   puts [::expr {$x + 1}]\n\
                   puts [::lindex {a b c} 1]\n\
                   puts [::tcl::prefix match {apple banana} ap]\n\
                   puts [::tcl::mathop::+ 1 2]");
    assert_eq!(out, "hi\n3\n4\nb\napple\n3\n");
}

#[test]
fn the_prefix_is_the_whole_run_of_leading_colons() {
    // `TclGetNamespaceForQualName` skips every colon after the first two.
    let out = run("::::puts four\n:::puts three");
    assert_eq!(out, "four\nthree\n");
}

#[test]
fn control_commands_and_proc_take_the_spelling_too() {
    let out = run("::proc f {a} {return [::string toupper $a]}\n\
                   puts [f q]\n\
                   puts [::f q]\n\
                   ::foreach i {1 2} {::if {$i == 2} {::puts two}}\n\
                   ::for {set i 0} {$i < 2} {incr i} {::puts $i}\n\
                   ::switch x {x {puts X} default {puts D}}\n\
                   puts [::catch {::error boom} m]\n\
                   puts $m\n\
                   puts [::info exists m]\n\
                   ::unset m\n\
                   puts [::info exists m]");
    assert_eq!(out, "Q\nQ\ntwo\n0\n1\nX\n1\nboom\n1\n0\n");
}

#[test]
fn a_procedure_defined_by_global_proc_is_known_to_the_prescan() {
    // The call is compiled before the `::proc` that defines the name, inside
    // a body — which is what the signature prescan exists for.
    let out = run("proc caller {} {return [g 2]}\n\
                   ::proc g {n} {return [expr {$n * 10}]}\n\
                   puts [caller]\n\
                   ::namespace eval ns {::proc h {} {return H}}\n\
                   puts [ns::h]");
    assert_eq!(out, "20\nH\n");
}

#[test]
fn inside_a_namespace_the_global_spelling_is_never_the_namespaces_own() {
    let out = run("namespace eval ns {\n\
                     proc string {args} {return NS}\n\
                     puts [::string length abc]\n\
                     ::puts [::namespace current]\n\
                   }");
    assert_eq!(out, "3\n::ns\n");
}

#[test]
fn the_spelling_reaches_the_command_through_a_computed_name_and_expansion() {
    let out = run("set c ::puts\n\
                   $c hi\n\
                   set s ::string\n\
                   puts [$s length abcd]\n\
                   {*}{::puts expanded}\n\
                   set l [list ::lindex {a b c} 2]\n\
                   puts [{*}$l]\n\
                   proc p {} {::puts inproc; ::set x 4; return [::expr {$x * 2}]}\n\
                   puts [p]");
    assert_eq!(out, "hi\n4\nexpanded\nc\ninproc\n8\n");
}

#[test]
fn wrong_number_of_arguments_names_the_command_as_written() {
    // `Tcl_WrongNumArgs` spells the command as `objv[0]` has it.
    let out = outcomes(&[
        "::puts a b c d",
        "::string length",
        "::lindex",
        "::incr",
        "::tcl::prefix",
        // Written bare, the message is bare.
        "puts a b c d",
    ]);
    assert_eq!(
        out,
        "1 <wrong # args: should be \"::puts ?-nonewline? ?channel? string\">\n\
         1 <wrong # args: should be \"::string length string\">\n\
         1 <wrong # args: should be \"::lindex list ?index ...?\">\n\
         1 <wrong # args: should be \"::incr varName ?increment?\">\n\
         1 <wrong # args: should be \"::tcl::prefix subcommand ?arg ...?\">\n\
         1 <wrong # args: should be \"puts ?-nonewline? ?channel? string\">\n"
    );
}

#[test]
fn a_body_owner_written_with_the_prefix_still_owns_its_body_in_errorinfo() {
    let out = run("catch {::foreach x {1} {error inside}} m o\n\
                   puts [dict get $o -errorinfo]\n\
                   catch {::if {bad} {}} m o\n\
                   puts [dict get $o -errorinfo]");
    assert_eq!(
        out,
        "inside\n    while executing\n\"error inside\"\n    (\"foreach\" body line 1)\n    invoked from within\n\"::foreach x {1} {error inside}\"\n\
         invalid bareword \"bad\"\nin expression \"bad\";\nshould be \"$bad\" or \"{bad}\" or \"bad(...)\" or ...\n    (parsing expression \"bad\")\n    invoked from within\n\"::if {bad} {}\"\n"
    );
}

#[test]
fn a_name_that_is_not_a_global_command_is_still_invalid() {
    let out = outcomes(&[
        "::nosuch",
        "::ns::nosuch",
        "namespace eval ns {}; ::ns::string length a",
    ]);
    assert_eq!(
        out,
        "1 <invalid command name \"::nosuch\">\n\
         1 <invalid command name \"::ns::nosuch\">\n\
         1 <invalid command name \"::ns::string\">\n"
    );
}

#[test]
fn proc_with_the_global_prefix_defines_the_command_the_bare_name_reaches() {
    // `proc ::lindex` replaces the command a bare `lindex` reaches, and
    // `::::g` is `g`; `info` answers for the bare name.
    let out = run("proc ::lindex {args} {return mine}\n\
                   puts [lindex a b]\n\
                   puts [::lindex a b]\n\
                   proc ::::g {} {return G}\n\
                   puts [g]\n\
                   proc ::f {a {b 2}} {return $a$b}\n\
                   puts [f 1]\n\
                   puts [info procs f]\n\
                   puts [info body f]\n\
                   puts [info args f]\n\
                   puts [namespace which f]\n\
                   namespace eval a {}\n\
                   proc ::a::b {} {return B}\n\
                   namespace eval a {puts [b]}\n\
                   puts [info procs ::a::*]");
    assert_eq!(
        out,
        "mine\nmine\nG\n12\nf\nreturn $a$b\na b\n::f\nB\n::a::b\n"
    );
}

#[test]
fn redefining_a_builtin_under_the_global_prefix_is_refused_as_the_bare_name_is() {
    // A built-in this compiler lowers itself cannot be replaced; the global
    // spelling is the same command, so it is refused with the same message
    // rather than defining a procedure only `::puts` would reach.
    let err = tclrs::eval("proc ::puts {args} {return redef}").unwrap_err();
    assert!(
        err.to_string()
            .contains("redefining the built-in command \"puts\" is not supported"),
        "{err}"
    );
}

#[test]
fn namespace_import_with_no_pattern_lists_the_imported_commands() {
    let out = run("puts <[namespace import]>\n\
                   namespace eval ns {namespace export g h; proc g {} {}; proc h {} {}}\n\
                   namespace import ns::g\n\
                   rename g gg\n\
                   proc x {} {}\n\
                   rename x y\n\
                   puts [lsort [namespace import]]\n\
                   namespace eval ns2 {namespace import ::ns::*; puts [lsort [namespace import]]}\n\
                   puts <[namespace import ::ns::*]>\n\
                   puts [lsort [namespace import]]\n\
                   namespace forget ::ns::h\n\
                   puts [lsort [namespace import]]");
    // A renamed import is still an import; a renamed procedure never was one.
    assert_eq!(out, "<>\ngg\ng h\n<>\ng gg h\ng gg\n");
}

#[test]
fn the_mathop_operators_are_listed_once_imported() {
    let out = run("namespace import ::tcl::mathop::*\nputs [lsort [namespace import]]");
    assert_eq!(
        out,
        "! != % & * ** + - / < << <= == > >= >> ^ eq ge gt in le lt ne ni | ~\n"
    );
}

#[test]
fn a_rename_moves_a_commands_origin_but_not_an_imports() {
    let out = run("proc x {} {}\n\
                   rename x y\n\
                   puts [namespace origin y]\n\
                   namespace eval a {namespace export one; proc one {} {return 1}}\n\
                   namespace eval b {\n\
                     namespace import ::a::one\n\
                     puts [namespace import]\n\
                     rename one uno\n\
                     puts [namespace import]\n\
                     puts [namespace origin uno]\n\
                   }");
    assert_eq!(out, "::y\none\nuno\n::a::one\n");
}
