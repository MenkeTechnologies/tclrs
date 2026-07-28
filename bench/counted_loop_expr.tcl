# The same counted loop, incremented through `expr` instead of `incr`. The
# only difference in the bytecode is the extension op `expr` emits to normalize
# its result — which is the one thing fusevm's AOT compiler has no lowering
# for, so the whole loop deopts to the interpreter. Read this row next to
# `counted_loop` to see what a single unlowered op costs.
set i 0
while {$i < 3000000} {
    set i [expr {$i + 1}]
}
puts $i
