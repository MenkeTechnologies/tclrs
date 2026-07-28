# Strings: the `string` ensemble and `format`.

set checks 0

proc check {label got want} {
    global checks
    incr checks
    if {$got ne $want} {
        error "$label: expected \"$want\" but got \"$got\""
    }
}

# Length and indexing count characters, not bytes.
check "length" [string length "hello"] 5
check "length unicode" [string length "héllo"] 5
check "index" [string index "hello" 1] e
check "index end" [string index "hello" end] o
check "range" [string range "hello world" 6 end] world

# Case conversion.
check "toupper" [string toupper "hello"] HELLO
check "tolower" [string tolower "HeLLo"] hello
check "totitle" [string totitle "hello world"] "Hello world"

# Comparison and equality, with -nocase where the ensemble takes it.
check "compare less" [string compare abc abd] -1
check "compare equal" [string compare abc abc] 0
check "compare greater" [string compare abd abc] 1
check "equal" [string equal abc abc] 1
check "equal nocase" [string equal -nocase ABC abc] 1

# Searching.
check "first" [string first "lo" "hello"] 3
check "first missing" [string first "z" "hello"] -1
check "last" [string last "l" "hello"] 3

# Building and rewriting.
check "cat" [string cat a b c] abc
check "repeat" [string repeat ab 3] ababab
check "reverse" [string reverse abc] cba
check "insert" [string insert "abd" 2 c] abcd
check "replace" [string replace "abcd" 1 2 X] aXd
check "map" [string map {a 1 b 2} abcab] 12c12

# Trimming.
check "trim" [string trim "  spaced  "] spaced
check "trimleft" [string trimleft "xxabc" x] abc
check "trimright" [string trimright "abc..." .] abc

# Glob matching and classification.
check "match" [string match {h*o} hello] 1
check "match no" [string match {h?o} hello] 0
check "match nocase" [string match -nocase {HEL*} hello] 1
check "is integer" [string is integer 42] 1
check "is integer no" [string is integer 4x2] 0
check "is alpha" [string is alpha abc] 1
check "is space" [string is space " \t"] 1

# format has sprintf's conversions with Tcl's argument rules.
check "format string" [format "%s-%s" a b] a-b
check "format integer" [format "%05d" 42] 00042
check "format width" [format "%-6s|" ab] "ab    |"
check "format float" [format "%.3f" 3.14159] 3.142
check "format hex" [format "%x" 255] ff
check "format hex upper" [format "%X" 255] FF
check "format percent" [format "100%%"] 100%
check "format char" [format "%c" 65] A

# Composing the two: a fixed-width table row.
set row [format "%-8s %5s" [string totitle apples] [format "%d" 12]]
check "table row" $row "Apples      12"

puts "strings.tcl: $checks checks passed"
