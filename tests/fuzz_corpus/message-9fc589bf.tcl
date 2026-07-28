set s1 {}
proc p5 {r1 {o1 10} args} {set __o {}; foreach e6 * {if {(100) ** 5} {puts abc; incr s2 2} else {puts 42; puts [expr {[string length 255] ge "10"}]}}; return $__o}
puts [p5 [string length {[ab]}] $s1 [string length {}] -7]
