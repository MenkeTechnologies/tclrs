proc p7 {r1 r2 args} {set __o {}; for {set f8 0} {$f8 < 5} {incr f8} {switch -exact -0 {a* {puts xyz; puts [list x { padded }]} default {}}}; return $__o}
puts [format %8.3f [list c1 {}]]
