# The counted loop of `counted_loop.tcl`, moved inside a procedure. That is the
# one difference: a procedure's locals are frame slots rather than VM globals,
# so the loop's ops are `GetSlot`/`SetSlot` and fusevm's tracing tier will take
# it. Read this row next to `counted_loop` to see what the tracing JIT is worth
# when it fires, and `tclrs --tiers` on both to see that it fires on only one.
proc count {n} {
    set i 0
    while {$i < $n} {
        incr i
    }
    return $i
}
puts [count 3000000]
