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

puts "eval.tcl: $checks checks passed"
