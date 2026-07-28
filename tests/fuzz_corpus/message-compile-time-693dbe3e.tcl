set s2 7
switch -glob -- [expr {$s2 ^ 2}] {a*b*c {set; catch {incr s2 0; incr f7 2} m11; puts m:$m11} default {puts [lsort [array names a5]]}}; catch {if {100 le 0} {puts [array size a5]} else {puts [string index x 1]}; while {$w13 < 1} {}} m12
