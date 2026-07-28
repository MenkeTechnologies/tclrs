catch {set w10 0; while {$w10 < 5} {incr w10; switch -- a {*b {lappend s2 1; puts [list 1.5 abc]}}}; puts [expr {1000 * -4611686018427387904}]} m9; puts m:$m9
