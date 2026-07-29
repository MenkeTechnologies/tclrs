//! Differential execution for Tcl's string handling: the `string` ensemble,
//! `append`, and `format`.
//!
//! Same contract as `execution_differential`: every program is run by both
//! tclsh and tclrs and the two outputs are compared byte for byte, so nothing
//! here is an expectation written by hand. That matters most for the parts of
//! `string` where the documentation is thin — the index arithmetic, the glob
//! matcher's treatment of `[`, Tcl's simple case mappings — and for `format`,
//! whose integer truncation and `%g` layout are inherited from C.
//!
//! The character-class and case-conversion sweeps loop over code points with
//! `format %c` so that thousands of characters are compared per program rather
//! than a handful of hand-picked ones.

use std::path::PathBuf;
use std::process::Command;

const PROGRAMS: &[&str] = &[
    // ── string length, index, range ──────────────────────────────────────
    "puts [string length abcdef]",
    "puts [string length {}]",
    "puts [string length héllo]",
    "puts [string length a\u{1F600}b]",
    "puts [string index abcd 0]",
    "puts [string index abcd end]",
    "puts [string index abcd end-1]",
    "puts [string index abcd end+1]",
    "puts [string index abcd end+-1]",
    "puts [string index abcd end-0]",
    "puts [string index abcd 1+1]",
    "puts [string index abcd 2-1]",
    "puts [string index abcd 1--1]",
    "puts [string index abcd 1+-1]",
    "puts [string index abcd -1]",
    "puts [string index abcd 10]",
    "puts [string index abcd 0x2]",
    "puts [string index abcd 0o3]",
    "puts [string index abcd 0b10]",
    "puts [string index abcd 000002]",
    "puts [string index abcd { 2 }]",
    "puts [string index abcd { 1+1 }]",
    "puts [string index abcd end-0x1]",
    "puts [string index abcd 1_0]",
    "puts [string index {} end]",
    "puts [string index héllo 1]",
    "puts [string index a\u{1F600}b 1]",
    "puts [string range abcdef 1 3]",
    "puts [string range abcdef 0 end]",
    "puts [string range abcdef 3 1]",
    "puts [string range abcdef -5 2]",
    "puts [string range abcdef 2 100]",
    "puts [string range abcdef end-2 end]",
    "puts [string range abcdef end end-1]",
    "puts [string range abcdef 0 -1]",
    "puts [string range héllo 1 3]",
    // ── comparison ───────────────────────────────────────────────────────
    "puts [string compare abc abd]",
    "puts [string compare abd abc]",
    "puts [string compare abc abc]",
    "puts [string compare {} a]",
    "puts [string compare a {}]",
    "puts [string compare -nocase ABC abc]",
    "puts [string compare -nocase abc ABD]",
    "puts [string compare -length 2 abc abd]",
    "puts [string compare -length -1 abc abd]",
    "puts [string compare -length 0 abc abd]",
    "puts [string compare -length 10 abc abd]",
    "puts [string compare -nocase -length 2 ABc abD]",
    "puts [string compare -length 2 -nocase ABC abd]",
    "puts [string compare é e]",
    "puts [string compare a\u{1F600} az]",
    "puts [string equal abc abc]",
    "puts [string equal abc abd]",
    "puts [string equal -nocase ABC abc]",
    "puts [string equal -length 2 abc abd]",
    "puts [string equal -length 10 abc abc]",
    "puts [string equal {} {}]",
    "set a foo\nset b FOO\nputs [string equal -nocase $a $b]",
    // ── first and last ───────────────────────────────────────────────────
    "puts [string first a 0a23456789abcdef 5]",
    "puts [string first a 0123456789abcdef 11]",
    "puts [string first {} abc]",
    "puts [string first abc {}]",
    "puts [string first a abcabc]",
    "puts [string first a abcabc 1]",
    "puts [string first a abcabc end]",
    "puts [string first a abcabc end-3]",
    "puts [string first a abcabc -5]",
    "puts [string first ab abab 1]",
    "puts [string first \u{1F600} a\u{1F600}b]",
    "puts [string last a 0a23456789abcdef 15]",
    "puts [string last a 0a23456789abcdef 9]",
    "puts [string last a abcabc]",
    "puts [string last a abcabc 0]",
    "puts [string last a abcabc -1]",
    "puts [string last a abcabc end-3]",
    "puts [string last {} abc]",
    // ── insert, replace, repeat, reverse ─────────────────────────────────
    "puts [string insert abcdef 2 XY]",
    "puts [string insert abcdef 0 X]",
    "puts [string insert abcdef end X]",
    "puts [string insert abcdef end-1 X]",
    "puts [string insert abcdef 100 X]",
    "puts [string insert abcdef -5 X]",
    "puts [string replace abcdef 1 3]",
    "puts [string replace abcdef 1 3 XY]",
    "puts [string replace abcdef 0 end X]",
    "puts [string replace abcdef 3 1 X]",
    "puts [string replace abcdef -3 1 X]",
    "puts [string replace abcdef 4 100 X]",
    "puts [string replace abcdef 6 7 X]",
    "puts [string replace abcdef 0 -1 X]",
    "puts [string replace {} 0 0 X]",
    "puts [string replace abc 1 1]",
    "puts [string repeat ab 3]",
    "puts [string repeat ab 0]",
    "puts [string repeat ab -1]",
    "puts [string repeat {} 5]",
    "puts [string repeat - 20]",
    "puts [string reverse abc]",
    "puts [string reverse a\u{1F600}b]",
    "puts [string reverse {}]",
    "puts [string cat a b c]",
    "puts [string cat]",
    "puts [string cat abc]",
    // ── trimming ─────────────────────────────────────────────────────────
    "puts <[string trim {  ab  }]>",
    "puts <[string trim xxabxx x]>",
    "puts <[string trimleft xxabxx x]>",
    "puts <[string trimright xxabxx x]>",
    "puts <[string trim abc {}]>",
    "puts <[string trim \"\\t\\n ab \\r\\n\"]>",
    "puts <[string trim abcba abc]>",
    "puts <[string trim abcabc abc]>",
    "puts <[string trimleft aXbXc abc]>",
    "puts <[string trimright abc {}]>",
    "puts <[string trim {} {}]>",
    "puts <[string trim \u{200b}ab\u{200b}]>",
    "puts <[string trim \u{feff}ab]>",
    "puts <[string trim \u{a0}ab\u{3000}]>",
    "puts [string length [string trim ab\u{0}]]",
    // ── case conversion ──────────────────────────────────────────────────
    "puts [string tolower ABC]",
    "puts [string toupper abc]",
    "puts [string totitle {hello world}]",
    "puts [string totitle HELLO]",
    "puts [string tolower ABCDEF 1]",
    "puts [string tolower ABCDEF 1 3]",
    "puts [string toupper abcdef 1 3]",
    "puts [string totitle abcdef 1 3]",
    "puts [string tolower ABCDEF end-1 end]",
    "puts [string tolower ABCDEF 10]",
    "puts [string tolower ABCDEF -5]",
    "puts [string tolower ABCDEF 3 1]",
    "puts [string tolower ABCDEF -5 2]",
    "puts [string tolower ABCDEF 2 100]",
    "puts [string totitle {} 0]",
    "puts [string toupper abcdef end end]",
    "puts [string toupper ﬁx]",
    "puts [string tolower İ]",
    "puts [string totitle ǳx]",
    "puts [string totitle Ǳ]",
    "puts [string toupper ß]",
    "puts [string tolower ẞ]",
    "puts [string totitle {élan vital}]",
    "puts [string toupper ᾀ]",
    "puts [string totitle ᾀ]",
    "puts [string totitle ა]",
    "puts [string toupper ა]",
    "puts [string totitle {ⴀⴁ}]",
    "puts [string toupper ɐ]",
    // ── glob matching ────────────────────────────────────────────────────
    "puts [string match a*b axxb]",
    "puts [string match a*b ab]",
    "puts [string match a*b a]",
    "puts [string match {a?c} abc]",
    "puts [string match {a?c} ac]",
    "puts [string match {[abc]x} bx]",
    "puts [string match {[a-c]x} bx]",
    "puts [string match {[a-c]x} dx]",
    "puts [string match {[A-z]} _]",
    "puts [string match -nocase {[A-z]} _]",
    "puts [string match {[b-a]} a]",
    "puts [string match {[a-]} -]",
    "puts [string match {[]a]} a]",
    "puts [string match {[^a]} b]",
    "puts [string match {[^a]} ^]",
    "puts [string match {\\*} *]",
    "puts [string match {\\*} a]",
    "puts [string match * {}]",
    "puts [string match {} {}]",
    "puts [string match {} a]",
    "puts [string match {a[b} {a[b}]",
    "puts [string match {[a} {[a}]",
    "puts [string match -nocase ABC abc]",
    "puts [string match {*\u{1F600}*} x\u{1F600}y]",
    "puts [string match {**a} xa]",
    "puts [string match {a**} ax]",
    "puts [string match {*[ab]*} zbz]",
    "puts [string match -nocase {*ABC*} xxabcxx]",
    "set p {a*}\nputs [string match $p abc]",
    // ── string map ───────────────────────────────────────────────────────
    "puts [string map {abc 1 ab 2 a 3 1 0} 1abcaababcabababc]",
    "puts [string map {1 0 ab 2 a 3 abc 1} 1abcaababcabababc]",
    "puts [string map {a b} aaa]",
    "puts [string map -nocase {A b} aaa]",
    "puts [string map {} abc]",
    "puts [string map {{} x} abc]",
    "puts [string map {a {}} abc]",
    "puts [string map {ab AB} abab]",
    "puts [string map {a b b a} ab]",
    "puts [string map {a b c d} abc]",
    "puts [string map {{a b} X} {a b}]",
    "puts [string map {\\  _} {a b}]",
    "puts [string map {é E} héllo]",
    "puts [string map {abc X} ab]",
    "puts [string map {aa X} aaa]",
    "puts [string map -nocase {aB X} {ab AB Ab}]",
    "puts [string map {\\u0041 X} A]",
    "puts [string map {{a\\ b} X} {a b}]",
    "puts [string map \"a\\\\ b x\" {a b}]",
    // ── string is ────────────────────────────────────────────────────────
    "puts [string is integer 5]",
    "puts [string is integer { 5 }]",
    "puts [string is integer 0x10]",
    "puts [string is integer 0b101]",
    "puts [string is integer 0o17]",
    "puts [string is integer 0d10]",
    "puts [string is integer 010]",
    "puts [string is integer {}]",
    "puts [string is integer -strict {}]",
    "puts [string is integer +5]",
    "puts [string is integer 5.0]",
    "puts [string is integer 99999999999999999999999999]",
    "puts [string is integer {5 5}]",
    "puts [string is integer -]",
    "puts [string is integer 1_0]",
    "puts [string is integer 1__0]",
    "puts [string is integer 1_]",
    "puts [string is integer _1]",
    "puts [string is integer 0x_1]",
    "puts [string is integer 0x1_]",
    "puts [string is integer 1e2]",
    "puts [string is wideinteger 5]",
    "puts [string is wideinteger 99999999999999999999999999]",
    "puts [string is wideinteger 9223372036854775807]",
    "puts [string is wideinteger 9223372036854775808]",
    "puts [string is entier 5]",
    "puts [string is double 5]",
    "puts [string is double 5.5]",
    "puts [string is double 1e10]",
    "puts [string is double { 1.5 }]",
    "puts [string is double abc]",
    "puts [string is double Inf]",
    "puts [string is double NaN]",
    "puts [string is double {}]",
    "puts [string is double -strict {}]",
    "puts [string is double 0x10]",
    "puts [string is double .5]",
    "puts [string is double 5.]",
    "puts [string is double 1e]",
    "puts [string is double 1_0.5_5]",
    "puts [string is double 1.5_]",
    "puts [string is boolean true]",
    "puts [string is boolean TRUE]",
    "puts [string is boolean tr]",
    "puts [string is boolean t]",
    "puts [string is boolean y]",
    "puts [string is boolean o]",
    "puts [string is boolean of]",
    "puts [string is boolean on]",
    "puts [string is boolean 2]",
    "puts [string is boolean -1]",
    "puts [string is boolean 0]",
    "puts [string is boolean 00]",
    "puts [string is boolean 1.0]",
    "puts [string is boolean { 1 }]",
    "puts [string is boolean {}]",
    "puts [string is boolean -strict {}]",
    "puts [string is true yes]",
    "puts [string is true no]",
    "puts [string is false no]",
    "puts [string is false yes]",
    "puts [string is true T]",
    "puts [string is false F]",
    "puts [string is alpha abc]",
    "puts [string is alpha ab1]",
    "puts [string is alpha {}]",
    "puts [string is alpha -strict {}]",
    "puts [string is alnum ab1]",
    "puts [string is alnum {ab 1}]",
    "puts [string is digit 123]",
    "puts [string is digit 12a]",
    "puts [string is space { \t\n}]",
    "puts [string is space a]",
    "puts [string is space \u{200b}]",
    "puts [string is space \u{180e}]",
    "puts [string is space \u{2060}]",
    "puts [string is space \u{a0}]",
    "puts [string is xdigit 1aF]",
    "puts [string is xdigit 1g]",
    "puts [string is ascii abc]",
    "puts [string is ascii héllo]",
    "puts [string is upper ABC]",
    "puts [string is upper ABc]",
    "puts [string is lower abc]",
    "puts [string is wordchar ab_1]",
    "puts [string is wordchar ab-1]",
    "puts [string is control \\x01]",
    "puts [string is control a]",
    "puts [string is list {a b c}]",
    "puts [string is list {a {b c}}]",
    "puts [string is list {a \"b c\"}]",
    "puts [string is list \"a \\{b c\"]",
    "puts [string is list \"a \\\"b\"]",
    "puts [string is list {}]",
    "puts [string is list -strict {}]",
    "puts [string is list {  a  b  }]",
    "puts [string is list {a\"b}]",
    "puts [string is list {{a}b}]",
    "puts [string is li {a b}]",
    "puts [string is in 5]",
    "puts [string is list \"a\\\\{b\"]",
    "puts [string is list \"{a}b\"]",
    "puts [string is list \"\\\"a\\\"b\"]",
    "puts [string is list \"a b\\\\\\\\\"]",
    // abbreviated subcommands, classes and options
    "puts [string len abcdef]",
    "puts [string comp abc abd]",
    "puts [string tou abc]",
    "puts [string compare -len 2 abc abd]",
    "puts [string equal -noc ABC abc]",
    "puts [string is integer -str {}]",
    "puts [string is int -strict 5]",
    // index arithmetic that overflows the wide range
    "puts <[string index abcd 99999999999999999999999999]>",
    "puts <[string index abcd -99999999999999999999999999]>",
    "puts <[string index abcd end-99999999999999999999999999]>",
    "puts <[string range abcdef 99999999999999999999999999 end]>",
    "puts [string first a abcabc 99999999999999999999]",
    "puts [string last a abcabc 99999999999999999999]",
    // options and indices arriving as values rather than literals
    "set n 2\nputs [string compare -length $n abc abd]",
    "set i end-1\nputs [string index abcdef $i]",
    "set c x\nputs <[string trim xxaxx $c]>",
    // ── append ───────────────────────────────────────────────────────────
    "set q {}\nappend q a\nappend q b c\nputs $q",
    "set q x\nputs [append q y]",
    "append q a\nputs $q",
    "set n 5\nputs [append n 6]",
    "set q abc\nputs [append q]",
    "set s {}\nset i 0\nwhile {$i < 5} {append s $i-; incr i}\nputs $s",
    "set q {}\nappend q [string toupper ab] [string repeat x 2]\nputs $q",
    // ── format: integers ─────────────────────────────────────────────────
    "puts [format %d 5]",
    "puts [format %i 5]",
    "puts [format %d -1]",
    "puts [format %d 4294967296]",
    "puts [format %d 4294967295]",
    "puts [format %d 2147483648]",
    "puts [format %d 9223372036854775807]",
    "puts [format %lld 9223372036854775807]",
    "puts [format %ld 4294967296]",
    "puts [format %hd 65537]",
    "puts [format %u -1]",
    "puts [format %x -1]",
    "puts [format %llx -1]",
    "puts [format %llx 255]",
    "puts [format %llx -255]",
    "puts [format %llo -8]",
    "puts [format %llb -5]",
    "puts [format %lx -1]",
    "puts [format %hx -1]",
    "puts [format %o 8]",
    "puts [format %#o 8]",
    "puts [format %#x 255]",
    "puts [format %#X 255]",
    "puts [format %#b 5]",
    "puts [format %b 5]",
    "puts [format %#d 5]",
    "puts [format %#x 0]",
    "puts [format %#o 0]",
    "puts [format %#b 0]",
    "puts [format %#d 0]",
    "puts [format %X -255]",
    "puts [format %o -1]",
    "puts [format %b -1]",
    "puts [format %u 4294967296]",
    "puts [format %5.3d 7]",
    "puts [format %.3d -7]",
    "puts [format %.0d 0]",
    "puts [format %.0x 0]",
    "puts [format %#.0o 0]",
    "puts [format %.5x 255]",
    "puts [format %#.5x 255]",
    "puts [format %+d 7]",
    "puts [format {% d} 7]",
    "puts [format %05d 7]",
    "puts [format %+05d 7]",
    "puts [format {%-5d|} 7]",
    "puts [format {%+-5d|} 7]",
    "puts [format {%-+5d|} 7]",
    "puts [format {%-08d|} 7]",
    "puts [format %05.3d 7]",
    "puts [format %+llx 255]",
    "puts [format %+x 255]",
    "puts [format {% x} 255]",
    "puts [format %#llx -255]",
    "puts [format %#llo -8]",
    "puts [format %08x -255]",
    "puts [format %d 0x10]",
    "puts [format %d { 12 }]",
    "puts [format %d 007]",
    "puts [format %x 0x1f]",
    "puts [format %d 1_0]",
    "puts [format %c 65]",
    "puts [format %c 128512]",
    "puts [format %5c 65]",
    // ── format: strings ──────────────────────────────────────────────────
    "puts [format %s abc]",
    "puts [format %.2s abcdef]",
    "puts [format %10.2s abcdef]",
    "puts [format {%-10.2s|} abcdef]",
    "puts [format %5s ab]",
    "puts [format {%-5s|} ab]",
    "puts [format %05s ab]",
    "puts [format %s {}]",
    "puts [format %s é]",
    "puts [format %.1s éx]",
    "puts [format %3s é]",
    "puts [format {%5.2s|} abcdef]",
    "puts [format %s%s a b]",
    "puts [format abc]",
    "puts [format %%]",
    "puts [format {100%%}]",
    "puts [format %s 1.0]",
    "puts [format %s 007]",
    "puts [format {%s} [expr {1.0/3}]]",
    // ── format: floating point ───────────────────────────────────────────
    "puts [format %f 1.5]",
    "puts [format %.0f 1.5]",
    "puts [format %.0f 2.5]",
    "puts [format %f 0]",
    "puts [format %f -0.0]",
    "puts [format %e 1234.5678]",
    "puts [format %E 1234.5678]",
    "puts [format %g 1234.5678]",
    "puts [format %G 0.000012345]",
    "puts [format %.3g 1234.5678]",
    "puts [format %#g 1.5]",
    "puts [format %#.0f 1.5]",
    "puts [format %g 100000]",
    "puts [format %g 1000000]",
    "puts [format %g 0.0001]",
    "puts [format %g 0.00001]",
    "puts [format %g 0]",
    "puts [format %.0g 123]",
    "puts [format %.0g 0.0001]",
    "puts [format %.1g 0]",
    "puts [format %e 0]",
    "puts [format %08.3f -1.5]",
    "puts [format %+.3e 0.0]",
    "puts [format %.2f 2.675]",
    "puts [format %.1f 0.05]",
    "puts [format %.20f 0.1]",
    "puts [format %.3e 9.9995]",
    "puts [format %e 1e-300]",
    "puts [format %g 123456789]",
    "puts [format %.10g 123456789]",
    "puts [format %G 1e20]",
    "puts [format %f 1e-7]",
    "puts [format %.0e 1234]",
    "puts [format %#.3g 1.0]",
    "puts [format %.17g 0.1]",
    "puts [format %20.10f 1.5]",
    "puts [format {%-20.10f|} 1.5]",
    "puts [format %5.0f 1.5]",
    "puts [format {%#.0e} 1.5]",
    "puts [format %#e 1.5]",
    "puts [format %08.2e 1.5]",
    "puts [format {% .3f} 1.5]",
    "puts [format %.100f 0.1]",
    "puts [format %f 1e300]",
    "puts [format %e inf]",
    "puts [format %f inf]",
    "puts [format %E inf]",
    "puts [format %G -inf]",
    "puts [format %+f inf]",
    "puts [format %10f inf]",
    "puts [format {%-10f|} inf]",
    "puts [format %010f inf]",
    // ── format: widths, precisions, positions ────────────────────────────
    "puts [format {%*d} 5 42]",
    "puts [format {%*d} -5 42]",
    "puts [format {%.*f} 2 3.14159]",
    "puts [format {%.*f} -2 3.14159]",
    "puts [format {%2$s %1$s} a b]",
    "puts [format {%1$s %1$s} x]",
    "puts [format {%2$s equity (%3$.2f x %1$d)} 123 BigCorp 19.37]",
    "puts [format {%1$s-%2$s} a b]",
    "puts [format %s%s%s a b c]",
    "set w 8\nputs [format {[%*s]} $w hi]",
    // ── the pieces working together ──────────────────────────────────────
    "set w 6\nset i 0\nwhile {$i < 4} {puts [format {|%*d|%-*s|} $w $i $w [string repeat x $i]]; incr i}",
    "set sep +-[string repeat - 5]-+\nputs $sep",
    "set s {}\nset i 0\nwhile {$i < 5} {append s [string index abcdefgh [expr {$i*2}]]; incr i}\nputs $s",
    "puts [string toupper [format %s-%s [string trim { a }] [string reverse cba]]]",
    "set n 0\nset i 0\nwhile {$i < 20} {if {[string match {*[aeiou]*} [string index abcdefghij $i]]} {incr n}; incr i}\nputs $n",
    // ── code-point sweeps ────────────────────────────────────────────────
    // Character classes, over the range where the two implementations agree.
    "set i 1\nwhile {$i < 128} {puts -nonewline [string is alpha -strict [format %c $i]]; incr i}\nputs {}",
    "set i 1\nwhile {$i < 128} {puts -nonewline [string is alnum -strict [format %c $i]]; incr i}\nputs {}",
    "set i 1\nwhile {$i < 128} {puts -nonewline [string is digit -strict [format %c $i]]; incr i}\nputs {}",
    "set i 1\nwhile {$i < 128} {puts -nonewline [string is lower -strict [format %c $i]]; incr i}\nputs {}",
    "set i 1\nwhile {$i < 128} {puts -nonewline [string is upper -strict [format %c $i]]; incr i}\nputs {}",
    "set i 1\nwhile {$i < 128} {puts -nonewline [string is control -strict [format %c $i]]; incr i}\nputs {}",
    "set i 1\nwhile {$i < 128} {puts -nonewline [string is wordchar -strict [format %c $i]]; incr i}\nputs {}",
    "set i 1\nwhile {$i < 128} {puts -nonewline [string is xdigit -strict [format %c $i]]; incr i}\nputs {}",
    // Whitespace and the ASCII test run over the whole Basic Multilingual
    // Plane below the surrogates, where neither needs category tables.
    "set i 1\nwhile {$i < 55296} {puts -nonewline [string is space -strict [format %c $i]]; incr i}\nputs {}",
    "set i 1\nwhile {$i < 55296} {puts -nonewline [string is ascii -strict [format %c $i]]; incr i}\nputs {}",
    // Case conversion, over the same range and then over the supplementary
    // planes that hold Deseret and Adlam.
    "set i 1\nwhile {$i < 55296} {puts -nonewline [string toupper [format %c $i]]; incr i}\nputs {}",
    "set i 1\nwhile {$i < 55296} {puts -nonewline [string tolower [format %c $i]]; incr i}\nputs {}",
    "set i 1\nwhile {$i < 55296} {puts -nonewline [string totitle [format %c $i]]; incr i}\nputs {}",
    "set i 57344\nwhile {$i < 70000} {puts -nonewline [string toupper [format %c $i]]; incr i}\nputs {}",
    "set i 57344\nwhile {$i < 70000} {puts -nonewline [string tolower [format %c $i]]; incr i}\nputs {}",
    "set i 125184\nwhile {$i < 125252} {puts -nonewline [string totitle [format %c $i]]; incr i}\nputs {}",
    // Trimming and length over the same span, which exercises the default
    // trim set and the code-point counting together.
    "set i 1\nwhile {$i < 55296} {puts -nonewline [string length [string trim x[format %c $i]x]]; incr i}\nputs {}",
    // Index arithmetic over a moving string.
    "set i -3\nwhile {$i < 9} {puts -nonewline <[string index abcdef $i]>; incr i}\nputs {}",
    "set i 0\nwhile {$i < 8} {puts -nonewline <[string range abcdef $i end-$i]>; incr i}\nputs {}",
    // Every integer conversion over a spread of values.
    "set i -3\nwhile {$i < 4} {puts [format {%d %i %o %x %X %b %u} $i $i $i $i $i $i $i]; incr i}",
    // Rounding behaviour across a whole decade.
    "set i 0\nwhile {$i < 40} {puts [format {%.1f %.2e %g} [expr {$i/8.0}] [expr {$i/8.0}] [expr {$i/8.0}]]; incr i}",
];

