set d4 [dict create "x" "x" c1 {}]
proc p8 {r1 r2 {o1 xyz}} {set __o {}; return $__o}
set w11 0; while {$w11 < 1} {incr w11; continue}; puts [dict get $d4 end-1]
puts [p8 $s1 $u20 $d4 [format %s {#}] [expr {}]]
