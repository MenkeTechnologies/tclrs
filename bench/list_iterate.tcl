# Build a list with `lappend`, then walk it with `foreach`. Two different
# costs in one script: tclsh keeps a list object and appends to it in
# amortized constant time, while tclrs re-derives the list from its string
# representation on every `lappend`.
set l {}
set i 0
while {$i < 5000} {
    lappend l $i
    incr i
}
set total 0
foreach x $l {
    set total [expr {$total + $x}]
}
puts $total
