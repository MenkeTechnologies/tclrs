set d3 [dict create b end {} c1]
switch -- "pre-$d3-post" {{[ab]*} {puts -nonewline [expr {($w5) in 日本語}]}}
