# The event loop and the scope commands: `after`, `update`, `vwait`, `uplevel`,
# `upvar #0`, `variable`, `apply` and `info`.
#
# Every delay here is 0, so the script measures ordering rather than time.

set checks 0

proc check {label got want} {
    global checks
    incr checks
    if {$got ne $want} {
        error "$label: expected \"$want\" but got \"$got\""
    }
}

# ── after: registering, listing and cancelling ─────────────────────────────

# `after ms script` answers with a handle. The registry belongs to the
# interpreter, so the handles are numbered from zero within one run.
set first [after 100000 {puts never}]
check "a handle names the interpreter's registry" $first after#0

set second [after 100000 {puts never}]
# The list is newest first: `after` pushes onto the front of it.
check "after info lists newest first" [after info] "$second $first"
# `after info id` answers with the script and what kind of handler it is.
check "after info id" [after info $second] "{puts never} timer"
check "an idle handler says so" [after info [after idle {puts never}]] "{puts never} idle"

# Cancelling takes either the handle or the script text.
after cancel $first
after cancel $second
after cancel {puts never}
check "everything registered was cancelled" [after info] ""
# Cancelling something that was never registered is not an error.
check "cancelling nothing is not an error" [after cancel nosuch] ""

# ── update: drain what is pending, in Tcl's order ──────────────────────────

# The event queue is serviced before the idle handlers, so a timer registered
# *after* an idle handler still runs first.
set ::order {}
after idle {lappend ::order idle-first}
after 0 {lappend ::order timer}
after idle {lappend ::order idle-second}
update
check "timers run before idle handlers" $::order "timer idle-first idle-second"

# `update idletasks` runs only the idle handlers and leaves the timers alone.
set ::order {}
after idle {lappend ::order idle}
set pending [after 100000 {lappend ::order never}]
update idletasks
check "idletasks ran the idle handler" $::order idle
check "idletasks left the timer alone" [after info] $pending
after cancel $pending

# An `after` script runs at the global level, whatever registered it.
proc arm {} {
    after 0 {set ::armed inner}
}
set ::armed outer
arm
update
check "an after script runs at the global level" $::armed inner

# ── vwait: block until a variable is written ───────────────────────────────

set ::done 0
after 0 {set ::done 1}
vwait ::done
check "vwait returned when the variable was written" $::done 1

# One timer *event* runs every handler that is already due, so both of these
# fire before `vwait` looks at its variable again.
set ::n 0
after 0 {set ::n 1}
after 0 {set ::n 2}
vwait ::n
check "one pass runs every due handler" $::n 2

# ── uplevel: run a script at another level ─────────────────────────────────

# `uplevel #0` evaluates at the script's own level, which is what a library
# does to define something globally.
proc define {} {
    uplevel #0 {set defined yes}
}
define
check "uplevel #0 wrote the global" $defined yes

# The remaining words concatenate into the script, as `concat` concatenates
# them.
proc define2 {} {
    uplevel #0 set defined2 yes
}
define2
check "uplevel concatenates its words" $defined2 yes

# With no level the default is 1 — one level up — which from a procedure the
# script's own level called is that level.
proc bump {} {
    uplevel {incr counter}
}
set counter 0
bump
bump
check "uplevel 1 from a top-level call reaches the top level" $counter 2

# ── upvar #0: another spelling for a global ────────────────────────────────

proc stash {value} {
    upvar #0 store local
    set local $value
    return $local
}
check "the link reads and writes the global" [stash 7] 7
check "and the global has it" $store 7

# The link is followed by every command that reaches a variable, not just `set`.
proc grow {} {
    upvar #0 store local
    lappend local a b
    return [llength $local]
}
check "lappend follows the link" [grow] 3
check "the global grew" $store "7 a b"

# ── variable: a namespace variable, which here is a global ─────────────────

variable version 1
check "variable at the script's level assigns" $version 1

proc readVersion {} {
    variable version
    return $version
}
check "variable inside a procedure reaches the global" [readVersion] 1

# ── apply: an anonymous procedure ──────────────────────────────────────────

check "a lambda is called like a procedure" [apply {{x} {expr {$x * 2}}} 21] 42
check "with defaults" [apply {{a {b 9}} {list $a $b}} 1] "1 9"
check "and with a variadic tail" [apply {{a args} {llength $args}} 1 2 3] 2

# Its locals are its own: an outer variable of the same name is untouched.
set x outer
check "the lambda's local is its own" [apply {{x} {set x inner}} 1] inner
check "the outer variable is untouched" $x outer

# ── info: what the interpreter knows about itself ──────────────────────────

check "the Tcl level this frontend implements" [info tclversion] 9.0

set present 1
check "info exists finds a variable" [info exists present] 1
check "and reports one that is not set" [info exists absent] 0
unset present
check "unset really unsets it" [info exists present] 0

set arr(one) 1
check "an array element" [info exists arr(one)] 1
check "and one that is not there" [info exists arr(two)] 0

check "a whole script is complete" [info complete {set x 1}] 1
check "one with a brace still open is not" [info complete "set x \{1"] 0

proc documented {a {b 2} args} {
    return $a
}
check "info args" [info args documented] "a b args"
check "info body" [string trim [info body documented]] "return \$a"
check "info default finds one" [info default documented b fallback] 1
check "and stores it" $fallback 2
check "info default on a formal without one" [info default documented a fallback] 0

check "info level at the script's own level" [info level] 0
proc deep {} {return [info level]}
check "info level inside a procedure" [deep] 1

check "info procs takes a pattern" [lsort [info procs docum*]] documented
check "info commands knows the built-ins" [info commands lreverse] lreverse
set zvar 1
check "info globals takes a pattern" [info globals zvar] zvar

puts "events.tcl: $checks checks passed"
