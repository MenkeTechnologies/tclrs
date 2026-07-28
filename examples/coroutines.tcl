# Coroutines: a body that suspends where it stands and resumes there.

set checks 0

proc check {label got want} {
    global checks
    incr checks
    if {$got ne $want} {
        error "$label: expected \"$want\" but got \"$got\""
    }
}

# A generator. Creating the coroutine enters the body and runs it up to the
# first `yield`, whose value is what the `coroutine` command itself returns;
# each later call resumes the body where it stopped.
proc counter {start limit} {
    for {set i $start} {$i <= $limit} {incr i} {
        yield $i
    }
    return done
}

check "first value at creation" [coroutine count counter 1 3] 1
check "second value" [count] 2
check "third value" [count] 3
check "body's return value" [count] done

# The coroutine's command goes away when its body ends.
check "gone after return" [catch {count}] 1

# A resumed `yield` yields the value the resumer passed, so a coroutine consumes
# as well as produces.
proc accumulate {} {
    set total 0
    while {1} {
        set n [yield $total]
        if {$n eq "stop"} {
            return $total
        }
        incr total $n
    }
}
coroutine adder accumulate
check "accumulate 10" [adder 10] 10
check "accumulate 32" [adder 32] 42
check "accumulate final" [adder stop] 42

# A body may suspend at any depth, inside a loop and inside an open `catch`.
proc deep {} {
    foreach group {{1 2} {3 4}} {
        foreach n $group {
            if {[catch {yield $n} resumed] == 0 && $resumed eq "quit"} {
                return early
            }
        }
    }
    return exhausted
}
check "deep first" [coroutine walk deep] 1
check "deep second" [walk] 2
check "deep third" [walk] 3
check "deep quit" [walk quit] early

# `info coroutine` names the running coroutine, and is empty outside one.
proc whoami {} {
    yield [info coroutine]
    return [info coroutine]
}
check "info coroutine inside" [coroutine named whoami] ::named
check "info coroutine outside" [info coroutine] {}

# Two coroutines interleave: each keeps its own frames and locals.
coroutine odds counter 1 5
coroutine evens counter 2 6
check "odds start" [odds] 2
check "evens start" [evens] 3
check "odds continues independently" [odds] 3
check "evens continues independently" [evens] 4

# An error out of a body is reported to whoever resumed it, and deletes the
# coroutine.
proc explode {} {
    yield ready
    error "boom"
}
check "runs to first yield" [coroutine bomb explode] ready
check "error reaches resumer" [catch {bomb} msg] 1
check "error message" $msg boom
check "deleted after error" [catch {bomb}] 1

puts "coroutines.tcl: $checks checks passed"
