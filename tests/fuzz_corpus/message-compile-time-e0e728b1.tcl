proc p5 {r1 r2 {o1 *}} {set __o {}; return $__o}
proc p7 {} {set __o {}; puts [p5 {}]; return $__o}
