set s2 {}
if {1.5 in {$} + [llength $s2]} {switch -exact a*b {a* {puts [string index end-1 end-1]} *b {eval {puts b}} default {unset s2}}; switch -exact b {a* {puts [format %s 7]; append s1 xyz -1} a* {puts [string index "x" 7]} default {}}}
