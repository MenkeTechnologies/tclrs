proc p3 {{o1 {#comment}}} {set __o {}; return $__o}
proc p4 {r1 r2 args} {set __o {}; if {255 % 42} {puts $r2; puts [p3 $r2]}; if {1 ni -0.0} {return -code error hello}; return $__o}
if {("b") >> 0} {puts [format %lld x]; puts [p3 ]} elseif {3 <= (7)} {foreach {e7 e8} x {puts [p4 [list abc 1] x]}} else {eval {puts b}; for {set f9 0} {$f9 < 2} {incr f9} {set d10 [dict create {[} 42 -1 0.1]; if {$f5 >> 3 ** 16} {puts 1000; puts [lsort "x"]} else {}}}
