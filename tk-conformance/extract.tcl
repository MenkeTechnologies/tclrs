# Mechanically extract every test case from one official Tk suite file.
#
# A port of `conformance/extract.tcl` to the Tk suite. Everything about the
# lifting is the same and the reasoning there applies unchanged: the real
# tcltest is loaded, only `::tcltest::test` is replaced by a recorder, the
# recorder is tcltest's own argument parsing, constraint state comes from
# tcltest's own evaluation, and no file and no case is chosen by hand.
#
# Three things differ, and all three are forced by the suite being Tk's:
#
#  * `package require Tk` before the recorder is installed. Every Tk suite file
#    opens with `package require tk`, and `tests/constraints.tcl` — sourced by
#    most of them — calls `tk windowingsystem`, `winfo`, `font`, `image` and
#    `childTkProcess` at the *file's top level*, so a Tk that is not loaded is
#    not an extraction with fewer cases but no extraction at all.
#  * an explicit `exit` at the end. A tclsh with Tk loaded does not return from
#    the end of its script the way a plain one does.
#  * `tests/` on the module path, because the 9.0 suite factors its helpers into
#    a `testutils` module each file imports (`tests/testutils.tcl`).
#
# Usage: tclsh tkextract.tcl <suite-tests-dir> <file.test> <out.cases> <scratch>
#
# Output: one record per line, tab separated, every field base64 of UTF-8:
#   <index> <name> <constraints> <constraint-skipped 0|1> <state> <setup> <body>
# followed by a final status line:
#   # status <ok|error> <base64 message> <count> <child-interps>

# Resolved before the [cd] below, so relative paths on the command line keep
# meaning what they meant to the caller.
set suiteDir [file normalize [lindex $argv 0]]
set testFile [file normalize [lindex $argv 1]]
set outFile  [file normalize [lindex $argv 2]]
set scratch  [file normalize [lindex $argv 3]]

file mkdir $scratch
cd $scratch

# The suite's own helper module, which every 9.0 test file imports.
if {[lsearch -exact [tcl::tm::path list] $suiteDir] < 0} {
    tcl::tm::path add $suiteDir
}

package require tcltest 2.5
# Tk before the recorder: `constraints.tcl` runs `tk windowingsystem` and opens
# a child Tk process while the file is being sourced.
package require Tk
::tcltest::configure -testdir $suiteDir -tmpdir $scratch

set ::EXTRACT_OUT [open $outFile w]
fconfigure $::EXTRACT_OUT -translation lf -encoding utf-8
set ::EXTRACT_N 0

proc ::EXTRACT_b64 {s} {
    return [binary encode base64 [encoding convertto -profile tcl8 utf-8 $s]]
}

proc ::EXTRACT_record {name constraints skipped state setup body} {
    puts $::EXTRACT_OUT [join [list \
	    $::EXTRACT_N \
	    [::EXTRACT_b64 $name] \
	    [::EXTRACT_b64 $constraints] \
	    $skipped \
	    [::EXTRACT_b64 $state] \
	    [::EXTRACT_b64 $setup] \
	    [::EXTRACT_b64 $body]] "\t"]
    incr ::EXTRACT_N
    flush $::EXTRACT_OUT
}

# The ambient global variables a suite file has built up by the time it reaches
# a given test.
#
# Test files set variables at their top level and inside helper procs, then
# write bodies that read them — `list.test` has a proc that assigns a global
# `d` and immediately declares three tests that index it. A body lifted out on
# its own would read an unset variable and measure nothing. So each case
# carries the variables its file created, replayed as `set` and `array set`
# commands ahead of the body.
#
# The set is a difference against a baseline taken before the file is sourced,
# so the interpreter's own globals and tcltest's are not dragged along: only
# what the file itself created or changed.
#
# Procs are deliberately not replayed. A body that calls a helper proc fails
# under tclsh too and the harness skips it as needing an unavailable command;
# replaying the definitions would instead make every such case a `proc` case,
# which measures the wrong thing entirely.
proc ::EXTRACT_snapshot {into} {
    upvar 1 $into snap
    array unset snap
    foreach name [info globals] {
	if {[string match EXTRACT_* $name]} continue
	# These two are rewritten by every caught error, so they carry no
	# information about the file and would only add noise.
	if {$name in {errorInfo errorCode}} continue
	if {[array exists ::$name]} {
	    catch {set snap($name) [list array [array get ::$name]]}
	} else {
	    catch {set snap($name) [list scalar [set ::$name]]}
	}
    }
}

