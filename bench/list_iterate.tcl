# Build a list with `lappend`, then walk it with `foreach`. Two different costs
# in one script: growing a list one element at a time, and reading one back.
#
# Both implementations keep the growth linear, and neither re-derives the list
# per append — tclsh holds a list object, tclrs appends to the list's own string
# inside the variable (`src/cmd_list.rs`). Raise the bound to check that it stays
# linear: a quadratic `lappend` shows up here first.
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
