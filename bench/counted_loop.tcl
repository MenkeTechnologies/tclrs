# A counted loop and nothing else: variable read, compare, increment, branch.
# The shape a JIT exists for.
set i 0
while {$i < 3000000} {
    incr i
}
puts $i
