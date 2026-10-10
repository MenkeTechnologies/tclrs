//! Pinned answers for the back-reference, look-ahead and match-selection
//! rules of `crate::are_bt`. Each expectation is what tclsh 9.0 printed for the
//! program; they are checked against tclrs unconditionally, and against a 9.0
//! `tclsh` as well when one is on `PATH`.

mod golden;

use golden::{ok, Case};

const CASES: &[Case] = &[
    ok(
        "backref_repeat",
        "puts [regexp -inline {(a*)\\1} aaaaa]",
        "aaaa aa\n",
    ),
    ok(
        "backref_indices",
        "puts [regexp -inline -indices {(a+)\\1} aaaa]",
        "{0 3} {0 1}\n",
    ),
    ok(
        "backref_nocase",
        "puts [regexp -inline -nocase {(a)\\1} aA]",
        "aA a\n",
    ),
    ok(
        "backref_unset_group_fails",
        "puts [regexp -inline {(a)|b\\1} b]",
        "\n",
    ),
    ok(
        "backref_to_open_group_is_an_error",
        "puts [catch {regexp {(a\\1)} a} m]; puts $m",
        "1\ncannot compile regular expression pattern: invalid backreference number\n",
    ),
    ok(
        "backref_beyond_groups_is_an_error",
        "puts [catch {regexp {(a)\\2} aa} m]; puts $m",
        "1\ncannot compile regular expression pattern: invalid backreference number\n",
    ),
    ok(
        "backref_quantified_unset_fails",
        "puts [regexp -inline -indices {(a)?\\1?b} b]",
        "\n",
    ),
    ok(
        "backref_inside_noncapturing_quantifier",
        "puts [regexp -inline -indices {(a)?(?:\\1)?b} b]",
        "{0 0} {-1 -1}\n",
    ),
    ok(
        "backref_anchored_square",
        "puts [regexp -inline {^(a*)\\1$} aaaa]",
        "aaaa aa\n",
    ),
    ok(
        "backref_regsub_pairs",
        "puts [regsub -all {(.)\\1} aabbcd {<\\1>}]",
        "<a><b>cd\n",
    ),
    ok(
        "backref_all_inline",
        "puts [regexp -all -inline {(\\w)\\1} aabbccd]",
        "aa a bb b cc c\n",
    ),
    ok(
        "backref_lazy_group",
        "puts [regexp -inline {(a*?)\\1b} aab]",
        "aab a\n",
    ),
    ok(
        "lookahead_positive",
        "puts [regexp -inline {a(?=b)} ab]",
        "a\n",
    ),
    ok(
        "lookahead_negative",
        "puts [regexp -inline -indices {a(?!b)} abac]",
        "{2 2}\n",
    ),
    ok(
        "lookahead_is_zero_width_all",
        "puts [regexp -all -inline -indices {(?=a)} aaa]",
        "{0 -1} {1 0} {2 1}\n",
    ),
    ok(
        "lookahead_regsub",
        "puts [regsub -all {a(?=b)} abab X]",
        "XbXb\n",
    ),
    ok(
        "lookahead_direct_group_does_not_capture",
        "puts [regexp -inline {(?=(a))(a)} a]",
        "a a\n",
    ),
    ok(
        "lookahead_nested_group_is_numbered",
        "puts [regexp -inline -indices {(?=a((b)))(a)(b)} ab]",
        "{0 1} {-1 -1} {0 0} {1 1}\n",
    ),
    ok(
        "lookahead_backref_direct_is_an_error",
        "puts [catch {regexp {(a)(?=\\1)} aa} m]; puts $m",
        "1\ncannot compile regular expression pattern: invalid backreference number\n",
    ),
    ok(
        "lookahead_backref_nested_is_allowed",
        "puts [regexp -inline -indices {(a)(?=(?:\\1))} aa]",
        "{0 0} {0 0}\n",
    ),
    ok(
        "lookahead_quantifier_is_an_error",
        "puts [catch {regexp {(?=a)*b} b} m]; puts $m",
        "1\ncannot compile regular expression pattern: invalid quantifier operand\n",
    ),
    ok(
        "lookbehind_is_not_arc",
        "puts [catch {regexp {(?<=a)b} ab} m]; puts $m",
        "1\ncannot compile regular expression pattern: invalid quantifier operand\n",
    ),
    ok(
        "longest_alternation",
        "puts [regexp -inline {a|ab} ab]",
        "ab\n",
    ),
    ok(
        "longest_alternation_all",
        "puts [regexp -all -inline {a|ab} abab]",
        "ab ab\n",
    ),
    ok(
        "longest_alternation_empty_branch",
        "puts [regexp -inline {x*|y} y]",
        "y\n",
    ),
    ok(
        "longest_after_greedy_optional_group",
        "puts [regexp -inline {a*(ab)?} aab]",
        "aab ab\n",
    ),
    ok(
        "earlier_groups_take_the_longest_share",
        "puts [regexp -inline {(a|ab)(c|bcd)(d*)} abcd]",
        "abcd ab c d\n",
    ),
    ok(
        "plus_over_group_last_iteration",
        "puts [regexp -inline -indices {(a+)+} aaa]",
        "{0 2} {2 2}\n",
    ),
    ok(
        "bounded_iteration_last_copy",
        "puts [regexp -inline -indices {(a+){1,2}} aa]",
        "{0 1} {1 1}\n",
    ),
    ok(
        "star_over_group_single_iteration",
        "puts [regexp -inline -indices {(a+)*} aaa]",
        "{0 2} {0 2}\n",
    ),
    ok(
        "plus_over_nullable_group",
        "puts [regexp -inline -indices {(a*)+} aa]",
        "{0 1} {2 1}\n",
    ),
    ok(
        "star_over_nullable_group",
        "puts [regexp -inline -indices {(a*)*} aa]",
        "{0 1} {0 1}\n",
    ),
    ok(
        "lazy_iteration_shortest_last",
        "puts [regexp -inline -indices {(a+){2,}?b} aaaab]",
        "{0 4} {1 3}\n",
    ),
    ok(
        "lazy_leading_quantifier_whole",
        "puts [regexp -inline {a+?b*} aabb]",
        "a\n",
    ),
    ok(
        "exact_count_takes_atom_preference",
        "puts [regexp -inline {a{2}|b} aab]",
        "aa\n",
    ),
    ok(
        "quantified_group_zero_count",
        "puts [regexp -all -inline {(x){0}} bb]",
        "{} {} {} {}\n",
    ),
    ok(
        "removed_group_backref_is_an_error",
        "puts [catch {regexp {(a){0}\\1} a} m]; puts $m",
        "1\ncannot compile regular expression pattern: invalid backreference number\n",
    ),
    ok(
        "nested_group_survives_zero_count",
        "puts [catch {regexp {(?:(a)){0}\\1} a} m]; puts $m",
        "0\n0\n",
    ),
    ok(
        "text_start_survives_all_restart",
        "puts [regexp -all {\\Aa|^b} aab]",
        "2\n",
    ),
    ok(
        "text_start_regsub_all",
        "puts [regsub -all {\\Aa|^b} bbb X]",
        "Xbb\n",
    ),
    ok(
        "text_start_regsub_all_a",
        "puts [regsub -all {\\Aa|^b} aab X]",
        "XXb\n",
    ),
    ok(
        "caret_does_not_match_at_restart",
        "puts [regexp -all -inline {^a|b} aab]",
        "a b\n",
    ),
    ok(
        "word_boundary_alternation",
        "puts [regexp -all -inline {\\yab|b\\y} \"ab b ab\"]",
        "ab b ab\n",
    ),
    ok(
        "line_anchors_alternation",
        "puts [regexp -line -all -inline {^a|b$} \"a\\nab\\nb\"]",
        "a a b b\n",
    ),
];

#[test]
fn regexp_selection_and_backtracking_rules() {
    golden::check("regexp", CASES);
}