fn tclsh() -> Option<PathBuf> {
    for name in ["tclsh", "tclsh9.0", "tclsh8.6"] {
        if let Ok(out) = Command::new("sh")
            .arg("-c")
            .arg(format!("command -v {name}"))
            .output()
        {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }
    }
    None
}

fn reference_output(tclsh: &PathBuf, index: usize, program: &str) -> String {
    let path =
        std::env::temp_dir().join(format!("tclrs-string-{}-{index}.tcl", std::process::id()));
    std::fs::write(&path, program).expect("write program");
    let out = Command::new(tclsh).arg(&path).output().expect("run tclsh");
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "tclsh rejected program:\n{program}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Keep a divergence report readable when the program sweeps thousands of
/// characters: show where the two first differ rather than both outputs.
fn divergence(expected: &str, actual: &str) -> String {
    let (e, a): (Vec<char>, Vec<char>) = (expected.chars().collect(), actual.chars().collect());
    let at = (0..e.len().min(a.len()))
        .find(|&i| e[i] != a[i])
        .unwrap_or(e.len().min(a.len()));
    let window = |s: &[char]| -> String {
        s.iter()
            .skip(at.saturating_sub(20))
            .take(60)
            .collect::<String>()
            .escape_debug()
            .to_string()
    };
    format!(
        "first differ at char {at} (tclsh {} chars, tclrs {} chars)\n  tclsh: {}\n  tclrs: {}",
        e.len(),
        a.len(),
        window(&e),
        window(&a)
    )
}

#[test]
fn string_handling_matches_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };

    let mut failures = Vec::new();
    for (i, program) in PROGRAMS.iter().enumerate() {
        let expected = reference_output(&tclsh, i, program);
        match tclrs::eval(program) {
            Ok(outcome) if outcome.output == expected => {}
            Ok(outcome) => failures.push(format!(
                "program:\n{program}\n  {}",
                divergence(&expected, &outcome.output)
            )),
            Err(e) => failures.push(format!(
                "program:\n{program}\n  tclsh: {:?}\n  tclrs failed: {e}",
                expected.chars().take(60).collect::<String>()
            )),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} programs diverge:\n\n{}",
        failures.len(),
        PROGRAMS.len(),
        failures.join("\n\n")
    );
}

