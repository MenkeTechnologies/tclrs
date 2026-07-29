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

# lassign spreads a list across variables and hands back what is left over.
set rest [lassign {a b c d} first second]
check "lassign first" $first a
check "lassign second" $second b
check "lassign remainder" $rest {c d}
# More variables than elements: the rest get the empty string.
set rest [lassign {x} p q]
check "lassign past the end" <$q> <>

# lset replaces an element in place, and grows the list by one at the end.
set row {a b c}
lset row 1 X
check "lset" $row {a X c}
lset row end Z
check "lset end" $row {a X Z}
lset row 3 W
check "lset grows by one" $row {a X Z W}
set grid {{a b} {c d}}
lset grid 1 0 Q
check "lset index path" $grid {{a b} {Q d}}

# lpop takes an element out of the variable, the last one by default.
set stack {1 2 3}
check "lpop yields the element" [lpop stack] 3
check "lpop shortened the list" $stack {1 2}
check "lpop at an index" [lpop stack 0] 1

# ledit replaces a range; both ends clamp rather than refusing.
set letters {a b c d}
ledit letters 1 2 X
check "ledit" $letters {a X d}
ledit letters 9 9 Y
check "ledit clamps" $letters {a X d Y}

# lremove is the same idea by index, and ignores an index outside the list.
check "lremove" [lremove {a b c d} 1 3] {a c}
check "lremove ignores out of range" [lremove {a b c} 9] {a b c}

# lrepeat builds a list by repetition.
check "lrepeat" [lrepeat 3 x] {x x x}
check "lrepeat many" [lrepeat 2 a b] {a b a b}

# lseq is Tcl 9's arithmetic sequence. A zero step is one element, not a hang,
# and a step pointing away from the end is none at all.
check "lseq count" [lseq 4] {0 1 2 3}
check "lseq range" [lseq 1 5] {1 2 3 4 5}
check "lseq counts down" [lseq 3 1] {3 2 1}
check "lseq by" [lseq 1 to 10 by 3] {1 4 7 10}
check "lseq count form" [lseq 5 count 3] {5 6 7}
check "lseq zero step" [lseq 1 10 0] 1
check "lseq wrong way" <[lseq 1 10 -2]> <>

# lmap is foreach that collects. An iteration that continues contributes
# nothing, where an empty body contributes an empty element.
check "lmap" [lmap n {1 2 3} {expr {$n * $n}}] {1 4 9}
check "lmap two lists" [lmap a {1 2} b {x y} {list $a $b}] {{1 x} {2 y}}
check "lmap continue omits" [lmap n {1 2 3} {if {$n == 2} continue; set n}] {1 3}
check "lmap empty body" [lmap n {1 2} {}] {{} {}}

puts "lists.tcl: $checks checks passed"
