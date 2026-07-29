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

# `uplevel` runs a script in the frame of a caller instead of this one: level 1
# is the caller, level 0 is here, and `#0` is the script's own top level. What
# the script reads and writes are that level's variables.
proc peek {} {
    return [uplevel 1 {set hidden}]
}
proc holder {} {
    set hidden found
    return [peek]
}
check "uplevel reads the caller" [holder] found

# A write goes to the caller's variable, and a variable the script creates is
# created there — which is how a procedure gives one back without a return.
proc stamp {} {
    uplevel 1 {set marked yes}
}
proc stamped {} {
    stamp
    return $marked
}
check "uplevel writes the caller" [stamped] yes

# Levels count outwards, so a procedure two calls deep can reach the first.
proc third {} {
    return [uplevel 2 {set depth}]
}
proc second {} {
    return [third]
}
proc first {} {
    set depth one
    return [second]
}
check "uplevel counts outwards" [first] one

# `#0` is the top level however deep the call is, and needs no `global`.
set setting loud
proc ask {} {
    return [uplevel #0 {set setting}]
}
check "uplevel #0 is the top level" [ask] loud

# A level that does not exist is an error rather than a guess.
check "no such level" [catch {uplevel 9 {set x 1}} msg] 1
check "no such level message" $msg {bad level "9"}

# `apply` runs a lambda: a two-element list of parameters and body, with the
# same argument rules a procedure has — defaults, a variadic tail, and a frame
# of its own.
check "apply" [apply {{a b} {expr {$a + $b}}} 20 22] 42
check "apply with no parameters" [apply {{} {return still}}] still
check "apply with a default" [apply {{a {b 10}} {expr {$a + $b}}} 5] 15
check "apply with a tail" [apply {{a args} {llength $args}} 1 2 3] 2

# The lambda's locals are its own, and `return` returns from it.
check "a lambda has its own locals" [apply {{} {set v 1
    incr v
    return $v}}] 2

# A lambda may be applied wherever a value can be built, including from another
# lambda.
check "a lambda inside a lambda" [apply {{n} {apply {{m} {expr {$m * 3}}} $n}} 4] 12

# Its third element is a namespace, and this frontend has one: `::`.
check "the global namespace" [apply {{x} {expr {$x * 2}} ::} 21] 42

# A wrong argument count is reported against the lambda rather than a name,
# because a lambda has none.
check "too few arguments" [catch {apply {{a b} {expr 1}} 1} msg] 1
check "reported as a lambda" $msg {wrong # args: should be "apply lambdaExpr a b"}

puts "procs.tcl: $checks checks passed"
