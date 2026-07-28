# Variables and substitution: the rules that decide what a word means.
#
# Every example in this directory is self-checking: `check` raises an error the
# moment a result drifts, so a non-zero exit is a regression. The scripts are
# also run under tclsh and compared byte for byte, so the expectations here are
# checked against the reference implementation rather than trusted.

set checks 0

proc check {label got want} {
    global checks
    incr checks
    if {$got ne $want} {
        error "$label: expected \"$want\" but got \"$got\""
    }
}

# Rule 4: a dollar sign substitutes a variable, inside a quoted word too.
set greeting hello
set target world
check "quoted substitution" "$greeting, $target" "hello, world"

# Rule 6: braces suppress every substitution, so the word is its own text.
check "braced word" {$greeting} {$greeting}

# Rule 5: brackets run a command and substitute its result.
check "command substitution" [string toupper $greeting] HELLO
check "nested substitution" [string length [set greeting]] 5

# A value that looks numeric but is not: Tcl keeps the string it was written as.
set version 05
check "leading zero kept" $version 05
set scaled 1.10
check "trailing zero kept" $scaled 1.10

# incr, with and without an explicit increment.
set n 1
incr n
incr n 40
check "incr" $n 42

# append builds a string in place and yields the new value.
set path /usr
append path /local
check "append result" [append path /bin] /usr/local/bin

# An array element is a variable whose name carries a key; unset takes either a
# whole array or one element of it.
set counts(apples) 3
set counts(pears) 7
check "array element" $counts(apples) 3
check "array size" [array size counts] 2
unset counts(pears)
check "size after element unset" [array size counts] 1
unset counts
check "array gone" [array exists counts] 0

puts "hello.tcl: $checks checks passed"
