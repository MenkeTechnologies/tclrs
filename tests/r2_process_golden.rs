//! `exit`, `time` and `exec` against answers recorded from the reference interpreter. Runs without one: the pinned output is what is checked, and a 9.0.x tclsh on PATH is checked against it too.

mod golden;

use golden::{ok, Case};

#[test]
fn pinned() {
    golden::check(
        "r2_process_golden",
        &[
            Case { name: "exit_status_and_flush", script: "puts before\nexit 3\nputs after\n", stdout: "before\n", status: 3 },
            Case { name: "exit_not_caught", script: "puts [catch {exit 4} m]\nputs unreachable\n", stdout: "", status: 4 },
            ok("exit_default_zero", "puts hi\nexit\n", "hi\n"),
            ok("exit_bad_arg", "puts [catch {exit abc} m]; puts $m\nputs [catch {exit 1 2} m]; puts $m\n", "1\nexpected integer but got \"abc\"\n1\nwrong # args: should be \"exit ?returnCode?\"\n"),
            ok("exit_flushes_open_channel", "set f [open r2_exit_flush.tmp w]\nputs -nonewline $f unflushed\nexit 0\n", ""),
            ok("exit_flush_result", "set f [open r2_exit_flush.tmp]\nputs [read $f]\nclose $f\n", "unflushed\n"),
            ok("exit_proc_override", "proc exit {{c 0}} {puts \"exit $c\"}\nputs a\n", "a\nexit 0\n"),
            ok("time_integer_for_one", "puts [regexp {^\\d+ microseconds per iteration$} [time {set x 1}]]\nputs [regexp {^\\d+ microseconds per iteration$} [time {set x 1} 1]]\nputs [time {set x 1} 0]\nputs [time {set x 1} -3]\n", "1\n1\n0 microseconds per iteration\n0 microseconds per iteration\n"),
            ok("time_double_for_many", "puts [regexp {^[0-9.e+-]+ microseconds per iteration$} [time {set x 1} 7]]\n", "1\n"),
            ok("time_runs_count_times_at_caller_level", "set n 0\ntime {incr n} 12\nputs $n\nproc p {} {set k 0; time {incr k 2} 5; set k}\nputs [p]\nproc g {} {global gg; set gg 0; time {incr gg} 4}\ng\nputs $gg\n", "12\n10\n4\n"),
            ok("time_error_and_usage", "puts [catch {time {error boom} 3} m]; puts $m\nputs [catch {time} m]; puts $m\nputs [catch {time {x} abc} m]; puts $m\nputs [catch {time {set a 1} 1 2} m]; puts $m\n", "1\nboom\n1\nwrong # args: should be \"time command ?count?\"\n1\nexpected integer but got \"abc\"\n1\nwrong # args: should be \"time command ?count?\"\n"),
            ok("exec_basic", "puts [exec echo a b c]\nputs [exec echo hi | tr a-z A-Z]\nputs [exec -keepnewline echo hi]\nputs [string length [exec -keepnewline echo hi]]\nputs [exec sh -c {printf \"x\\n\\n\"}]|\nputs [exec cat << \"abc\\ndef\"]\n", "a b c\nHI\nhi\n\n3\nx\n|\nabc\ndef\n"),
            ok("exec_status_and_stderr", "puts [catch {exec false} m o]; puts $m\nputs [lindex [dict get $o -errorcode] 0]\nputs [lindex [dict get $o -errorcode] 2]\nputs [catch {exec sh -c {echo out; echo err >&2; exit 3}} m o]; puts $m\nset ec [dict get $o -errorcode]; puts [lindex $ec 0]; puts [lindex $ec 2]\n", "1\nchild process exited abnormally\nCHILDSTATUS\n1\n1\nout\nerr\nCHILDSTATUS\n3\n"),
        ],
    );
}
