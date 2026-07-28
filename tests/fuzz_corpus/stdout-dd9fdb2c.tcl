set s3 3
set w8 0; while {$w8 < 3} {incr w8; puts -nonewline [expr {-9223372036854775808 / 5}]}
puts [format %e [expr {$s3 != 1}]]
