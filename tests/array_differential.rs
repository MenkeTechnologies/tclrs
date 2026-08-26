//! Differential execution for associative data: array variables, `array`,
//! `dict`, and the list syntax they are written in.
//!
//! Same rule as `execution_differential`: no expected value is written by hand.
//! Every program is run by tclsh and by tclrs and the two outputs compared byte
//! for byte, so a misreading of Tcl's list quoting, its dict ordering, or its
//! element-lookup errors fails here rather than becoming a baked-in bug.
//!
//! Two orderings in this area are deliberately undefined by `array(n)` — the
//! order of `array names` and of `array get` — so no program below prints more
//! than one array element name directly. Multi-element arrays are checked
//! through order-independent operations (`array size`, `dict get` on the result
//! of `array get`) and, in `array_get_sorts_through_dict_operations`, through a
//! selection sort written in Tcl that turns the undefined order into a defined
//! one.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Programs whose output must match tclsh exactly.
fn programs() -> Vec<String> {
    let mut programs: Vec<String> = FIXED.iter().map(|s| s.to_string()).collect();
    programs.push(quoting_matrix("puts [dict create {} 1]"));
    programs.push(quoting_matrix("puts [dict get [dict create {} v] {}]"));
    programs.push(quoting_matrix(
        "puts [dict create outer [dict create {} 1]]",
    ));
    programs.push(quoting_matrix("puts [dict create a {}]"));
    programs
}

/// One program that exercises list-element quoting over every ASCII character,
/// in leading, interior and trailing position. `{}` in `template` is replaced by
/// the quoted-word form of the element under test.
///
/// The characters are written as `\xNN` escapes so the program text itself stays
/// plain, and so that the parser's escape handling is exercised alongside.
fn quoting_matrix(template: &str) -> String {
    let mut out = String::new();
    for code in 1u8..=126 {
        for shape in ["a\\x{:02x}b", "\\x{:02x}b", "a\\x{:02x}", "\\x{:02x}"] {
            let element = format!("\"{}\"", shape.replace("{:02x}", &format!("{code:02x}")));
            out.push_str(&template.replace("{}", &element));
            out.push('\n');
        }
    }
    out
}

