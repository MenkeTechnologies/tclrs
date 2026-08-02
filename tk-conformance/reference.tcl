# Produce the reference outcome for every extracted Tk case, using tclsh with
# the real Tk loaded.
#
# A port of `conformance/reference.tcl`. The isolation, the stdout capture and
# the output format are the same and the reasoning there applies unchanged.
#
# The one difference is what a "fresh interpreter" has to contain. A Tk case
# expects a main window: `.` exists, `winfo`, `pack`, `bind` and the widget
# commands are there. So each child interpreter gets Tk loaded into it —
# `load {} Tk $child`, which is how `interp create` + Tk is done — and that
# child gets a toplevel of its own. Without it every case would fail under the
# reference for want of `winfo`, and a reference that fails is a case the
# harness sets aside rather than a case tclrs is measured on: the number would
# be built out of nothing.
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

package require Tk
wm withdraw .

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
    # Tk into the child, so the case has a main window and the widget commands.
    # A child that cannot have one — the display is gone, or Tk refuses a
    # second toplevel — leaves the case to fail here, and the harness then
    # records it as having no reference outcome rather than as tclrs's fault.
    catch {load {} Tk $child}
    catch {$child eval {wm withdraw .}}
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
# A tclsh with Tk loaded does not end when its script does.
exit 0
