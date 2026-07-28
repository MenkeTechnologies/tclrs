set s2 3
proc p4 {} {set __o {}; return $__o}
if {2 le $s2 - "abc"} {}
switch -exact -- a {a*b*c {if {2 ? (8) : 65535} {puts [p4 2]; puts [lreplace $s3 5 -1 {}]}; while {$w19 < 5} {incr w19; puts [string length {}]}; puts [dict remove $d10 abc]}}
