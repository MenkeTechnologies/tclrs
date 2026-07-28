# expr: the operator set, and the places where Tcl's arithmetic is its own.

set checks 0

proc check {label got want} {
    global checks
    incr checks
    if {$got ne $want} {
        error "$label: expected \"$want\" but got \"$got\""
    }
}

# Precedence and grouping.
check "precedence" [expr {1 + 2 * 3}] 7
check "parentheses" [expr {(1 + 2) * 3}] 9
check "unary minus" [expr {-(2 + 3)}] -5

# Integer division and remainder floor toward negative infinity, which is not
# what C does and not what most languages do.
check "floored division" [expr {-57 / 10}] -6
check "floored remainder" [expr {-57 % 10}] 3
check "negative divisor" [expr {57 / -10}] -6
check "negative divisor remainder" [expr {57 % -10}] -3

# ** is right associative and stays integral for integral operands.
check "power" [expr {2 ** 10}] 1024
check "power associativity" [expr {2 ** 3 ** 2}] 512
check "negative exponent" [expr {2 ** -1}] 0

# Doubles print in the shortest form that reads back exactly, and never look
# like an integer.
check "double division" [expr {3.0 / 2}] 1.5
check "double promotion" [expr {1.0 + 1}] 2.0
check "double product" [expr {2.0 * 3}] 6.0

# Comparison prefers numbers; the lexical operators never do.
check "numeric comparison" [expr {"10" > "9"}] 1
check "string comparison" [expr {"10" gt "9"}] 0
check "string equality" [expr {"abc" eq "abc"}] 1
check "numeric equality across forms" [expr {1 == 1.0}] 1
check "string equality across forms" [expr {"1" eq "1.0"}] 0

# Bitwise and shift.
check "bitwise and" [expr {6 & 3}] 2
check "bitwise or" [expr {6 | 3}] 7
check "bitwise xor" [expr {6 ^ 3}] 5
check "complement" [expr {~5}] -6
check "left shift" [expr {1 << 10}] 1024
check "right shift" [expr {-16 >> 2}] -4

# Logical operators short-circuit, and the ternary picks one arm.
check "and" [expr {1 && 0}] 0
check "or" [expr {0 || 3}] 1
check "not" [expr {!0}] 1
check "ternary" [expr {5 > 3 ? "yes" : "no"}] yes

# Membership compares as strings against a list's elements, so a value equal
# numerically but written differently is not a member.
check "in" [expr {"b" in {a b c}}] 1
check "in is not numeric" [expr {1 in {01}}] 0
check "ni" [expr {"z" ni {a b c}}] 1

# A condition has to be a Tcl boolean: a number in any radix, or one of the
# boolean words abbreviated to any unambiguous prefix.
check "boolean word" [expr {"yes" ? 1 : 0}] 1
check "boolean prefix" [expr {"fals" ? 1 : 0}] 0
check "hex is a number" [expr {0x10 + 1}] 17
check "boolean rejects other words" [catch {expr {"maybe" ? 1 : 0}}] 1

puts "expressions.tcl: $checks checks passed"