const FIXED: &[&str] = &[
    // ── array elements ──
    "set a(1) x\nputs $a(1)",
    "set a(x) 1\nset a(y) 2\nputs [expr {$a(x)+$a(y)}]",
    "set i 3\nset a($i) hit\nputs $a(3)",
    "set i 3\nset a(3) hit\nputs $a($i)",
    "set i k\nset a(pre$i.post) v\nputs $a(prek.post)",
    "set a(v) 4\nputs [expr {$a(v)*$a(v)}]",
    "puts [set a(only) written]",
    "set a(k) {x y}\nputs [array get a]",
    "set a() empty\nputs [array names a]\nputs $a()",
    // `$a(x(y))` is a parse error in tclsh — only the parsed form constrains
    // the index text — so the element is read back through `array get`.
    "set a(x(y)) 1\nputs [array names a]\nputs [array get a]",
    "set a(1) one\nset a(1) two\nputs $a(1)\nputs [array size a]",
    // `q(x)y` does not end in `)`, so it names a scalar, not an element.
    "set q(x)y scalar\nputs [set q(x)y]\nputs [array exists q]",
    // ── incr on elements ──
    "set a(n) 5\nputs [incr a(n)]\nputs [incr a(n) 3]\nputs [incr a(n) -10]",
    "puts [incr a(new)]\nputs [array names a]",
    "set a(n) { 5 }\nputs [incr a(n)]",
    "set a(n) 1\nputs [incr a(n) 0x10]",
    "set i 0\nwhile {$i < 5} {set sq($i) [expr {$i*$i}]; incr i}\nputs [array size sq]\nputs $sq(4)",
    // ── unset ──
    "set a(k) v\nunset a(k)\nputs [array size a]\nputs [array exists a]",
    "unset -nocomplain nosuchthing\nputs survived",
    "set v 1\nunset v\nset v 2\nputs $v",
    "set p 1\nset q 2\nunset p q\nputs [array exists p]",
    "set a(k) v\nunset -nocomplain a(nope)\nputs [array size a]",
    "puts [unset -nocomplain nothing]x",
    // ── array subcommands ──
    "puts [array exists nope]",
    "set b 5\nputs [array exists b]",
    "array set a {}\nputs [array exists a]\nputs [array size a]",
    "array set a {p 1 q 2}\nputs [array size a]",
    "puts [array set a {p 1}]done",
    "array set a {solo 9}\nputs [array names a]\nputs [array get a]",
    "array set a {a 1}\narray set a {a 9 b 2}\nputs $a(a)\nputs [array size a]",
    "array set a {a 1 b 2}\nunset a\nputs [array exists a]",
    "array set a {ax 1 ay 2 b 3}\narray unset a a*\nputs [array size a]\nputs [array names a]",
    "array set a {a 1}\narray unset a\nputs [array exists a]",
    "set s scalar\narray unset s\nputs $s",
    "array set a {ax 1 ay 2 b 3}\nputs [array names a b*]\nputs [array get a b*]",
    "array set a {a* 1 ay 2}\nputs [array names a -exact a*]",
    // Three arguments: the last is the pattern, never a mode.
    "array set a {-exact 1 b 2}\nputs [array names a -exact]",
    "array set a {x 1}\nputs [array names a -glob x]",
    "array set a {ab 1 b 2}\nputs [array names a {[ab]b}]",
    // `-regexp` searches the name rather than anchoring to it, which is what
    // separates it from `-glob`: `b` finds `ab` here and `b*` would not.
    "array set a {ab 1 zz 2}\nputs [array names a -regexp b]",
    "array set a {ab 1 zz 2}\nputs [array names a -regexp {^a}]",
    "array set a {ab 1 zz 2}\nputs [lsort [array names a -regexp {}]]",
    // No `-nocase`: the fold `-glob` would do through `string match -nocase` is
    // not available here, so a capital pattern finds nothing.
    "array set a {Ab 1}\nputs [array names a -regexp {^a}]|",
    // A literal `-regexp` element is still only a mode when it stands in the
    // mode position, so this is `array names` with a one-element pattern.
    "array set a {-regexp 1 b 2}\nputs [array names a -exact -regexp]",
    // Inside a procedure the array is a frame slot rather than a global-table
    // entry, and the filter reaches it through the place either way.
    "proc p {} {array set a {ab 1 zz 2}\nreturn [array names a -regexp {^a}]}\nputs [p]",
    "array set a {ab 1 zc 2 zz 3}\nputs [array size a]\nputs [array names a a?]x",
    "array set a {x 1}\nputs [array si a]",
    "puts [array size never]\nputs [array names never]\nputs [array get never]",
    // ── dict values ──
    "puts [dict create]x",
    "puts [dict create a 1 b 2]",
    "puts [dict create a 1 a 2]",
    "puts [dict create b 1 a 2 b 3]",
    "puts [dict create {a b} {c d} e {}]",
    "puts [dict create 1 one 2 two]",
    "puts [dict get {a 1 b 2} b]",
    "puts [dict get {a  1   b  2}]",
    "puts [dict get {a {b {c 7}}} a b c]",
    "puts [dict exists {a 1} a]\nputs [dict exists {a 1} z]",
    "puts [dict exists {a {b 1}} a b]\nputs [dict exists {a 1 b} a]",
    "puts [dict exists {a 1} a b]",
    "puts [dict keys {b 2 a 1 c 3}]",
    "puts [dict keys {bx 2 ax 1 by 3} b*]",
    "puts [dict keys {ab 1 ac 2 b 3} a?]",
    "puts [dict keys {a*b 1 axb 2} {a\\*b}]",
    "puts [dict keys {a 1 b 2} zzz]x",
    "puts [dict values {b 2 a 1 c 3}]",
    "puts [dict values {b 2 a 1 c 3} 2]",
    "puts [dict size {a 1 b 2}]\nputs [dict size {}]",
    "puts [dict remove {a 1 b 2 c 3} b]",
    "puts [dict remove {a 1 b 2 c 3} a c]",
    "puts [dict remove {a 1}]\nputs [dict remove {a 1} zz]",
    "puts [dict merge {a 1 b 2} {b 9 c 3}]",
    "puts [dict merge]x",
    "puts [dict merge {a  1}]",
    "puts [dict merge {a  1} {}]",
    "puts [dict merge {} {a  1}]",
    "puts [dict merge {a  1} {a 1}]",
    "puts [dict si {a 1 b 2}]",
    // ── dict variables ──
    "dict set d a 1\nputs $d",
    "set d {a 1 b 2}\nputs [dict set d b 9]\nputs $d",
    "set d {a 1}\ndict set d z 26\nputs $d",
    "dict set d a b 1\nputs $d",
    "dict set d a b c d 1\nputs $d",
    "set d {a 1 b 2 c 3}\ndict set d b 9\nputs $d",
    // ── dict for ──
    "dict for {k v} {b 2 a 1} {puts \"$k=$v\"}",
    "dict for {k v} {} {puts never}\nputs done",
    "puts [dict for {k v} {a 1} {expr 1}]x",
    "dict for {k v} {a 1 b 2 c 3} {if {$k eq \"b\"} break; puts $k}",
    "dict for {k v} {a 1 b 2 c 3} {if {$k eq \"b\"} continue; puts $k}",
    "dict for {k v} {a 1} {}\nputs \"$k $v\"",
    "set t 0\ndict for {k v} {a 1 b 2 c 3} {incr t $v}\nputs $t",
    "dict for {k v} {a 1 b 2} {dict for {i j} {x 8} {puts \"$k$i$v$j\"}}",
    "set n 0\nwhile {$n < 2} {dict for {k v} {a 1 b 2} {puts \"$n$k$v\"}; incr n}",
    // ── list quoting through dict ──
    "puts [dict create k {a b}]",
    "puts [dict create k {}]",
    "puts [dict create \"a\nb\" 1]",
    "puts [dict create {a$b} 1]",
    "puts [dict create {a[b]} 1]",
    "puts [dict create {a\"b} 1]",
    "puts [dict create {a;b} 1]",
    "puts [dict create \"a\\\\b\" 1]",
    "puts [dict create \"a\\\\\" 1]",
    "puts [dict create \"a\\{b\" 1]",
    "puts [dict create {a{b}c} 1]",
    "puts [dict create # 1]\nputs [dict create a #]\nputs [dict create a 1 # 2]",
    "puts [dict keys [dict create # 1 b 2]]\nputs [dict keys [dict create b 2 # 1]]",
    "puts [dict get {a\\nb 1} \"a\nb\"]",
    "puts [dict get {{a\\nb} 1} {a\\nb}]",
    "puts [dict get {\"a b\" 1} {a b}]",
    "puts [dict get \"a\tb\nc\td\" c]",
    // ── dict incr ──
    // A missing key counts as zero rather than as an error, so the increment
    // becomes the value; the command yields the whole dict, as `dict set` does.
    "set d [dict create a 1 b 2]\ndict incr d a\nputs $d",
    "set d [dict create a 1 b 2]\ndict incr d a 5\nputs $d",
    "set d [dict create a 1]\ndict incr d fresh\nputs $d",
    "set d [dict create a 1]\ndict incr d fresh 7\nputs $d",
    "set d [dict create a 1]\nputs [dict incr d a]",
    "set d [dict create a 1]\ndict incr d a -3\nputs $d",
    "set d [dict create a 1]\ndict incr d a 0\nputs $d",
    // The variable itself may be absent: `dict incr` creates it.
    "dict incr fresh k\nputs $fresh",
    "dict incr fresh k 4\nputs [dict get $fresh k]",
    // Integers in any of Tcl's spellings, and promotion past an `i64` — Tcl's
    // integers are arbitrary precision and `dict incr` is no exception.
    "set d [dict create a 1]\ndict incr d a 0x10\nputs $d",
    "set d [dict create a 1]\ndict incr d a 1_0\nputs $d",
    "set d [dict create a 9223372036854775807]\ndict incr d a\nputs $d",
    "set d [dict create a -9223372036854775808]\ndict incr d a -1\nputs $d",
    "set d [dict create a 99999999999999999999]\ndict incr d a 1\nputs $d",
    // Refusals: the increment, the stored value, and the argument count. The
    // wording is `incr`'s own, not an `expr` operand error.
    "set d [dict create a 1]\nputs [catch {dict incr d a x} m]\nputs $m",
    "set d [dict create a notanint]\nputs [catch {dict incr d a} m]\nputs $m",
    "set d [dict create a 1.5]\nputs [catch {dict incr d a} m]\nputs $m",
    "set d [dict create a 1]\nputs [catch {dict incr d a 1.5} m]\nputs $m",
    // A key that needs quoting, and one that arrives from a variable.
    "set d {}\ndict incr d {a b}\nputs $d",
    "set d {}\nset k {x y}\ndict incr d $k 2\nputs $d",
    // Inside a procedure the dict is a frame slot, which the place operand
    // reaches as readily as a global.
    "proc p {} {dict incr d k 3\nreturn $d}\nputs [p]\nputs [p]",
    "set d [dict create a 1]\nproc p {} {global d\ndict incr d a\nreturn $d}\nputs [p]\nputs $d",
    // In a loop, which is where a dict counter is actually used.
    "set d {}\nforeach w {a b a c a} {dict incr d $w}\nputs [dict get $d a]\nputs [dict size $d]",
    // ── dict replace / getdef ──
    "puts [dict replace {a 1} a 2 b 3]",
    "puts [dict replace {a 1 b 2}]",
    "puts [dict replace {} {x y} {1 2}]",
    "puts [catch {dict replace {a 1} b} m]\nputs $m",
    "puts [dict getdef {a 1} z 9]",
    "puts [dict getwithdefault {a 1} a 9]",
    "puts [dict getwithdefault {a {b 1}} a b 9]",
    "puts [dict getwithdefault {a {b 1}} a z 9]",
    "puts [dict getwithdefault {a 1} z y 9]",
    "puts [catch {dict getwithdefault {a 1}} m]\nputs $m",
    // ── dict unset ──
    "set d {a 1 b 2}\ndict unset d a\nputs $d",
    "set d {a 1}\ndict unset d z\nputs $d",
    "set d {a {b 1 c 2}}\ndict unset d a b\nputs $d",
    "set d {a {b 1}}\nputs [catch {dict unset d z q} m]\nputs $m",
    "set d {a 1 b 2}\nputs [dict unset d a]",
    "dict unset fresh k\nputs [list $fresh]",
    "set d {a 1}\nputs [catch {dict unset d} m]\nputs $m",
    "proc p {} {set d {a 1 b 2}\ndict unset d a\nreturn $d}\nputs [p]",
    // ── dict lappend / append ──
    "set d {a 1}\ndict lappend d a 2 3\nputs $d",
    "set d {}\ndict lappend d k v1 v2\nputs $d",
    "set d {a {}}\ndict lappend d a {x y}\nputs $d",
    "set d {a 1}\ndict lappend d a\nputs $d",
    "set d {a 1}\ndict append d a xy\nputs $d",
    "set d {}\ndict append d k a b c\nputs $d",
    "set d {a 1}\nputs [dict append d a z]",
    "proc p {} {dict lappend d k 1\ndict lappend d k 2\nreturn $d}\nputs [p]",
    // ── dict filter ──
    "puts [dict filter {a 1 b 2} key a]",
    "puts [dict filter {aa 1 ab 2 b 3} key a*]",
    "puts [dict filter {a 1 b 2} value 1]",
    "puts [dict filter {a 1 b 2} value *]",
    "puts [dict filter {a 1 b 2} key]",
    "puts [dict filter {aa 1 bb 2 cc 3} key a* c*]",
    "puts [catch {dict filter {a 1} bogus} m]\nputs $m",
    // ── dict filter … script ──
    "puts [dict filter {a 1 b 2 c 3} script {k v} {expr {$v > 1}}]",
    "puts [dict filter {a 1 b 2 c 3} script {k v} {if {$k eq \"c\"} break; expr 1}]",
    "puts [dict filter {a 1 b 2 c 3} script {k v} {if {$k eq \"b\"} continue; expr 1}]",
    "puts [dict filter {} script {k v} {expr 1}]x",
    "puts [catch {dict filter {a 1} script {k v} {set k}} m]\nputs $m",
    "puts [catch {dict filter {a 1} script {k} {expr 1}} m]\nputs $m",
    "puts [dict filter {a 1} script {k v} {set k Z; expr 1}]",
    "proc p {} {set d {a 1 b 2}\nreturn [dict filter $d script {k v} {expr {$v==2}}]}\nputs [p]",
    // ── dict map ──
    "puts [dict map {k v} {a 1 b 2 c 3} {expr {$v*2}}]",
    "puts [dict map {k v} {a 1 b 2 c 3} {if {$k eq \"b\"} break; expr {$v*2}}]",
    "puts [dict map {k v} {a 1 b 2 c 3} {if {$k eq \"b\"} continue; expr {$v*2}}]",
    "puts [dict map {k v} {} {set k}]x",
    "puts [dict map {k v} {a 1} {set k Z; set v}]",
    "puts [catch {dict map {k v} {a 1 b} {set k}} m]\nputs $m",
    "puts [catch {dict map {k} {a 1} {set k}} m]\nputs $m",
    "puts [catch {dict map {k v} {a 1} {error boom}} m]\nputs $m",
    "puts [dict map {k v} {a {p 1}} {dict map {i j} $v {expr {$j*3}}}]",
    "proc p {} {set d {x 1 y 2}\nreturn [dict map {k v} $d {expr {$v+10}}]}\nputs [p]",
    // A walk whose body re-enters the same walk. The state is per-invocation,
    // so the outer one still visits every pair; a shared cursor made it stop
    // after the first.
    "proc w {d n} {set out {}\ndict for {k v} $d {lappend out $k$n\nif {$n < 2} {set out [concat $out [w $d [expr {$n+1}]]]}}\nreturn $out}\nputs [w {a 1 b 2} 0]",
    "proc m {d n} {return [dict map {k v} $d {if {$n < 2} {m $d [expr {$n+1}]} else {set v}}]}\nputs [m {a 1 b 2} 0]",
    // ── dict update ──
    //
    // The write-back is a `finally`, so every ending the body can have is a
    // separate case: the interesting ones are the endings that are *not* an
    // ordinary result, because those are the ones an ending-shaped
    // implementation would get wrong while still passing the first line here.
    "set d {a 1 b 2}\ndict update d a x {set x 99}\nputs $d",
    "set d {a 1 b 2}\nputs [dict update d a x {expr {$x + 10}}]",
    "set d {a 1 b 2}\ndict update d a x b y {set x $y; set y 7}\nputs $d",
    // A key the dictionary does not have leaves its variable *unset*, not empty.
    "set d {a 1}\nputs [dict update d zz y {info exists y}]\nputs $d",
    "set d {a 1}\ndict update d zz y {set y new}\nputs $d",
    // Unsetting the variable removes the key.
    "set d {a 1 b 2}\ndict update d a x {unset x}\nputs $d",
    // An error still writes back, and the error still leaves the command.
    "set d {a 1}\nputs [catch {dict update d a x {set x 5; error boom}} m]\nputs $m\nputs $d",
    // So does a `break` bound for the enclosing loop.
    "set d {a 1}\nforeach i {1 2 3} {dict update d a x {set x $i; break}}\nputs $d",
    "set d {a 1}\nset seen {}\nforeach i {1 2 3} {dict update d a x {set x $i; continue}\nlappend seen $i}\nputs $d\nputs [list $seen]",
    // And a `return`, which spends a level on the way out.
    "proc p {} {set d {a 1}\ndict update d a x {set x 42; return $d}}\nputs [p]",
    "proc p {} {upvar 1 d d\ndict update d a x {set x 42; return done}}\nset d {a 1}\nputs [p]\nputs $d",
    // The dictionary variable going away drops the write-back silently.
    "set d {a 1}\nputs [dict update d a x {unset d; set x 5}]\nputs [info exists d]",
    // The variable becoming something that is not a dictionary fails *here*,
    // after the body, replacing what the body left.
    "set d {a 1}\nputs [catch {dict update d a x {set d \"q w e\"; set x 5}} m]\nputs $m\nputs $d",
    // Refusals, all of them from the command rather than from the body.
    "puts [catch {dict update nosuch k v {set v 1}} m]\nputs $m",
    "set d \"a b c\"\nputs [catch {dict update d k v {set v 1}} m]\nputs $m",
    "array set A {a 1}\nputs [catch {dict update A k v {set v 1}} m]\nputs $m",
    "set d {a 1}\narray set x {q 1}\nputs [catch {dict update d a x {set x 5}} m]\nputs $m",
    "puts [catch {dict update d k} m]\nputs $m",
    "puts [catch {dict update d k v k2} m]\nputs $m",
    // The keys are read once, before the body, and the body cannot change them.
    "set d {a 1 b 2}\nset n 0\ndict update d [incr n; format a] x {set x 9}\nputs $n\nputs $d",
    "set d {a 1 b 2}\nset k a\ndict update d $k x {set k b; set x 9}\nputs $d",
    // Nested and recursive: an inner write-back must not be handed the outer
    // one's record, which a single hidden one would have been.
    "set d {a 1 b {c 2}}\ndict update d b inner {dict update inner c z {set z 9}}\nputs $d",
    "proc walk {n} {set d [list k $n]\ndict update d k v {if {$n > 0} {walk [expr {$n-1}]}\nset v [expr {$v*2}]}\nputs $d}\nwalk 3",
    // Inside a procedure the variables are frame slots, not globals.
    "proc p {} {set d {a 1 b 2}\ndict update d a x {set x [expr {$x+1}]}\nreturn $d}\nputs [p]\nputs [info exists x]",
    // ── dict with ──
    //
    // Same `finally` write-back as `dict update`, so every ending is a case
    // again; what is new is that the variables are named by the dictionary's own
    // *keys*, which are values.
    "set d {a 1 b 2}\ndict with d {set a 99}\nputs $d",
    "set d {a 1 b 2}\nputs [dict with d {expr {$a + $b}}]",
    "set d {}\ndict with d {}\nputs [list $d]",
    // Every key goes back, not only the ones the body assigned.
    "set d {a 1 b 2}\ndict with d {set a 9}\nputs $d",
    // Unsetting a bound variable is the one way the body can remove a key.
    "set d {a 1 b 2}\ndict with d {unset a}\nputs $d",
    // The keys the write-back puts back are the ones the *binding* recorded, so
    // a body that empties or edits the dictionary itself does not lose them.
    "proc p {} {set d {a 1 q 2}\ndict with d {dict unset d q}\nreturn $d}\nputs [p]",
    "proc p {} {set d {a 1 q 2}\ndict with d {set d {}}\nreturn $d}\nputs [p]",
    "proc p {} {set d {a 1 q 2}\ndict with d {dict set d zz 9}\nreturn $d}\nputs [p]",
    // Every ending reaches the write-back.
    "set d {a 1}\nputs [catch {dict with d {set a 5; error boom}} m]\nputs $m\nputs $d",
    "set d {a 1}\nforeach i {1 2 3} {dict with d {set a $i; break}}\nputs $d",
    "set d {a 1}\nset seen {}\nforeach i {1 2 3} {dict with d {set a $i; continue}\nlappend seen $i}\nputs $d\nputs [list $seen]",
    // A `return` reads its word *before* the write-back, so it answers the
    // dictionary as the body found it.
    "proc p {} {set d {a 1}\ndict with d {set a 42; return $d}}\nputs [p]",
    "proc p {} {upvar 1 d d\ndict with d {set a 42; return done}}\nset d {a 1}\nputs [p]\nputs $d",
    // The dictionary variable going away drops the write-back silently.
    "proc p {} {set d {a 1}\ndict with d {unset d; set a 9}\nreturn [info exists d]}\nputs [p]",
    // The variable becoming something that is not a dictionary fails here,
    // after the body, replacing what the body left.
    "set d {a 1}\nputs [catch {dict with d {set d \"q w e\"; set a 5}} m]\nputs $m\nputs $d",
    // A key that collides with a local overwrites it, and the local keeps the
    // value afterwards — `dict with` does not restore what it displaced.
    "proc p {} {set a 111\nset d {a 1}\ndict with d {set a 7}\nreturn [list $d $a]}\nputs [p]",
    // A key named as the dictionary variable clobbers the dictionary, which the
    // write-back then finds is not one.
    "proc p {} {set d {d 1 b 2}\nreturn [catch {dict with d {set b 3}} m]\n}\nputs [p]",
    // Keys are variable *names*, so an `a(i)` key is one element of an array,
    // and one whose base already holds a scalar is refused as `set` refuses it.
    "set d {q(1) 5}\ndict with d {set q(1) 6}\nputs $d\nputs [array size q]",
    "set a hello\nset d {a(1) 5}\nputs [catch {dict with d {}} m]\nputs $m",
    "proc p {} {set a hello\nset d {a(1) 5}\nreturn [catch {dict with d {}} m],$m}\nputs [p]",
    // A path names a sub-dictionary to open out instead.
    "set d {k1 {k2 {a 1 b 2}}}\ndict with d k1 k2 {set a 99}\nputs $d",
    "set d {k {a 1}}\nputs [catch {dict with d zz {set a 1}} m]\nputs $m",
    // A path that stops leading anywhere while the body runs drops the
    // write-back, exactly as a missing variable does.
    "proc p {} {set d {k {a 1}}\ndict with d k {set d {other 1}; set a 99}\nreturn $d}\nputs [p]",
    "proc p {} {set d {k {a 1}}\ndict with d k {dict unset d k; set a 99}\nreturn [list $d]}\nputs [p]",
    // Refusals, all from the command rather than from the body.
    "puts [catch {dict with nosuch {}} m]\nputs $m",
    "set d \"a b c\"\nputs [catch {dict with d {}} m]\nputs $m",
    "array set A {a 1}\nputs [catch {dict with A {}} m]\nputs $m",
    "set d {x 1}\narray set x {q 1}\nputs [catch {dict with d {}} m]\nputs $m",
    "puts [catch {dict with} m]\nputs $m",
    "puts [catch {dict with d} m]\nputs $m",
    // Nested, over the same dictionary and over a sub-dictionary: an inner
    // write-back must not be handed the outer one's keys.
    "set d {a 1 b {c 2}}\ndict with d {dict with b {set c 42}\nset a 5}\nputs $d",
    "proc walk {n} {set d [list k $n]\ndict with d {if {$n > 0} {walk [expr {$n-1}]}\nset k [expr {$k*2}]}\nputs $d}\nwalk 3",
    // Inside a procedure the bound variables are frame slots and go away with
    // the frame; at the script's own level they are globals and stay.
    "proc p {} {set d {a 1}\ndict with d {set a [expr {$a+1}]}\nreturn $d}\nputs [p]\nputs [info exists a]",
    "set d {zz 1}\ndict with d {set zz 2}\nputs $d\nputs $zz",
    // A key the body never spells has no frame slot the compiler could have
    // assigned, so it gets one at run time and is a local of that activation
    // like any other. These are the shapes that catch that going wrong: writing
    // the key through the dictionary, a nested binding that names it, and — the
    // two that used to diverge — a nested script that assigns it, and a *second*
    // binding over the same unmentioned key, which must find the local the first
    // one made rather than a second one of its own.
    "proc p {} {set d {a 1 q 2}\ndict with d {dict set d q 7}\nreturn [list $d]}\nputs [p]",
    "proc p {} {set d {q 1}\ndict with d {dict set d q 5\ndict with d {set _ $q}}\nreturn $d}\nputs [p]",
    "set d {q 1}\ndict with d {dict set d q 5\ndict with d {}}\nputs $d",
    "set e {r 1}\ndict with e {eval {set r 9}}\nputs $e",
    "proc p {} {set d {kk 1}\ndict with d {eval {set kk 5}}\nreturn $d}\nputs [p]",
    "proc p {} {set a {zz 1}\nset b {zz 2}\ndict with a {}\nset x [eval {set zz}]\ndict with b {}\nreturn \"$x [eval {set zz}] $a $b\"}\nputs [p]",
    // The variable the binding made is one of the activation's locals, and dies
    // with it.
    "proc p {} {set d {xx 1}\ndict with d {}\nreturn [lsort [info locals]]}\nputs [p]",
    "proc p {} {set d {ww 1}\ndict with d {}}\np\nputs [info exists ww]",
    // ── the two together ──
    "array set a {x 1 y 2}\nputs [dict get [array get a] y]\nputs [dict size [array get a]]",
    "array set a {x 1 y 2 z 3}\nset d [array get a]\nputs [dict exists $d z]\nputs [dict exists $d w]",
    "dict for {k v} {alpha 1 beta 2} {set counts($k) $v}\nputs [array size counts]\nputs $counts(beta)",
    "set d {}\nset i 0\nwhile {$i < 4} {dict set d k$i $i; incr i}\nputs $d\nputs [dict size $d]",
    // ── naming an element of a variable that is not an array ──
    //
    // Which verb the refusal uses is not uniform in tclsh, and the difference is
    // where each command's lookup happens rather than anything about the
    // variable: `incr` reads before it writes and answers `can't read`, while
    // `append` and `lappend` read tolerantly and are refused by the store, which
    // answers `can't set`. A plain read answers `can't read`.
    "set b 1\nputs [catch {set b(1)} m]:$m",
    "set b 1\nputs [catch {incr b(1)} m]:$m",
    "set b 1\nputs [catch {append b(1) x} m]:$m",
    "set b 1\nputs [catch {lappend b(1) x} m]:$m",
    "set b 1\nputs [catch {set b(1) v} m]:$m",
    "set b 1\nputs [catch {unset b(1)} m]:$m",
    // The same three commands on a name that is not a variable at all, and on an
    // array missing that one element, answer differently again.
    "puts [catch {set nope(1)} m]:$m",
    "set a(9) x\nputs [catch {set a(1)} m]:$m",
    "set a(9) x\nputs [catch {unset a(1)} m]:$m",
    "set a(9) x\nincr a(1) 3\nappend a(2) q\nlappend a(3) e\nputs [lsort [array names a]]",
];