proc ::EXTRACT_state {setup body} {
    ::EXTRACT_snapshot now
    set text "$setup\n$body"
    set script ""
    foreach name [lsort [array names now]] {
	if {[info exists ::EXTRACT_BASE($name)] \
		&& $::EXTRACT_BASE($name) eq $now($name)} {
	    continue
	}
	# Only variables the case can actually reach. Without this a file that
	# builds a large table at its top level — cmdAH.test builds several —
	# would attach a copy of it to every one of its cases, and cmdAH alone
	# extracts seventeen thousand. The test is on the name appearing in the
	# text, so it errs towards carrying too much rather than too little,
	# and whatever it leaves out is left out of both runs alike.
	if {[string first $name $text] < 0} continue
	lassign $now($name) kind value
	if {$kind eq "array"} {
	    append script [list array set $name $value] "\n"
	} else {
	    append script [list set $name $value] "\n"
	}
    }
    return $script
}

proc ::EXTRACT_finish {status message} {
    puts $::EXTRACT_OUT "# status $status [::EXTRACT_b64 $message]\
	    $::EXTRACT_N $::EXTRACT_CHILDREN"
    close $::EXTRACT_OUT
}

# The recorder only sees `test` calls made in this interpreter. A handful of
# suite files — package.test and the safe-interpreter ones — declare their
# tests inside a child interpreter instead, and those calls go to whatever
# `test` exists in the child, not here. Counting the child interpreters a file
# creates is what lets the report name the files whose case count is a floor
# rather than a total, without anyone having to keep a list up to date.
set ::EXTRACT_CHILDREN 0
rename ::interp ::EXTRACT_real_interp
proc ::interp {args} {
    if {[lindex $args 0] eq "create"} {
	incr ::EXTRACT_CHILDREN
    }
    return [::EXTRACT_real_interp {*}$args]
}

# A test file that calls [exit] — several do, on a failed [package require] —
# must not take the extraction with it. What was recorded so far is kept.
rename ::exit ::EXTRACT_real_exit
proc ::exit {{code 0}} {
    ::EXTRACT_finish exit "the file called exit $code"
    ::EXTRACT_real_exit 0
}

# Reporting and whole-suite driving are the harness's job here, not tcltest's.
proc ::tcltest::cleanupTests {args} {}
proc ::tcltest::runAllTests {args} {}

# The recorder. The parsing below is tcltest::test's own, minus the parts that
# run the test.
proc ::tcltest::test {name description args} {
    variable testLevel

    incr testLevel
    lassign {} constraints setup cleanup body result returnCodes errorCode match
    set match exact
    set returnCodes [list 0 2]
    set errorCode "*"

    if {[string match -* [lindex $args 0]] || ([llength $args] <= 1)} {
	if {[llength $args] == 1} {
	    set list [SubstArguments [lindex $args 0]]
	    foreach {element value} $list {
		set testAttributes($element) $value
	    }
	    foreach item {constraints match setup body cleanup \
		    result returnCodes errorCode output errorOutput} {
		if {[info exists testAttributes(-$item)]} {
		    set testAttributes(-$item) [uplevel 1 \
			    ::concat $testAttributes(-$item)]
		}
	    }
	} else {
	    array set testAttributes $args
	}
	foreach item [array names testAttributes] {
	    set [string trimleft $item "-"] $testAttributes($item)
	}
    } else {
	set result [lindex $args end]
	if {[llength $args] == 2} {
	    set body [lindex $args 0]
	} elseif {[llength $args] == 3} {
	    set constraints [lindex $args 0]
	    set body [lindex $args 1]
	} else {
	    incr testLevel -1
	    return
	}
    }

    # tcltest's own constraint evaluation decides whether this configuration
    # can run the test at all. Its answer is recorded, not second-guessed.
    set skipped 0
    catch {set skipped [::tcltest::Skipped $name $constraints]}
    incr testLevel -1

    ::EXTRACT_record $name $constraints $skipped \
	    [::EXTRACT_state $setup $body] $setup $body
    return
}

# Every suite file opens with
#     if {"::tcltest" ni [namespace children]} {
#         package require tcltest 2.5
#         namespace import -force ::tcltest::*
#     }
# and a real run reaches that branch, because tcltest sources each file in a
# fresh interpreter. Here the package is already loaded, so the branch is dead
# and the import has to be done on the file's behalf — after the recorder is in
# place, so that the imported `test` is the recorder.
# A suite file is normally run as `tclsh thefile.test`, and a few of them hand
# their own command line to `::tcltest::configure`. The extractor's arguments
# have all been read into variables by now, so the file gets the command line it
# expects instead of ours.
set ::argv0 $testFile
set ::argv {}
set ::argc 0

::EXTRACT_snapshot ::EXTRACT_BASE

namespace import -force ::tcltest::*

if {[catch {uplevel #0 [list source $testFile]} msg]} {
    ::EXTRACT_finish error $msg
} else {
    ::EXTRACT_finish ok {}
}

# With Tk loaded this process does not end when the script does.
::EXTRACT_real_exit 0
