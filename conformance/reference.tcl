# Produce the reference outcome for every extracted case, using tclsh.
#
# Each case runs as `setup` followed by `body` in a fresh child interpreter, so
# one case cannot leave state behind for the next — the same isolation tclrs
# gets, where every case is a fresh compile and a fresh VM.
#
# The outcome of a case is the triple (return code, result string, stdout). The
# stdout capture is a port of tcltest's `Replace::puts`: writes to stdout are
# collected, writes to any other channel are handed to the real `puts`. tclrs
# captures its own `puts` the same way, so the two sides are comparable.
#
# Usage: tclsh reference.tcl <cases file> <out file> <start index> <scratch>
#
# Writes one line per case, flushed as it goes, so a supervisor that has to
# kill this process for a runaway case can see exactly how far it got and
# restart past it:
#   <index> <status> <base64 result> <base64 stdout>
# where status is `ok` for return code 0, `err` for 1, and `code<N>` otherwise.

set casesFile [file normalize [lindex $argv 0]]
set outFile   [file normalize [lindex $argv 1]]
set startIdx  [lindex $argv 2]
set scratch   [file normalize [lindex $argv 3]]

file mkdir $scratch
cd $scratch

proc b64 {s} {
    return [binary encode base64 [encoding convertto -profile tcl8 utf-8 $s]]
}

proc unb64 {s} {
    return [encoding convertfrom -profile tcl8 utf-8 [binary decode base64 $s]]
}

# The capture is installed in the child interpreter; `parent_capture` is how it
# reaches back here.
set ::CAPTURED ""
proc parent_capture {text} {
    append ::CAPTURED $text
}

set CAPTURE_PUTS {
    rename ::puts ::REAL_PUTS
    proc ::puts {args} {
	switch [llength $args] {
	    1 {
		CAPTURE [lindex $args 0]\n
		return
	    }
	    2 {
		if {[lindex $args 0] eq "-nonewline"} {
		    CAPTURE [lindex $args end]
		    return
		} else {
		    set channel [lindex $args 0]
		    set newline \n
		}
	    }
	    3 {
		if {[lindex $args 0] eq "-nonewline"} {
		    set channel [lindex $args 1]
		    set newline ""
		}
	    }
	}
	if {[info exists channel] && $channel eq "stdout"} {
	    CAPTURE [lindex $args end]$newline
	    return
	}
	return [::REAL_PUTS {*}$args]
    }
    # A case that calls [exit] would otherwise end the whole reference run.
    rename ::exit ::REAL_EXIT
    proc ::exit {{code 0}} {
	return -code error "exit $code"
    }
}

set in [open $casesFile r]
fconfigure $in -translation lf -encoding utf-8
set cases {}
while {[gets $in line] >= 0} {
    if {[string match "# *" $line]} continue
    lappend cases $line
}
close $in

set out [open $outFile [expr {$startIdx > 0 ? "a" : "w"}]]
fconfigure $out -translation lf -encoding utf-8

foreach line $cases {
    set fields [split $line "\t"]
    set idx [lindex $fields 0]
    if {$idx < $startIdx} continue
    set prog "[unb64 [lindex $fields 4]]\n[unb64 [lindex $fields 5]]\n[unb64 [lindex $fields 6]]"

    set ::CAPTURED ""
    set child [interp create]
    interp alias $child CAPTURE {} parent_capture
    $child eval $CAPTURE_PUTS
    set code [catch {$child eval $prog} res]
    catch {interp delete $child}

    set status [switch -- $code {
	0 {format ok}
	1 {format err}
	default {format code$code}
    }]
    puts $out "$idx\t$status\t[b64 $res]\t[b64 $::CAPTURED]"
    flush $out
}
close $out
