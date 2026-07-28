proc p4 {r1 r2 {o1 "x"}} {set __o {}; incr s3 1; return $__o}
puts [p4 a [list 255 end]]
