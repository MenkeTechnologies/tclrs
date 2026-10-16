//! Frozen expectations for `lsort -integer` operand bounds and `lreverse` of an
//! empty list. The tclsh-differential suites carry the same programs but skip
//! on any tclsh other than 9.0.4; these always run.

fn run(program: &str) -> String {
    tclrs::eval(program).expect("program runs").output
}

/// An operand is a bignum when its mathematical value leaves the `i64` range,
/// whatever radix it is spelled in; `i64::MIN` and `i64::MAX` themselves fit.
#[test]
fn lsort_integer_refuses_every_spelling_past_i64() {
    let program = "foreach v {0x8000000000000000 0xffffffffffffffff 0o1000000000000000000000 \
                   0b1000000000000000000000000000000000000000000000000000000000000000 \
                   9223372036854775808 -9223372036854775809 0x10000000000000000} {
                       puts \"[catch {lsort -integer [list $v 1]} m]$m\"
                   }";
    let out = run(program);
    assert_eq!(
        out.lines().collect::<Vec<_>>(),
        vec!["1integer value too large to represent"; 7]
    );
}

#[test]
fn lsort_integer_accepts_the_i64_extremes() {
    assert_eq!(
        run("puts [lsort -integer {0x7fffffffffffffff -9223372036854775808 0}]"),
        "-9223372036854775808 0 0x7fffffffffffffff\n"
    );
}

/// An empty list is returned as the same object, so its whitespace survives;
/// a one-element list is rebuilt canonically.
#[test]
fn lreverse_of_empty_list_keeps_its_spelling() {
    assert_eq!(
        run("puts \"[lreverse { }]|[lreverse {}]|[lreverse { a }]|\""),
        " ||a|\n"
    );
}
