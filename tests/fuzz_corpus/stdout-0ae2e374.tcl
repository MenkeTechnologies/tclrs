set s1 "\{} a"
set s2 -1
switch -glob -- [expr {$s2 + $s2}] {*b {unset d3; for {set f8 0} {$f8 < 5} {incr f8} {puts [string trimright abc ab]}} default {array set a9 {}; puts -nonewline "pre-$s1-post"}}
puts [expr {-9223372036854775808 * 4611686018427387903}]
