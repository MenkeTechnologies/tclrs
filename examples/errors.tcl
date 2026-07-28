# Errors: raising with `error`, trapping with `catch`, and unwinding out of a
# procedure the guarded script called.

set checks 0

proc check {label got want} {
    global checks
    incr checks
    if {$got ne $want} {
        error "$label: expected \"$want\" but got \"$got\""
    }
}

# catch yields the completion code: 0 for a script that finished, 1 for one that
# raised. The result variable holds the value or the message.
check "catch ok" [catch {expr {1 + 1}} r] 0
check "catch ok result" $r 2
check "catch error" [catch {error "boom"} r] 1
check "catch error message" $r boom

# A run-time error from a command reads the same way.
check "divide by zero" [catch {expr {1 / 0}} r] 1
check "divide by zero message" $r "divide by zero"

# The error may come from any depth: catch unwinds the value stack and the call
# frames back to the guarded script.
proc inner {} {
    error "from inner"
}
proc middle {} {
    inner
    return unreached
}
proc outer {} {
    middle
    return unreached
}
check "error through frames" [catch {outer} r] 1
check "error through frames message" $r "from inner"

# Locals are gone with their frames, and the interpreter keeps running.
check "still running" [expr {2 + 2}] 4

# A catch inside a procedure traps and lets the procedure return normally.
proc compute {op a b} {
    switch $op {
        div {
            if {$b == 0} {
                error "cannot divide by zero"
            }
            return [expr {$a / $b}]
        }
        mul {
            return [expr {$a * $b}]
        }
        default {
            error "unknown op: $op"
        }
    }
}
proc guarded {op a b} {
    if {[catch {compute $op $a $b} msg]} {
        return "caught: $msg"
    }
    return "ok: $msg"
}
check "nested catch passes" [guarded mul 6 7] "ok: 42"
check "nested catch traps" [guarded div 6 0] "caught: cannot divide by zero"
check "nested catch traps unknown" [guarded pow 2 8] "caught: unknown op: pow"

# catch nests: the inner one takes the error, the outer sees success.
check "inner catch wins" [catch {catch {error boom} inner_msg}] 0
check "inner message" $inner_msg boom

# Re-raising after cleanup.
proc with_cleanup {} {
    global cleaned
    set cleaned no
    set code [catch {error "original"} msg]
    set cleaned yes
    if {$code} {
        error $msg
    }
}
check "re-raise" [catch {with_cleanup} r] 1
check "re-raise message" $r original
check "cleanup ran" $cleaned yes

# An error inside a loop body escapes the loop.
proc scan_until_bad {items} {
    set seen {}
    foreach item $items {
        if {![string is integer $item]} {
            error "not a number: $item"
        }
        lappend seen $item
    }
    return $seen
}
check "loop ok" [scan_until_bad {1 2 3}] {1 2 3}
check "loop error" [catch {scan_until_bad {1 x 3}} r] 1
check "loop error message" $r "not a number: x"

# An error raised by a command's own argument checking is catchable the same way
# — the ones a script's *shape* decides (a wrong argument count for a procedure
# whose signature is known) are refused while compiling instead, which is why
# there is no `catch` around one here.
check "bad list index" [catch {lindex {a b c} bogus}] 1
check "non-integer sort key" [catch {lsort -integer {1 x}}] 1
check "unbalanced braces in a list" [catch {llength "\{a b"}] 1

puts "errors.tcl: $checks checks passed"
