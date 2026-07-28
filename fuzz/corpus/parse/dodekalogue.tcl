set x 5
puts "$x [expr {$x + 1}] é \x41 \\"
set l {a {b c} d}
foreach {k v} $l {puts $k=$v}
puts a;# trailing comment
set q "a
b"
