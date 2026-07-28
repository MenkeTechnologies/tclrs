proc p4 {{o1 x}} {set __o {}; return $__o}
set w7 0; while {$w7 < 2} {incr w7; for {set f8 0} {$f8 < 2} {incr f8} {append s2 10 0; set w9 0; while {$w9 < 4} {incr w9; error 1.5; puts [lreverse -0.0]}}; continue}
switch -glob -- 2 {{[ab]*} {for {set f11 0} {$f11 < 1} {incr f11} {set v12 {}; if {42 * -65536} {continue}}} a*b*c {switch -glob -- $s2 {x {puts [p4 [expr {-2 in {$x}}] a]; eval {puts 1}}}; for {set f14 0} {$f14 < 5} {incr f14} {}}}
