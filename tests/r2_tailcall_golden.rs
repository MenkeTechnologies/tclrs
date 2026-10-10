//! `tailcall` against answers recorded from the reference interpreter.

mod golden;

use golden::{ok, Case};

#[test]
fn pinned() {
    golden::check(
        "r2_tailcall_golden",
        &[
            ok("tailcall_replaces_the_activation", "proc g {args} {return \"g $args\"}\nproc f {} {tailcall g 1 2}\nputs [f]\nproc loop {n acc} {if {$n == 0} {return $acc}; tailcall loop [expr {$n-1}] [expr {$acc+$n}]}\nputs [loop 50000 0]\n", "g 1 2\n1250025000\n"),
            ok("tailcall_runs_the_command_at_the_callers_level", "proc f {} {tailcall set x 5}\nputs [f]\nputs [info exists x]\nproc g {} {info level}\nproc h {} {set a [info level]; tailcall g}\nputs [h]\n", "5\n1\n1\n"),
            ok("tailcall_outside_a_proc_and_without_a_command", "puts [catch {tailcall puts hi} m]; puts $m\nproc f {} {tailcall nosuch 1}\nputs [catch f m]; puts $m\nproc k {} {tailcall}\nputs [catch k m]; puts [string length $m]\n", "1\ntailcall can only be called from a proc, lambda or method\n1\ninvalid command name \"nosuch\"\n0\n0\n"),
            ok("tailcall_from_loops_namespaces_and_lambdas", "proc f {} {foreach x {1 2 3} {if {$x==2} {tailcall list $x}}}\nputs [f]\nnamespace eval ns {proc g {} {return ns-g}; proc f {} {tailcall g}}\nputs [ns::f]\nputs [apply {{} {tailcall expr {1+2}}}]\nproc w {} {tailcall string length abcd}\nputs [w]\nproc u {} {tailcall list a b; puts notreached}\nputs [u]\n", "2\nns-g\n3\n4\na b\n"),
        ],
    );
}
