//! A malformed procedure argument list is refused with the same wording by
//! `proc` and `apply`, and `apply` logs the lambda it was parsing in
//! `errorInfo`. Expectations are written from `proc(n)` and `apply(n)` and
//! checked against tclsh 9.0 by hand; no tclsh is needed to run them.

fn run(script: &str) -> String {
    tclrs::eval(script)
        .unwrap_or_else(|e| panic!("script failed: {e}\n{script}"))
        .output
}

#[test]
fn a_namespace_separator_in_a_formal_parameter_is_not_a_simple_name() {
    let out = run(
        "foreach spec {a::b ::a a:: a:::b args::x {{a::b 1}} a::b(1)} {\n\
           catch {proc zz $spec {}} m\n\
           puts $m\n\
         }",
    );
    assert_eq!(
        out,
        "formal parameter \"a::b\" is not a simple name\n\
         formal parameter \"::a\" is not a simple name\n\
         formal parameter \"a::\" is not a simple name\n\
         formal parameter \"a:::b\" is not a simple name\n\
         formal parameter \"args::x\" is not a simple name\n\
         formal parameter \"a::b\" is not a simple name\n\
         formal parameter \"a::b(1)\" is not a simple name\n"
    );
}

#[test]
fn an_array_element_is_named_before_a_separator_that_follows_its_parenthesis() {
    let out = run(
        "foreach spec {a(1) a(b::c) a( a) a:b} {\n\
           puts [catch {proc zz $spec {}} m]:$m\n\
         }",
    );
    assert_eq!(
        out,
        "1:formal parameter \"a(1)\" is an array element\n\
         1:formal parameter \"a(b::c)\" is an array element\n\
         0:\n\
         0:\n\
         0:\n"
    );
}

#[test]
fn apply_logs_the_lambda_it_could_not_parse() {
    let out = run(
        "set lambda [list {{a b c}} boo]\n\
         list [catch {apply $lambda} msg] $msg\n\
         puts [string map [list \\n |] $::errorInfo]",
    );
    assert_eq!(
        out,
        "too many fields in argument specifier \"a b c\"|    \
         (parsing lambda expression \"{{a b c}} boo\")|    \
         invoked from within|\"apply $lambda\"\n"
    );
}

#[test]
fn a_lambda_that_parses_but_fails_keeps_the_ordinary_trace() {
    let out = run(
        "catch {apply {{} {error foo}}}\n\
         puts [string map [list \\n |] $::errorInfo]",
    );
    assert_eq!(
        out,
        "foo|    while executing|\"error foo\"|    \
         (lambda term \"{} {error foo}\" line 1)|    \
         invoked from within|\"apply {{} {error foo}}\"\n"
    );
}