/// The selection sort that turns `array get`'s undefined order into a defined
/// one, so a multi-element array can be compared against tclsh at all.
const SORTED_ARRAY_WALK: &str = "\
array set a {delta 4 alpha 1 charlie 3 bravo 2}
set pairs [array get a]
set n [dict size $pairs]
set prev {}
set i 0
while {$i < $n} {
    set best {}
    dict for {k v} $pairs {
        if {($i == 0 || $k gt $prev) && ($best eq {} || $k lt $best)} {
            set best $k
        }
    }
    puts \"$best=[dict get $pairs $best] $a($best)\"
    set prev $best
    incr i
}
";

fn tclsh() -> Option<PathBuf> {
    for name in ["tclsh9.0", "tclsh", "tclsh8.6"] {
        let Ok(out) = Command::new("sh")
            .arg("-c")
            .arg(format!("command -v {name}"))
            .output()
        else {
            continue;
        };
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if path.is_empty() {
            continue;
        }
        // Only the exact release this port is written against is an oracle.
        // tclrs targets 9.0.4 (`src/cmd_info.rs`'s `TCL_PATCHLEVEL`), and a
        // reference from any other release reports ITS version's differences
        // as tclrs failures: 8.6 words errors differently ("couldn't compile
        // regular expression" for "cannot compile") and has a different
        // ensemble membership, while 9.0.3 predates the lseq fixes (a zero
        // step yields the empty list where the manual says it yields `count`
        // elements, and a bareword argument is still an expr). The ubuntu CI
        // image ships 8.6, so CI skips these and they run against a matching
        // tclsh locally.
        let Ok(v) = Command::new("sh")
            .arg("-c")
            .arg(format!("printf 'puts [info patchlevel]\\n' | {path}"))
            .output()
        else {
            continue;
        };
        if String::from_utf8_lossy(&v.stdout).trim() == "9.0.4" {
            return Some(PathBuf::from(path));
        }
    }
    None
}

