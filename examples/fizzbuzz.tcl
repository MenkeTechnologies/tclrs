# FizzBuzz — a program that prints rather than only checking, so the byte-for-
# byte comparison against tclsh has real output to compare.
#
# The loop is inside a procedure: its variables are frame slots, which is what
# lets a hot loop reach a compiled trace (`tclrs --tiers fizzbuzz.tcl` reports
# which tiers a run actually reached).

proc fizzbuzz {n} {
    set out {}
    for {set i 1} {$i <= $n} {incr i} {
        if {$i % 15 == 0} {
            lappend out FizzBuzz
        } elseif {$i % 3 == 0} {
            lappend out Fizz
        } elseif {$i % 5 == 0} {
            lappend out Buzz
        } else {
            lappend out $i
        }
    }
    return $out
}

foreach word [fizzbuzz 20] {
    puts $word
}

# The same run, checked: 20 words, with the multiples where they belong.
set words [fizzbuzz 20]
if {[llength $words] != 20} {
    error "expected 20 words, got [llength $words]"
}
if {[lindex $words 14] ne "FizzBuzz"} {
    error "15 should be FizzBuzz, got [lindex $words 14]"
}
if {[lsearch -all $words Fizz] ne {2 5 8 11 17}} {
    error "Fizz positions wrong: [lsearch -all $words Fizz]"
}
if {[lsearch -all $words Buzz] ne {4 9 19}} {
    error "Buzz positions wrong: [lsearch -all $words Buzz]"
}

puts "fizzbuzz.tcl: [llength $words] words checked"
