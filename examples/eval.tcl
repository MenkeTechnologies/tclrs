# eval: a script built as a value and run against the interpreter's own
# variables. It is compiled when the op runs and cached by source text, so the
# same script inside a loop is lowered once however many times it runs.

set checks 0

proc check {label got want} {
    global checks
    incr checks
    if {$got ne $want} {
        error "$label: expected \"$want\" but got \"$got\""
    }
}

# The simplest form: a script that is already text.
check "eval literal" [eval {expr {6 * 7}}] 42

# eval concatenates its arguments, so a command may be assembled from words.
check "eval concatenates" [eval expr {1 +} 2] 3
check "eval builds a command" [eval [list string toupper hello]] HELLO

# The nested script sees the interpreter's variables, in both directions.
set base 10
check "reads a variable" [eval {expr {$base * 2}}] 20
eval {set derived [expr {$base + 5}]}
check "writes a variable" $derived 15

# A variable the nested script set before failing keeps the value it was given:
# the script is not undone.
set partial none
check "failing script" [catch {eval {set partial half; error stop}} msg] 1
check "error message" $msg stop
check "value survives the failure" $partial half

# Building a script from data. Each iteration runs the same source text, so it
# is compiled once.
set total 0
foreach n {1 2 3 4 5} {
    eval {incr total $n}
}
check "loop over eval" $total 15

# A generated script, where the command name itself is data.
set results {}
foreach op {toupper tolower totitle} {
    lappend results [eval [list string $op "mIxEd"]]
}
check "generated commands" $results {MIXED mixed Mixed}

# Array elements are variables, so an evaluated script reaches them too.
array set config {mode fast retries 3}
eval {set config(retries) 5}
check "array element through eval" $config(retries) 5
check "array read through eval" [eval {array size config}] 2

# An evaluated script is a chunk of its own: it shares the interpreter's
# variables, not its procedures, so the script it runs is built from builtins.
check "nested eval" [eval {eval {expr {21 * 2}}}] 42

# A list is the safe way to build a script: `list` quotes each word, so a value
# containing spaces or brackets stays one word.
set value {a b]c}
check "list quotes the word" [eval [list string length $value]] 5

# Inside a procedure body the script runs against that procedure's own frame: it
# reads and writes the locals, and a variable it creates is a local too.
proc scaled {n} {
    set factor 3
    eval {set product [expr {$n * $factor}]}
    return $product
}
check "reaches a local" [scaled 7] 21

# It reaches nothing else. A global the body did not declare is no more visible
# to the nested script than it is to the body itself.
set outside 99
proc blind {} {
    return [catch {eval {set outside}} msg]
}
check "and nothing else" [blind] 1

proc declared {} {
    global outside
    return [eval {expr {$outside + 1}}]
}
check "unless the body declared it" [declared] 100

# A write is visible to the next command of the body, not only at the end.
proc stepwise {} {
    set x 1
    eval {set x 2}
    eval {incr x}
    return $x
}
check "written straight through" [stepwise] 3

# Each frame of a recursive procedure has its own locals, so a nested script in
# one cannot reach another's.
proc countdown {n} {
    set here $n
    eval {append here !}
    if {$n > 0} {
        countdown [expr {$n - 1}]
    }
    return $here
}
check "one frame at a time" [countdown 2] 2!

puts "eval.tcl: $checks checks passed"