/// `append`, and every `set x "$x…"` lowered as one, extends the string the
/// variable already holds instead of building a copy of it. The cases below are
/// what that path can get wrong and a single append cannot, because each needs
/// either a *sequence* of appends or a second holder of the same value:
///
/// * a value another variable is holding, which must not change under it;
/// * the read order — `append`'s arguments are evaluated before it reads the
///   variable, and a word's `$x` is read before the parts after it, so
///   `append x [set x y]` and `set x "$x[set x y]"` do not agree, and neither
///   may be lowered as the other;
/// * a variable appended to itself, where the value is held twice;
/// * a frame slot, and a global reached from inside a procedure;
/// * a variable that is not a string yet, and one that does not exist yet.
#[test]
fn append_in_place_matches_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };

    let program = concat!(
        // A held value must not change under its holder, either way in.
        "set g {}\n",
        "append g hello\n",
        "set held $g\n",
        "append g \" world\"\n",
        "puts \"<$g> <$held>\"\n",
        "set again $g\n",
        "set g \"$g!\"\n",
        "puts \"<$g> <$again>\"\n",
        // Read order: the argument runs before `append` reads the variable, and
        // the word's own `$p` is read before the part after it.
        "set t start\n",
        "append t [set t middle]\n",
        "puts <$t>\n",
        "set p abc\n",
        "set p \"$p[set p X]\"\n",
        "puts <$p>\n",
        // A variable appended to itself.
        "set d ab\n",
        "set d \"$d$d\"\n",
        "append d $d\n",
        "puts <$d>\n",
        // Growing in a loop, both spellings, and the value each yields.
        "set a {}\n",
        "set b {}\n",
        "for {set i 0} {$i < 40} {incr i} {\n",
        "    append a $i,\n",
        "    set b \"$b$i,\"\n",
        "}\n",
        "puts \"[string length $a] [string equal $a $b]\"\n",
        "puts <[append a end]>\n",
        // Frame slots, and a global reached from inside a procedure.
        "proc grow {n} {\n",
        "    set out {}\n",
        "    for {set i 0} {$i < $n} {incr i} { set out \"$out$i\" }\n",
        "    return $out\n",
        "}\n",
        "puts <[grow 12]>\n",
        "set acc {}\n",
        "proc add {x} {\n",
        "    global acc\n",
        "    append acc $x\n",
        "    return $acc\n",
        "}\n",
        "add p\n",
        "puts \"<[add q]> <$acc>\"\n",
        // A variable that is not a string yet, and mid-word text.
        "set n 5\n",
        "append n 6\n",
        "puts <$n>\n",
        "set m 1\n",
        "set m \"${m}x${m}y\"\n",
        "puts <$m>\n",
        // A variable that does not exist yet.
        "append fresh a b\n",
        "puts <$fresh>\n",
    );

    let expected = reference_output(&tclsh, usize::MAX, program);
    let outcome = tclrs::eval(program).expect("tclrs runs the program");
    assert_eq!(
        outcome.output,
        expected,
        "append diverges from tclsh: {}",
        divergence(&expected, &outcome.output)
    );
}

