//! `info level N` and `info frame` against answers recorded from the reference interpreter.

mod golden;

use golden::ok;

#[test]
fn pinned() {
    golden::check(
        "r2_frame_golden",
        &[
            ok("info_level_number_words", "proc t {args} {info level 0}\nputs [t a {b c} \"d e\"]\nproc u {{a 1} {b 2}} {info level 0}\nputs [u]\nputs [u 5]\nproc r {} {info level 1}\nputs [r]\nproc r2 {y} {info level 1}\nproc s {x} {r2 $x}\nputs [s 5]\nproc v {} {lindex [info level 0] 0}\nputs [v]\nnamespace eval ns {proc w {} {info level 0}}\nputs [ns::w]\nputs [apply {{x} {info level 0}} 5]\nputs [catch {info level 5} m]; puts $m\nputs [catch {info level -5} m]; puts $m\nputs [catch {info level} m]; puts $m\nproc up {} {info level -1}\nproc caller {z} {up}\nputs [caller 7]\n", "t a {b c} {d e}\nu\nu 5\nr\ns 5\nv\nns::w\napply {{x} {info level 0}} 5\n1\nbad level \"5\"\n1\nbad level \"-5\"\n0\n0\ncaller 7\n"),
            ok("info_frame_counts_and_dicts", "puts [info frame]\nproc p {a} {\n  set n [info frame]\n  set top [info frame 1]\n  set cur [info frame 0]\n  list $n [dict get $top cmd] [dict get $top level] [dict get $cur proc] [dict get $cur level] [dict get $cur type]\n}\nputs [p 1]\nproc q {} {p x}\nputs [q]\nputs [catch {info frame 99} m]; puts $m\nputs [catch {info frame -9} m]; puts $m\nproc cmdline {} {dict get [info frame 0] cmd}\nputs [cmdline]\nputs [dict get [info frame 0] cmd]\n", "1\n2 {p 1} 1 ::p 0 source\n3 q 2 ::p 0 source\n1\nbad level \"99\"\n1\nbad level \"-9\"\ninfo frame 0\ninfo frame 0\n"),
        ],
    );
}
