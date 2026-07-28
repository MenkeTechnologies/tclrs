set s2 {}
switch -exact "pre-$s2-post" {* {if {$s1 >> 1} {puts [lrange $s2 end-1 3]}; set v12 "x"} default {foreach e13 {a b c} {puts {a b c}}}}
* {foreach e14 end {eval {puts 1}; if {-65536 << 1} {continue}}} default {incr s1 3; for {set f15 0} {$f15 < 5} {incr f15} {foreach e16 -0 {}}}