/// What the crate cannot do faithfully must say so rather than answer wrongly.
#[test]
fn unsupported_string_features_are_refused() {
    for (src, expected) in [
        // `wordend`, `wordstart` and `is dict` are implemented; what a word is
        // made of still rests on Unicode categories, so the two word
        // subcommands refuse beyond ASCII exactly as `string is alpha` does.
        (
            "puts [string wordend héllo 1]",
            "beyond ASCII need Unicode category tables",
        ),
        (
            "puts [string is graph abc]",
            "needs Unicode category tables",
        ),
        (
            "puts [string is integer -failindex v 12a]",
            "-failindex option",
        ),
        (
            "puts [string is alpha héllo]",
            "beyond ASCII need Unicode category tables",
        ),
        ("puts [format %a 1.5]", "is not supported yet"),
        ("puts [format %p 255]", "is not supported yet"),
        ("puts [string nosuch a]", "unknown or ambiguous subcommand"),
        ("puts [string wor abc 1]", "unknown or ambiguous subcommand"),
        ("puts [string is nosuch a]", "bad class"),
        ("puts [string is a abc]", "ambiguous class"),
        ("puts [string compare -foo a b]", "bad option"),
        ("puts [string length]", "wrong # args"),
        ("puts [string map {a b c} abc]", "char map list unbalanced"),
        ("puts [string index abc 2.0]", "bad index"),
        ("puts [string index abc {end-end}]", "bad index"),
        ("puts [string index abc {end+2-1}]", "bad index"),
        ("puts [string index abc {1 + 1}]", "bad index"),
        ("puts [string repeat ab x]", "expected integer"),
        ("puts [format %d abc]", "expected integer"),
        ("puts [format %f abc]", "expected floating-point number"),
        ("puts [format %f NaN]", "Not a Number"),
        ("puts [format %s]", "not enough arguments"),
        ("puts [format {%1$s %s} a b]", "cannot mix"),
        ("puts [format {%3$s} a b]", "argument index out of range"),
        ("puts [format %llu -1]", "unsigned bignum"),
        ("puts [string repeat ab 99999999999]", "exceed 2 GiB"),
        ("append nosuchvariable", "no such variable"),
    ] {
        let err = tclrs::eval(src).expect_err(&format!("{src:?} should fail"));
        assert!(
            err.contains(expected),
            "{src:?}: expected an error mentioning {expected:?}, got {err:?}"
        );
    }
}

/// A command's value is what command substitution reads, not only what it
/// prints.
#[test]
fn string_commands_yield_their_value() {
    assert_eq!(tclrs::eval("string length abcd").unwrap().result, "4");
    assert_eq!(tclrs::eval("set x a\nappend x b").unwrap().result, "ab");
    assert_eq!(tclrs::eval("format %05.2f 1.5").unwrap().result, "01.50");
    assert_eq!(
        tclrs::eval("set x {}\nappend x a b\nset x").unwrap().result,
        "ab"
    );
}
