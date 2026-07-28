proc p6 {{o1 1000}} {set __o {}; return $__o}
catch {if {1 & 3} {puts [p6 255 $v10]}} m11
