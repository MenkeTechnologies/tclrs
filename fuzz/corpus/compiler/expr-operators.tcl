set i 0
while {$i < 3} {incr i}
puts [expr {(1+2)*3 % -4 ** 2 << 1 >> 1 & 7 | 8 ^ 3}]
puts [expr {"a" eq "a" == 1 ? -0.5 : ~5}]
puts [expr {1 in {1 2} || 0 && !0}]