fn reference(tclsh: &PathBuf, program: &str) -> Result<String, String> {
    // The test functions run in parallel, so the scratch file has to be unique
    // per call and not merely per process.
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "tclrs-assoc-{}-{}.tcl",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path, program).expect("write program");
    let out = Command::new(tclsh).arg(&path).output().expect("run tclsh");
    let _ = std::fs::remove_file(&path);
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

fn compare(tclsh: &PathBuf, programs: &[String]) {
    let mut failures = Vec::new();
    for program in programs {
        let expected = match reference(tclsh, program) {
            Ok(out) => out,
            Err(e) => {
                failures.push(format!("tclsh rejected program:\n{program}\n{e}"));
                continue;
            }
        };
        match tclrs::eval(program) {
            Ok(outcome) if outcome.output == expected => {}
            Ok(outcome) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs: {:?}",
                outcome.output
            )),
            Err(e) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs failed: {e}"
            )),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} programs diverge:\n\n{}",
        failures.len(),
        programs.len(),
        failures.join("\n\n")
    );
}

#[test]
fn associative_execution_matches_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    compare(&tclsh, &programs());
}

/// A multi-element array printed in an order both implementations agree on.
#[test]
fn array_get_sorts_through_dict_operations() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };
    compare(&tclsh, &[SORTED_ARRAY_WALK.to_string()]);
}

