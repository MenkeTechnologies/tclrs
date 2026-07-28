set s3 {a b c}
proc p6 {r1 {o1 "x"}} {set __o {}; append __o $r1; append __o $o1; switch -- $o1 {x {if {255 && 8} {puts 5; puts [concat "x" a x]}} *b {switch -- "pre-$o1-post" {* {puts [list "x" {q"r}]} a*b*c {error {x]y}} {} {puts [lrange "x" 7 end-1]; set v7 65535}}; if {"1.0" * 7} {set v8 16}} default {if {100 >= "abc"} {puts a; append s1 -7 {$x}} elseif {-255} {puts {$x}} else {puts [expr {1 ** 8}]; puts xyz}; puts {}}}; if {2 gt 4} {}; return $__o}
puts [format %g "pre-$s3-post"]
puts [p6 ]
