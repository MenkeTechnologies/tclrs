# Control flow: if, while, for, foreach, switch, break and continue.

set checks 0

proc check {label got want} {
    global checks
    incr checks
    if {$got ne $want} {
        error "$label: expected \"$want\" but got \"$got\""
    }
}

# The first branch whose test is a true boolean runs.
proc classify {n} {
    if {$n < 0} {
        return negative
    } elseif {$n == 0} {
        return zero
    } else {
        return positive
    }
}
check "if negative" [classify -3] negative
check "if zero" [classify 0] zero
check "if else" [classify 3] positive

# while, counting.
set i 0
set sum 0
while {$i < 10} {
    incr sum $i
    incr i
}
check "while sum" $sum 45

# for, with its own step clause.
set factorial 1
for {set i 1} {$i <= 6} {incr i} {
    set factorial [expr {$factorial * $i}]
}
check "for factorial" $factorial 720

# break leaves the loop, continue starts the next iteration — in a `for`, after
# the step has run.
set found -1
for {set i 0} {$i < 100} {incr i} {
    if {$i * $i > 50} {
        set found $i
        break
    }
}
check "break" $found 8

set odds {}
for {set i 0} {$i < 10} {incr i} {
    if {$i % 2 == 0} {
        continue
    }
    lappend odds $i
}
check "continue" $odds {1 3 5 7 9}

# foreach walks one list...
set doubled {}
foreach n {1 2 3 4} {
    lappend doubled [expr {$n * 2}]
}
check "foreach" $doubled {2 4 6 8}

# ...several in parallel, where the longest fixes the iteration count and the
# shorter ones supply empty values...
set pairs {}
foreach a {1 2 3} b {x y} {
    lappend pairs "$a$b"
}
check "foreach parallel" $pairs {1x 2y 3}

# ...and takes more than one variable per list.
set flat {}
foreach {k v} {a 1 b 2 c 3} {
    lappend flat "$k=$v"
}
check "foreach pairs" $flat {a=1 b=2 c=3}

# switch, matching exactly by default and by glob pattern on request.
proc sound {animal} {
    switch $animal {
        dog { return woof }
        cat { return meow }
        default { return "?" }
    }
}
check "switch exact" [sound dog] woof
check "switch default" [sound fish] ?

proc kind {name} {
    switch -glob $name {
        *.tcl { return script }
        *.rs { return rust }
        default { return other }
    }
}
check "switch glob" [kind main.tcl] script
check "switch glob other" [kind README.md] other

# A condition has to be a boolean: an arbitrary string is an error, not a false
# branch.
check "non-boolean condition" [catch {if {"banana"} {set x 1}}] 1

puts "control.tcl: $checks checks passed"