/// Failures have to match too: the message tclsh produces is the specification
/// for the message tclrs produces.
#[test]
fn associative_errors_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh 9.0.4 on PATH");
        return;
    };

    let programs = [
        // Reading and writing elements.
        "puts $a(1)",
        "set b 1\nputs $b(1)",
        "array set c {}\nputs $c(1)",
        "set d(1) x\nunset d(1)\nputs $d(1)",
        "set e 3\nset e(1) x",
        "set f(1) x\nset g $f",
        "set h(1) x\nset h 3",
        "set i(x) q\nincr i(x)",
        "set j(x) 1\nincr j(x) 2.5",
        // unset.
        "unset nosuchvar",
        "unset -- nosuchvar",
        "array set k {}\nunset k(1)",
        "set l 1\nunset l(1)",
        // The array command.
        "set m 1\narray set m {a 1}",
        "set n 1\narray set n {}",
        "array set o {a 1 b}",
        "array size p q",
        "array bogus q",
        "array s q",
        "array set q {a 1}\narray names q -bogus a",
        // A `-regexp` pattern the engine will not compile is the command's own
        // error, with the same wording `regexp` gives for the same pattern.
        "array set q {a 1}\narray names q -regexp {a[}",
        "array exists q x",
        "array set q",
        // The dict command.
        "puts [dict get {a 1} z]",
        "puts [dict get {a 1 b} a]",
        "puts [dict get x a]",
        "puts [dict size {a 1 b}]",
        "puts [dict create a]",
        "puts [dict bogus]",
        "puts [dict s {a 1}]",
        "dict for {k} {a 1} {}",
        "set r(1) x\ndict set r k v",
    ];

    let mut failures = Vec::new();
    for program in programs {
        let Err(expected) = reference(&tclsh, program) else {
            failures.push(format!(
                "tclsh accepted a program meant to fail:\n{program}"
            ));
            continue;
        };
        // tclsh writes the message followed by a stack trace; only the first
        // line is the message itself.
        let expected = expected.lines().next().unwrap_or_default().to_string();
        match tclrs::eval(program) {
            Err(actual) if actual.starts_with(&expected) => {}
            Err(actual) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs: {actual:?}"
            )),
            Ok(outcome) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs succeeded: {outcome:?}"
            )),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} error programs diverge:\n\n{}",
        failures.len(),
        programs.len(),
        failures.join("\n\n")
    );
}

