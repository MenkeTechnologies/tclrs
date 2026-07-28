# drive.tcl — the per-case driver, run under BOTH engines.
#
# `scripts/fuzz_parity.sh` composes one runnable file per case by replacing the
# case marker line below with the case's text, and hands that file to `tclsh`
# and to `tclrs` in turn. Whatever each writes to stdout, what its first stderr
# line says and what it exits with is the whole comparison contract — the same
# contract `tests/cli_differential.rs` uses for a hand-written script.
#
# The case's text is spliced in at the *top level* of the file rather than
# wrapped in `catch`/`eval`, because `proc` and `coroutine` are only legal at a
# script's top level in tclrs (README [0x05]). Wrapping would refuse every case
# that defines one and hide most of the language behind a skip.
#
# There is no prologue: any command run before the case would have to be
# implemented identically by both engines and would change what the case
# observes. What the driver does contribute is the completion marker — the
# record separator printed after the case. Its presence in a captured stdout
# means the case ran to its end, which is how a run that died or was killed
# mid-stream is told apart from a case that failed on purpose. Both engines
# print it, so it hides no divergence.

#<<CASE>>
puts -nonewline "\x1e"
