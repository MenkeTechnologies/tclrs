set s2 "\{} a"
proc p5 {} {set __o {}; for {set f6 0} {$f6 < 1} {incr f6} {puts {x}; if {1 || 65535} {set v7 "x \{y z\}"; puts {q"r}} else {}; break}; return $__o}
puts [format %b $s2]
switch -exact -- $w9 {*b {puts xyz; puts [p5 ]} default {switch -glob -1 {a?c {puts [expr {}]; puts [format %c end-1]}}}}
