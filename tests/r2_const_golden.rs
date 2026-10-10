//! `const` against answers recorded from the reference interpreter.

mod golden;

use golden::ok;

#[test]
fn pinned() {
    golden::check(
        "r2_const_golden",
        &[
            ok("const_refuses_writes", "const X 5\nputs $X\nforeach s {{set X 6} {incr X} {append X a} {unset X} {foreach X {1} {}} {set ::X 7} {global X; set X 8}} {\n  puts \"$s => [catch {eval $s} m] $m\"\n}\nputs [catch {unset -nocomplain X} m]\nputs [info exists X]\nputs $X\nset n X\nputs [catch {set $n 9} m]; puts $m\nputs $X\n", "5\nset X 6 => 1 can't set \"X\": variable is a constant\nincr X => 1 can't incr \"X\": variable is a constant\nappend X a => 1 can't set \"X\": variable is a constant\nunset X => 1 can't unset \"X\": variable is a constant\nforeach X {1} {} => 1 can't set \"X\": variable is a constant\nset ::X 7 => 1 can't set \"::X\": variable is a constant\nglobal X; set X 8 => 1 can't set \"X\": variable is a constant\n0\n1\n5\n1\ncan't set \"X\": variable is a constant\n5\n"),
            ok("const_declaration_rules", "set y 1\nputs [catch {const y 2} m]; puts $m\nconst Z 3\nputs [catch {const Z 4} m]; puts \"<$m> $Z\"\nputs [catch {const A 1 B 2} m]; puts $m\nputs [catch {const} m]; puts $m\nputs [catch {const arr(1) 2} m]; puts $m\nconst V [expr {2 + 3}]\nputs $V\nputs [info constant V]\nputs [info constant y]\nputs [lsort [info consts]]\n", "1\ncan't make constant \"y\": variable already exists\n0\n<> 3\n1\nwrong # args: should be \"const varName value\"\n1\nwrong # args: should be \"const varName value\"\n1\ncan't make constant \"arr(1)\": name refers to an element in an array\n5\n1\n0\nV Z\n"),
            ok("const_in_procedures_and_namespaces", "proc f {} {\n  const K 3\n  puts $K\n  puts [catch {set K 4} m]; puts $m\n  puts [catch {incr K} m]; puts $m\n  puts [catch {unset K} m]; puts $m\n  puts [info constant K]\n  puts [info consts]\n}\nf\nputs [info exists K]\nproc g {} {const Z 1; return [info exists Z]}\nputs [g]\nputs [info exists Z]\nnamespace eval n {const V 10; proc get {} {variable V; return $V}}\nputs [n::get]\nputs [catch {set n::V 11} m]; puts $m\n", "3\n1\ncan't set \"K\": variable is a constant\n1\ncan't incr \"K\": variable is a constant\n1\ncan't unset \"K\": variable is a constant\n1\nK\n0\n1\n0\n10\n1\ncan't set \"n::V\": variable is a constant\n"),
        ],
    );
}
