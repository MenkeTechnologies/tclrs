# Associative data: `dict` over a value, `array` over a variable.

set checks 0

proc check {label got want} {
    global checks
    incr checks
    if {$got ne $want} {
        error "$label: expected \"$want\" but got \"$got\""
    }
}

# A dict is a list of alternating keys and values, so it prints like one and can
# be passed like any string.
set d [dict create name ada age 36]
check "dict create" $d {name ada age 36}
check "dict get" [dict get $d name] ada
check "dict keys" [dict keys $d] {name age}
check "dict values" [dict values $d] {ada 36}
check "dict exists" [dict exists $d age] 1
check "dict exists missing" [dict exists $d city] 0
check "dict get missing is an error" [catch {dict get $d city}] 1

# dict set writes through a variable — the dict itself is a value, so the
# command that changes one names the variable holding it.
set d2 $d
dict set d2 city london
check "dict set adds" [dict get $d2 city] london
dict set d2 name grace
check "dict set overwrites" [dict get $d2 name] grace
check "original untouched" [dict get $d name] ada

# dict remove takes a value and yields a new one.
check "dict remove" [dict remove $d2 age] {name grace city london}
check "dict remove missing key" [dict remove $d city] {name ada age 36}

# dict merge, with later dicts winning.
check "dict merge" [dict merge {a 1 b 2} {b 3 c 4}] {a 1 b 3 c 4}
check "dict merge empty" [dict merge {a 1} {}] {a 1}

check "dict size" [dict size $d] 2
check "dict size empty" [dict size {}] 0

# `dict for` walks the pairs, binding two variables per iteration...
set rendered {}
dict for {k v} $d {
    lappend rendered "$k=$v"
}
check "dict for" [join $rendered " "] "name=ada age=36"

# ...and a dict is a list, so the ordinary list loop walks it too.
set again {}
foreach {k v} $d {
    lappend again "$k=$v"
}
check "dict as pairs" $again $rendered

# An array is a variable, not a value: its elements are named a(k).
array set stock {apples 3 pears 7 figs 2}
check "array exists" [array exists stock] 1
check "array size" [array size stock] 3
check "array element" $stock(pears) 7
check "array names sorted" [lsort [array names stock]] {apples figs pears}
check "array get sorted" [lsort [array get stock]] {2 3 7 apples figs pears}

# Elements are ordinary variables, so the ordinary commands work on them.
incr stock(apples) 5
check "incr element" $stock(apples) 8
set stock(plums) 0
check "size after add" [array size stock] 4
array unset stock plums
check "size after unset" [array size stock] 3

# Summing the values of an array.
set total 0
foreach name [array names stock] {
    incr total $stock($name)
}
check "array total" $total 17

# An array inside a procedure is that procedure's own: two calls do not
# accumulate, and two frames of a recursive procedure hold different elements.
proc counted {} {
    set seen(once) 1
    return [array size seen]
}
check "a local array starts empty" [counted] 1
check "and again on the next call" [counted] 1

proc depth {n} {
    set frame($n) here
    if {$n > 0} {
        depth [expr {$n - 1}]
    }
    return [array names frame]
}
check "recursion does not share" [depth 2] 2

# `global` is what reaches the script's own array from inside a procedure.
proc reach {} {
    global stock
    return [array size stock]
}
check "global reaches the outer array" [reach] 3

# Converting between the two: an array's `get` is a dict.
set as_dict [array get stock]
check "array get is a dict" [dict get $as_dict figs] 2

puts "dicts.tcl: $checks checks passed"
