# String building by repeated concatenation — the idiom that grows a value by
# rewriting it, so both implementations copy the accumulated string every
# iteration. The script prints the iteration count rather than the string
# itself, which would be megabytes; `string length` is not in tclrs yet.
set s ""
set i 0
while {$i < 100000} {
    set s "$s$i"
    incr i
}
puts $i
