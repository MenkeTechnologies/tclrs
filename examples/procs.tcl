# Procedures: defaults, a trailing args, recursion, forward references, global.

set checks 0

proc check {label got want} {
    global checks
    incr checks
    if {$got ne $want} {
        error "$label: expected \"$want\" but got \"$got\""
    }
}

# A procedure may call one the script defines further down: every signature is
# collected before anything is emitted.
proc outer {n} {
    return [inner $n]
}
proc inner {n} {
    return [expr {$n * 2}]
}
check "forward reference" [outer 21] 42

# Parameters with defaults are filled in at the call site.
proc greet {name {greeting hello}} {
    return "$greeting, $name"
}
check "default used" [greet world] "hello, world"
check "default overridden" [greet world hi] "hi, world"

# A trailing `args` collects whatever is left, as a list.
proc tally {label args} {
    return "$label:[llength $args]:[join $args ,]"
}
check "args empty" [tally counts] counts:0:
check "args collected" [tally counts a b c] counts:3:a,b,c

# Recursion. Locals are frame slots, so each activation has its own.
proc fib {n} {
    if {$n < 2} {
        return $n
    }
    return [expr {[fib [expr {$n - 1}]] + [fib [expr {$n - 2}]]}]
}
check "fib 10" [fib 10] 55
check "fib 20" [fib 20] 6765

# A procedure with no explicit `return` yields its last command's result.
proc last_value {a b} {
    expr {$a + $b}
}
check "implicit result" [last_value 2 3] 5

# `global` addresses the global table from inside a body.
set counter 0
proc bump {} {
    global counter
    incr counter
    return $counter
}
bump
bump
check "global mutated" [bump] 3
check "global visible outside" $counter 3

# A local of the same name is a different variable.
proc shadow {} {
    set counter 99
    return $counter
}
check "local shadows" [shadow] 99
check "global untouched" $counter 3

# `return -code error` raises from a procedure exactly as `error` does.
proc refuse {} {
    return -code error "not today"
}
check "return -code error" [catch {refuse} msg] 1
check "return -code error message" $msg "not today"

# `return -code ok` is the ordinary return.
proc allow {} {
    return -code ok fine
}
check "return -code ok" [allow] fine

puts "procs.tcl: $checks checks passed"
