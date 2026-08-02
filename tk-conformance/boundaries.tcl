# Split a Tcl script into whole statements, and say where each one ends.
#
# The Tk widget demonstration is `wish`'s own sample application, and "how far
# does it get" is only a measurement if the units are statements: stopping
# part-way through a `proc` body would be an artefact of where the lines fall
# rather than of anything tclrs did. `info complete` is the interpreter's own
# answer to "is this a whole script yet", so the boundaries are Tcl's rather
# than a guess at its grammar.
#
# Usage: tclsh boundaries.tcl <script> <out>
# Output: one line per boundary, `<first line> <last line>`, 1-based inclusive.

set path [file normalize [lindex $argv 0]]
set outPath [file normalize [lindex $argv 1]]

set f [open $path]
fconfigure $f -encoding utf-8
set text [read $f]
close $f

set out [open $outPath w]
fconfigure $out -translation lf -encoding utf-8

proc isCommand {chunk} {
    foreach line [split $chunk \n] {
	set line [string trim $line]
	if {$line eq "" || [string index $line 0] eq "#"} continue
	return 1
    }
    return 0
}

set acc ""
set first 1
set n 0
foreach line [split $text \n] {
    incr n
    append acc $line "\n"
    if {![info complete $acc]} continue
    # A run of blank lines and comments is complete but is not a command, and
    # counting it as a statement that ran would inflate the answer with the
    # demo's copyright header.
    if {[isCommand $acc]} {
	puts $out "$first $n"
    }
    set acc ""
    set first [expr {$n + 1}]
}
close $out
