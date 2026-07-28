# Lists: a list is a string, so every list command re-derives its elements from
# one and every result is quoted back into one.

set checks 0

proc check {label got want} {
    global checks
    incr checks
    if {$got ne $want} {
        error "$label: expected \"$want\" but got \"$got\""
    }
}

# Construction and canonical quoting. An element needing protection is braced,
# an empty one becomes {}, and a bracket is escaped rather than braced.
check "list" [list a b c] {a b c}
check "list quotes spaces" [list a "b c" d] {a {b c} d}
check "list quotes empty" [list a {} b] {a {} b}
check "list escapes bracket" [list {a]b}] {a\]b}

# Length and indexing, including the index grammar.
set items {alpha beta gamma delta}
check "llength" [llength $items] 4
check "lindex first" [lindex $items 0] alpha
check "lindex end" [lindex $items end] delta
check "lindex end-1" [lindex $items end-1] gamma
check "lindex out of range" [lindex $items 99] {}

# Growing a list in place.
set acc {}
lappend acc one
lappend acc two three
check "lappend" $acc {one two three}

# Slicing, reversing, inserting and replacing all yield new lists.
check "lrange" [lrange $items 1 2] {beta gamma}
check "lrange to end" [lrange $items 2 end] {gamma delta}
check "lreverse" [lreverse {1 2 3}] {3 2 1}
check "linsert" [linsert {a d} 1 b c] {a b c d}
check "lreplace" [lreplace {a b c d} 1 2 X] {a X d}
check "lreplace deletes" [lreplace {a b c d} 1 2] {a d}

# Searching: exact by default, and the result is an index unless -inline asks
# for the element.
check "lsearch" [lsearch $items gamma] 2
check "lsearch missing" [lsearch $items omega] -1
check "lsearch glob" [lsearch -glob $items {d*}] 3
check "lsearch inline" [lsearch -inline -glob $items {b*}] beta
check "lsearch all" [lsearch -all {a b a c a} a] {0 2 4}

# Sorting. The default comparison is ASCII, so numbers need -integer.
check "lsort ascii" [lsort {pear apple fig}] {apple fig pear}
check "lsort ascii on numbers" [lsort {10 9 100}] {10 100 9}
check "lsort integer" [lsort -integer {10 9 100}] {9 10 100}
check "lsort decreasing" [lsort -integer -decreasing {10 9 100}] {100 10 9}
check "lsort unique" [lsort -unique {b a b c a}] {a b c}
check "lsort real" [lsort -real {2.5 1.75 10.0}] {1.75 2.5 10.0}

# Between lists and strings.
check "join" [join {a b c} -] a-b-c
check "join default" [join {1 2 3}] {1 2 3}
check "split" [split a-b-c -] {a b c}
check "split chars" [split abc {}] {a b c}
check "concat" [concat {a b} {c d}] {a b c d}
check "concat trims" [concat "  a  " " b "] {a b}

# A list round-trips through its string representation.
set nested [list {a b} {c d}]
check "nested list" [llength $nested] 2
check "nested element" [lindex $nested 0] {a b}
check "nested inner" [lindex [lindex $nested 1] 1] d

# Iterating a list with the loop that reads it in place.
set total 0
foreach n [split "1,2,3,4,5" ,] {
    incr total $n
}
check "sum of split" $total 15

puts "lists.tcl: $checks checks passed"
