set s2 8
for {set f5 0} {$f5 < 3} {incr f5} {if {1 % $s2} {set w6 0; while {$w6 < 1} {incr w6; break}} else {for {set f8 0} {$f8 < 2} {incr f8} {puts xyz}}; if {"b" / $f5} {error { padded }; if {~$s2 / [llength end]} {puts [llength -0.0]} else {}}}
