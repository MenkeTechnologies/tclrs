if {1 * 0} {expr {1 +}; catch {puts [string index abc 10]; puts +5} m12; puts m:$m12} else {}
