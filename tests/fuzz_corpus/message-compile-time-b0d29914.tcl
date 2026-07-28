proc p5 {{o1 a*b}} {set __o {}; if {~65535} {return -code error abc}; return $__o}
puts [p5 ]
if {(42) + 1000} {puts "a; puts [list 7 {}]}