/// The undefined orderings are at least stable here, which keeps tclrs's own
/// output reproducible from run to run even where tclsh's is not.
#[test]
fn array_names_and_get_are_sorted() {
    let names = tclrs::eval("array set a {delta 4 alpha 1 charlie 3}\nputs [array names a]")
        .expect("runs")
        .output;
    assert_eq!(names, "alpha charlie delta\n");
    let pairs = tclrs::eval("array set a {delta 4 alpha 1 charlie 3}\nputs [array get a]")
        .expect("runs")
        .output;
    assert_eq!(pairs, "alpha 1 charlie 3 delta 4\n");
}

/// Subcommands that exist in tclsh but not here must say so rather than do
/// something else.
#[test]
fn unimplemented_subcommands_are_refused() {
    for (src, expected) in [
        (
            "array startsearch a",
            "array startsearch is not supported yet",
        ),
        ("array for {k v} a {}", "array for is not supported yet"),
        // `array names -regexp` landed; what it answers is compared against
        // tclsh in `FIXED`, and a pattern that will not compile is in the
        // error corpus.
        // `dict incr` is implemented; what it still refuses is an array element
        // as the target, which is `dict set`'s limitation and now also its own.
        (
            "set a(1) x\ndict incr a(1) k",
            "array element is not supported yet",
        ),
        // `dict filter … script` and `dict map` were refused here until they
        // landed; what they answer is now compared against tclsh in `FIXED`.
        ("dict info {a 1}", "dict info is not supported yet"),
        // `dict with` landed; what it answers is compared against tclsh in
        // `FIXED`. What it still refuses is the same two names `dict update`
        // cannot resolve while compiling.
        (
            "set a(1) x\ndict with a(1) {}",
            "array element is not supported yet",
        ),
        (
            "set d {a 1}\nset b {set a 9}\ndict with d $b",
            "script body must be a literal in this phase",
        ),
        // `dict update` landed; what it answers is compared against tclsh in
        // `FIXED`. What it still refuses is the two names it cannot resolve
        // while compiling — an array element as the dictionary, and a computed
        // variable name, which is the wall `set $name 1` meets.
        (
            "set a(1) x\ndict update a(1) k v {}",
            "array element is not supported yet",
        ),
        (
            "set d {a 1}\nset n v\ndict update d a $n {}",
            "variable name must be a literal in this phase",
        ),
        (
            "set a(1) x\ndict set a(1) k v",
            "array element is not supported yet",
        ),
    ] {
        let err = tclrs::eval(src).expect_err(&format!("{src:?} should fail"));
        assert!(
            err.contains(expected),
            "{src:?}: expected an error mentioning {expected:?}, got {err:?}"
        );
    }
}
