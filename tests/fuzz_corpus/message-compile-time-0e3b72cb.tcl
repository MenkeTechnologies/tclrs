set s1 65535
proc p7 {r1 {o1 -1}} {set __o {}; return $__o}
incr w13 4
switch -exact -- [expr {$w13 != $s1}] {* {if {(65535) ne 2} {if {255 ne 1} {puts $w13} else {puts "x"; puts [lrange $s2 -1 7]}; switch -glob hello {{} {puts [lrange $s2 7 7]; incr w13 0} a?c {puts [p7 ]} {} {puts [string index 0 end+1]} default {puts $w13}}} else {if {$w13 / 100} {set v23 -7; incr w13 2} else {eval {puts 1}}; while {$w26 < 3} {incr w26; puts [concat $s2 {} $s2]}}; dict set d3 0.1 "x"}}
