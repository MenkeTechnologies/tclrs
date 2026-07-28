proc p4 {r1 r2 {o1 0.1} args} {set __o {}; if {100 != 1} {return -code error abc}; return $__o}
set w12 0; while {$w12 < 1} {incr w12; switch -exact -- [format %s a*b] {a* {puts [string compare 1000 1]; error {$}} default {puts [p4 [expr {"b" ^ 1}] hello 255]}}; incr f11 3}
