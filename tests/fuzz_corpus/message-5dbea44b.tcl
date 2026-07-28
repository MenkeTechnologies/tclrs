set s2 100
switch -exact [expr {$s2 ? 42 : 0}] {* {set w8 0; while {$w8 < 1} {incr w8; while {$w9 < 2} {incr w9; puts {a b c}}}} a* {foreach e10 "back\\slash" {puts [lsort $s1]; break}} default {puts [expr {~0.1}]; set d12 [dict create 0 255 A-B b]}}
error "a\\b"
