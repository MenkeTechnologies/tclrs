# String building by repeated concatenation — the idiom that grows a value by
# rewriting it. tclsh copies the accumulated string every iteration, which makes
# the loop quadratic; tclrs lowers an assignment that only grows its own
# variable to the same in-place append `append` uses, so it is linear
# (`src/cmd_string.rs`).
#
# The length is printed rather than the string, which would be half a megabyte,
# and printing it is what keeps the benchmark honest: a build that skipped the
# work would print the wrong number.
set s ""
set i 0
while {$i < 100000} {
    set s "$s$i"
    incr i
}
puts "$i [string length $s]"
