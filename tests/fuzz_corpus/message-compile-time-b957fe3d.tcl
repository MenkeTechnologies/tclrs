set f5 0
if {$f5 + "a" / -7} {for {set f9 0} {$f9 < 2} {incr f9} {if {$s1 < 1000} {puts $; error {}}; puts 42}}
switch -exact $s2 {a* {switch -exact [format %s 42] {{[ab]*} {puts 0.1; for {set f16 0} {$f16 < 2} {incr f16} {set v17 16}} default {if {[llength 0.1] eq "1.0"} {puts [string index 42 3]} else {expr {1 +}; puts end-1}}}; while {$w18 < 5} {incr w18; catch {puts a*b; unset s1} m19; puts m:$m19}} default {}}
