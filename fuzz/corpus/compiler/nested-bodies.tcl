proc f {x {y 2} args} {
    foreach v $x {
        if {[catch {expr {$v / 0}} e]} {continue}
        switch -glob -- $v {a* {return $v} default {break}}
    }
    return [list $y {*}[dict keys [dict create k v]]]
}
puts [catch {f {a b}} r]
