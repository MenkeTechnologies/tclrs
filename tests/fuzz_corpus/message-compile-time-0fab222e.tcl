set s4 end
proc p6 {{o1 7}} {set __o {}; if {4 * -65536} {return -code error 42}; return $__o}
switch -glob $s4 {x {foreach e8 end {set w9 0; while {$w9 < 1} {incr w9; error {[a]}}}; set v10 -1} x {for {set f11 0} {$f11 < 5} {incr f11} {for {set f12 0} {$f12 < 2} {incr f12} {expr {}; set v13 a*b}}; unset s3}}
puts [p6 [format %s end-1]]
