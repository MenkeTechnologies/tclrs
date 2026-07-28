proc p4 {{o1 7} args} {set __o {}; return $__o}
proc p5 {{o1 {[ab]}} args} {set __o {}; return $__o}
set d13 [dict create {} -7 -1 10]
switch -exact -- abc {a*b*c {if {0 == 42} {set v14 42; switch -glob -- [expr {$s2 / $s2}] {* {puts [p4 [format %s 42]]; puts b} default {append d13 xyz 10}}}; puts [string equal { } 42]} {} {switch -exact -- [list xyz abc] {* {foreach e17 {q"r} {puts [string replace {} end -1 5]; eval {puts a}}} default {expr {1 +}; eval {puts b}}}} {[ab]*} {for {set f18 0} {$f18 < 2} {incr f18} {switch -exact $s2 {{[ab]*} {puts [p5 A-B 5 -7]} {[ab]*} {puts 7} a*b*c {array set a19 {}; unset s1}}; while {$w20 < 1} {incr w20; break}}; while {$w21 < 1} {incr w21; lappend s3 5}} default {puts [string length "x"]; puts [p5 $d13 [expr {7 in {}}]]}}
