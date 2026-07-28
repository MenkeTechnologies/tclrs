# Integer arithmetic in a loop: multiply, add, subtract and a shift per
# iteration, all through `expr`. The accumulator stays inside i64.
set i 0
set sum 0
while {$i < 1000000} {
    set sum [expr {$sum + $i * $i - ($i >> 3)}]
    incr i
}
puts $sum
