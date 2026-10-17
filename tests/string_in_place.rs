//! `string length`, `index` and `range` read their string where it sits, by byte
//! for ASCII and by character otherwise (`read_in_place` in `src/cmd_string.rs`).
//! These pin the index arithmetic both paths must agree on, with the
//! expectations written from `string(n)`.

fn run(script: &str) -> String {
    tclrs::eval(script)
        .unwrap_or_else(|e| panic!("script failed: {e}\n{script}"))
        .output
}

#[test]
fn ascii_and_multibyte_strings_index_the_same_way() {
    let out = run("foreach s {hello h\\u00e9llo} {\n\
           puts [list [string length $s] [string index $s 1] [string index $s end] \
                 [string index $s end-1] [string index $s 9] [string index $s -1]]\n\
         }");
    assert_eq!(out, "5 e o l {} {}\n5 \u{e9} o l {} {}\n");
}

#[test]
fn range_clamps_and_empties_like_the_reference() {
    let out = run("foreach s {abcdef a\\u2603cdef} {\n\
           puts [list [string range $s 1 3] [string range $s -5 1] [string range $s 4 99] \
                 [string range $s 3 1] [string range $s end-2 end] [string range $s 2 2]]\n\
         }\n\
         puts <[string range {} 0 5]>");
    assert_eq!(
        out,
        "bcd ab ef {} def c\n\u{2603}cd a\u{2603} ef {} def c\n<>\n"
    );
}

#[test]
fn a_non_integer_index_is_refused_in_the_reference_wording() {
    let out = run("puts [catch {string index abc x} m]\nputs $m\n\
         puts [catch {string range abc 0 end-x} m]\nputs $m");
    assert_eq!(
        out,
        "1\nbad index \"x\": must be integer?[+-]integer? or end?[+-]integer?\n\
         1\nbad index \"end-x\": must be integer?[+-]integer? or end?[+-]integer?\n"
    );
}

#[test]
fn walking_a_long_string_by_index_does_not_slow_down_with_its_length() {
    let out = run("set s [string repeat abcdefgh 4000]\n\
         set n 0\n\
         for {set i 0} {$i < [string length $s]} {incr i} {\n\
           if {[string index $s $i] eq {a}} {incr n}\n\
         }\n\
         puts $n");
    assert_eq!(out, "4000\n");
}
