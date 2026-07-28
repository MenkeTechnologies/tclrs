# gen.tcl — seeded generator for the tclrs differential fuzz corpus.
#
#   tclsh scripts/fuzz/gen.tcl SEED N DEPTH
#
# Writes N Tcl programs to stdout, each introduced by a `#=== INDEX` line.
# `scripts/fuzz_parity.sh` splits them into case files and runs every one under
# both `tclsh` (ground truth) and `tclrs` (subject); any case whose stdout,
# exit status or error message differs is a parity gap.
#
# Two properties are load-bearing:
#
# * **Deterministic.** The PRNG is a 32-bit xorshift built from `<<`, `>>`, `^`
#   and `&` only, so it never leaves i64 range and the same SEED produces the
#   byte-identical corpus on any machine. A divergence is reproducible from the
#   seed and the case index alone.
# * **Terminating.** Loop bounds are structural, never generated: a `while`
#   always counts a fresh counter up to a small literal and its `incr` is the
#   *first* statement of the body, so a generated `continue` cannot skip it; a
#   `for` counts the same way; a `foreach` is bounded by a literal list. No
#   unbounded `while` is ever emitted. `string repeat` counts and index
#   arithmetic come from the small pool for the same reason.
#
# One statement per line is the corpus contract — bodies are inlined inside
# braces rather than spread over lines — because the shrinker
# (`scripts/fuzz/shrink.pl`) reduces a case by deleting lines, and a line that
# is a whole statement keeps every candidate brace-balanced.
#
# The generator itself is written in the subset of Tcl that *both* engines
# implement (no `env`, no file I/O, no `info`, no math functions, no arrays in a
# procedure), so `scripts/fuzz_parity.sh --self-check` can run it under tclrs
# too and compare the corpora — a generator that only ran under tclsh could not
# be checked that way.

# ── PRNG ────────────────────────────────────────────────────────────────────

proc rnext {} {
    global S
    set s $S
    set s [expr {($s ^ ($s << 13)) & 0xFFFFFFFF}]
    set s [expr {$s ^ ($s >> 17)}]
    set s [expr {($s ^ ($s << 5)) & 0xFFFFFFFF}]
    set S $s
    return $s
}

# Uniform-ish integer in [0, n).
proc rint {n} {
    if {$n <= 1} {
        return 0
    }
    return [expr {[rnext] % $n}]
}

proc rpick {lst} {
    return [lindex $lst [rint [llength $lst]]]
}

# True `pct` percent of the time.
proc rchance {pct} {
    return [expr {[rint 100] < $pct}]
}

# ── literal pools ───────────────────────────────────────────────────────────
#
# Deliberately awkward: every one of these has broken a Tcl reimplementation
# somewhere. Braces and brackets test rules 5/6 and list quoting, `$` tests
# substitution, a leading `#` tests the comment rule and `lsort`'s first-element
# quoting, leading zeros and `1_0` test the integer grammar, the i64 bounds test
# overflow reporting, and the non-ASCII text tests character-versus-byte
# indexing.

set POOL_PLAIN [list \
    a b abc xyz hello A-B c1 "" 0 1 2 5 42 -1 -7 255 1000]

set POOL_AWKWARD [list \
    "" " " "  padded  " "hello world" "a b  c" "a b {c d} e" \
    "a\{b" "\{a\}" "a\}b" "\{" "\[a\]" "x\]y" "\[" \
    "he said \"hi\"" "q\"r" "back\\slash" "a\\b" \
    "\$x" "cost\$" "\$" "#comment" "#" "a#b" \
    007 010 0x1f 0o17 0b101 0d9 1_0 -0 +5 \
    9223372036854775807 -9223372036854775808 9223372036854775808 \
    1e300 1.0e-7 0.1 1.5 -0.0 3.0 2.5e-3 \
    "héllo" "日本語" "αβγ" "ÜñîçøðÉ" "naïve café" \
    "line\nbreak" "tab\there" \
    "a b c" "1 2 3" "\{a b\} c" "end" "end-1" "*" "a*b" "?" "\[ab\]"]

# Integers that stay inside i64 under the arithmetic the generator emits.
set POOL_INT [list 0 1 2 3 4 5 7 8 10 16 42 -1 -2 -7 100 255 1000 65535 -65536 \
    123456789 4611686018427387903 -4611686018427387904]

# Integers at or over the i64 boundary: tclsh promotes to a bignum, tclrs
# reports `integer value too large to represent` (BUGS.md, "Arbitrary-precision
# integers"). Drawn rarely so the corpus is not mostly that one skip.
set POOL_BIG [list 9223372036854775807 -9223372036854775808 \
    9223372036854775808 99999999999999999999]

set POOL_FLOAT [list 0.0 -0.0 1.0 0.5 -1.5 3.14 0.1 1e300 1.0e-7 2.5e-3 1e10]

# Small counts: everything that could allocate or iterate is drawn from here.
set POOL_SMALL [list 0 1 2 3 4]

set POOL_INDEX [list 0 1 2 3 -1 end end-1 end+1 5 0x2 1_0]

set POOL_GLOB [list * a* *b "a?c" "\[ab\]*" "" x "a*b*c"]

# Only the classes tclrs implements: the rest need Unicode category tables and
# are refused (`cmd_string.rs:372`), which would classify the whole case as a
# skip rather than compare it. Non-ASCII text still reaches these classes from
# the value pool, and that refusal is a skip the report counts.
set POOL_STRCLASS [list alnum alpha boolean control digit double false \
    integer lower true upper wordchar]

set POOL_FMT [list %s %d %i %u %o %x %X %b %c %e %E %f %g %G \
    %5d %-8s %+d %08.3f %.3s %5.2f %#x %#o %lld %hd]

# ── emitting a literal ──────────────────────────────────────────────────────

# The escape map for the quoted form. Built with `list` rather than written as a
# braced literal so what each pair means is unambiguous.
set QMAP [list \\ \\\\ \" \\\" \$ \\\$ \[ \\\[ \] \\\] \{ \\\{ \} \\\} \
    \n \\n \t \\t]

# `s` as one Tcl word that both engines read back as exactly `s`.
#
# Three forms, cheapest first: bare when nothing in the string is special,
# brace-quoted when the string has no brace and no backslash — braces suppress
# every other substitution — and double-quoted with rule-9 escapes otherwise.
proc Q {s} {
    global QMAP
    if {[string length $s] == 0} {
        return "\{\}"
    }
    set special 0
    foreach ch [list " " "\t" "\n" "\{" "\}" "\[" "\]" "\$" "\"" ";" "\\"] {
        if {[string first $ch $s] >= 0} {
            set special 1
        }
    }
    if {!$special && [string index $s 0] ne "#"} {
        return $s
    }
    set hard 0
    foreach ch [list "\{" "\}" "\\" "\n" "\t"] {
        if {[string first $ch $s] >= 0} {
            set hard 1
        }
    }
    if {!$hard} {
        return "\{$s\}"
    }
    return "\"[string map $QMAP $s]\""
}

# A quoted literal drawn from the awkward pool, or a plain one.
proc value {} {
    global POOL_PLAIN POOL_AWKWARD
    if {[rchance 55]} {
        return [Q [rpick $POOL_AWKWARD]]
    }
    return [Q [rpick $POOL_PLAIN]]
}

# ── per-case state ──────────────────────────────────────────────────────────
#
# What the case has defined so far, so a statement only reads what is certain to
# exist. Two rules keep "certain" honest, and both matter because reading a
# variable that was never set is itself a divergence (tclrs answers `""` where
# tclsh raises `can't read "x": no such variable`) that the allowlist suppresses:
#
# * a variable is registered only when its `set` is at the case's own top level
#   (`NESTED` is 0) — one written inside an `if` body might never run;
# * inside a procedure body (`INPROC`) only that procedure's own locals are
#   readable, since a top-level name is not in scope there without `global`.
#
# A read of a never-set variable is then generated deliberately, at
# `UNSET_RATE` percent, so the allowlist entry keeps getting hits and stays
# visible in the report rather than quietly covering nothing.

set UNSET_RATE 4

proc reset_case {} {
    global LINES VARS NVARS LVARS ARRS DICTS PROCS NEXT NESTED INPROC LOCALS
    global COUNTERS
    set LINES [list]
    set COUNTERS [list]
    set VARS [list]
    set NVARS [list]
    set LVARS [list]
    set ARRS [list]
    set DICTS [list]
    set PROCS [list]
    set NEXT 0
    set NESTED 0
    set INPROC 0
    set LOCALS [list]
}

proc emit {line} {
    global LINES
    lappend LINES $line
}

# A fresh name, unique within the case.
proc fresh {prefix} {
    global NEXT
    incr NEXT
    return "$prefix$NEXT"
}

proc note {kind name} {
    global VARS NVARS LVARS ARRS DICTS NESTED INPROC
    # Only a top-level assignment outside every procedure makes a name certain.
    if {$NESTED > 0 || $INPROC} {
        return
    }
    switch -exact -- $kind {
        var {
            if {[lsearch -exact $VARS $name] < 0} {
                lappend VARS $name
            }
        }
        num {
            if {[lsearch -exact $NVARS $name] < 0} {
                lappend NVARS $name
            }
            note var $name
        }
        list {
            if {[lsearch -exact $LVARS $name] < 0} {
                lappend LVARS $name
            }
            note var $name
        }
        array {
            if {[lsearch -exact $ARRS $name] < 0} {
                lappend ARRS $name
            }
        }
        dict {
            if {[lsearch -exact $DICTS $name] < 0} {
                lappend DICTS $name
            }
            note var $name
        }
    }
}

# A scalar read: a variable that is certain to be set here, or — at
# `UNSET_RATE` — one that was never set.
proc rvar {} {
    global VARS UNSET_RATE INPROC LOCALS
    set pool $VARS
    if {$INPROC} {
        set pool $LOCALS
    }
    if {[llength $pool] == 0 || [rchance $UNSET_RATE]} {
        return "\$[fresh u]"
    }
    return "\$[rpick $pool]"
}

proc rnumvar {} {
    global NVARS INPROC POOL_INT
    if {$INPROC || [llength $NVARS] == 0} {
        return [rpick $POOL_INT]
    }
    return "\$[rpick $NVARS]"
}

proc rlistvar {} {
    global LVARS INPROC POOL_AWKWARD
    if {$INPROC || [llength $LVARS] == 0} {
        return [Q [rpick $POOL_AWKWARD]]
    }
    return "\$[rpick $LVARS]"
}

# ── expressions ─────────────────────────────────────────────────────────────

set OPS_ARITH [list + - * / % **]
set OPS_CMP [list < > <= >= == !=]
set OPS_STR [list lt gt le ge eq ne]
set OPS_BIT [list & | ^ << >>]
set OPS_LOGIC [list && ||]
set OPS_MEMBER [list in ni]

# An operand: a literal, a variable, or a nested command substitution.
proc operand {depth} {
    global POOL_INT POOL_FLOAT POOL_BIG
    set r [rint 100]
    if {$depth > 0 && $r < 12} {
        return "([expr_body [expr {$depth - 1}]])"
    }
    if {$depth > 0 && $r < 18} {
        return "\[llength [rlistvar]\]"
    }
    if {$depth > 0 && $r < 22} {
        return "\[string length [value]\]"
    }
    if {$r < 34} {
        return [rnumvar]
    }
    if {$r < 42} {
        return [rpick $POOL_FLOAT]
    }
    if {$r < 46} {
        return [rpick $POOL_BIG]
    }
    if {$r < 54} {
        return "\"[rpick [list a b abc 10 9 1.0]]\""
    }
    return [rpick $POOL_INT]
}

# An `expr` body, without the surrounding braces.
proc expr_body {depth} {
    global OPS_ARITH OPS_CMP OPS_STR OPS_BIT OPS_LOGIC OPS_MEMBER
    global POOL_SMALL POOL_AWKWARD
    if {$depth <= 0} {
        return [operand 0]
    }
    set d [expr {$depth - 1}]
    set r [rint 100]
    if {$r < 34} {
        return "[operand $d] [rpick $OPS_ARITH] [operand $d]"
    }
    if {$r < 46} {
        return "[operand $d] [rpick $OPS_CMP] [operand $d]"
    }
    if {$r < 56} {
        return "[operand $d] [rpick $OPS_STR] [operand $d]"
    }
    if {$r < 66} {
        return "[operand $d] [rpick $OPS_BIT] [rpick $POOL_SMALL]"
    }
    if {$r < 74} {
        return "[operand $d] [rpick $OPS_LOGIC] [operand $d]"
    }
    if {$r < 80} {
        return "[operand $d] [rpick $OPS_MEMBER] [Q [rpick $POOL_AWKWARD]]"
    }
    if {$r < 86} {
        return "[rpick [list ! ~ -]][operand $d]"
    }
    if {$r < 92} {
        return "[operand $d] ? [operand $d] : [operand $d]"
    }
    return "[expr_body $d] [rpick $OPS_ARITH] [operand $d]"
}

# A word that is a value: a literal, a variable, or a command substitution.
proc word {depth} {
    set r [rint 100]
    if {$r < 40} {
        return [value]
    }
    if {$r < 60} {
        return [rvar]
    }
    if {$r < 72} {
        return "\[expr \{[expr_body $depth]\}\]"
    }
    if {$r < 80} {
        return "\"pre-[rvar]-post\""
    }
    if {$r < 88} {
        return "\[list [value] [value]\]"
    }
    if {$r < 94} {
        return "\[string length [value]\]"
    }
    return "\[format %s [value]\]"
}

# ── statements ──────────────────────────────────────────────────────────────
#
# `ctx` is a list of flags that rule some statements out:
#   loop    — inside a loop body, so `break`/`continue` are legal
#   catch   — inside a `catch` script, so `break`/`continue`/`return` are not
#             (tclrs refuses an exit out of a catch script — README [0x05])
#   proc    — inside a procedure body, so `eval`, `proc` and `coroutine` are not

proc has {ctx flag} {
    return [expr {[lsearch -exact $ctx $flag] >= 0}]
}

# A body: a `;`-joined run of statements inside braces, on one line. `NESTED`
# rises for the duration, so nothing a body assigns counts as certainly set.
proc body {depth ctx} {
    global NESTED
    incr NESTED
    set n [expr {1 + [rint 2]}]
    set parts [list]
    for {set i 0} {$i < $n} {incr i} {
        lappend parts [stmt [expr {$depth - 1}] $ctx]
    }
    incr NESTED -1
    return "\{[join $parts {; }]\}"
}

proc stmt {depth ctx} {
    global VARS NVARS LVARS ARRS DICTS PROCS COUNTERS
    global POOL_AWKWARD POOL_GLOB
    if {$depth <= 0} {
        # A leaf: something with no body of its own.
        return [leaf_stmt $ctx]
    }
    set r [rint 100]
    if {$r < 40} {
        return [leaf_stmt $ctx]
    }
    if {$r < 52} {
        # if / elseif / else
        set s "if \{[expr_body 2]\} [body $depth $ctx]"
        if {[rchance 30]} {
            append s " elseif \{[expr_body 2]\} [body $depth $ctx]"
        }
        if {[rchance 50]} {
            append s " else [body $depth $ctx]"
        }
        return $s
    }
    if {$r < 62} {
        # while, counting a fresh counter. `incr` is the first statement of the
        # body so a generated `continue` cannot skip it.
        set c [fresh w]
        set n [expr {1 + [rint 5]}]
        note num $c
        lappend COUNTERS $c
        return "set $c 0; while \{\$$c < $n\} \{incr $c; [inner_body $depth $ctx]\}"
    }
    if {$r < 70} {
        # for. Tcl runs the increment on `continue`, so the body is free.
        set c [fresh f]
        set n [expr {1 + [rint 5]}]
        note num $c
        lappend COUNTERS $c
        return "for \{set $c 0\} \{\$$c < $n\} \{incr $c\} \{[inner_body $depth $ctx]\}"
    }
    if {$r < 80} {
        # foreach, over one or two variable/value lists. The loop variable is
        # only certainly set when the value list has an element and parses as a
        # list at all, so registering it is conditional on both.
        set v [fresh e]
        set raw [rpick $POOL_AWKWARD]
        set len 0
        catch {set len [llength $raw]}
        if {$len > 0} {
            note var $v
        }
        set vals [Q $raw]
        if {[rchance 30]} {
            set v2 [fresh e]
            if {$len > 1} {
                note var $v2
            }
            return "foreach \{$v $v2\} $vals \{[inner_body $depth $ctx]\}"
        }
        return "foreach $v $vals \{[inner_body $depth $ctx]\}"
    }
    if {$r < 88} {
        # switch, with the option forms tclrs implements.
        set opt [rpick [list -exact -glob "-exact --" "-glob --" --]]
        set arms [list]
        set n [expr {1 + [rint 3]}]
        for {set i 0} {$i < $n} {incr i} {
            lappend arms "[Q [rpick $POOL_GLOB]] [body $depth $ctx]"
        }
        if {[rchance 70]} {
            lappend arms "default [body $depth $ctx]"
        }
        return "switch $opt [word 1] \{[join $arms { }]\}"
    }
    if {$r < 96} {
        # catch, whose script may not exit a loop or return.
        set m [fresh m]
        note var $m
        set inner [lsort -unique [concat $ctx [list catch]]]
        set inner [lsearch -all -inline -not -exact $inner loop]
        return "catch [body $depth $inner] $m; puts m:\$$m"
    }
    return [leaf_stmt $ctx]
}

# A loop body: statements, plus `break`/`continue` at some rate. The caller has
# already guaranteed progress, so an exit here cannot make the loop unbounded.
proc inner_body {depth ctx} {
    global NESTED
    incr NESTED
    set inner [lsort -unique [concat $ctx [list loop]]]
    set n [expr {1 + [rint 2]}]
    set parts [list]
    for {set i 0} {$i < $n} {incr i} {
        lappend parts [stmt [expr {$depth - 1}] $inner]
    }
    incr NESTED -1
    if {[rchance 25] && ![has $ctx catch]} {
        set exit [rpick [list break continue]]
        if {[rchance 50]} {
            lappend parts "if \{[expr_body 1]\} \{$exit\}"
        } else {
            lappend parts $exit
        }
    }
    return [join $parts {; }]
}

# A statement with no nested body.
proc leaf_stmt {ctx} {
    global VARS NVARS LVARS ARRS DICTS PROCS INPROC COUNTERS
    global POOL_INT POOL_SMALL POOL_INDEX POOL_FMT
    set r [rint 100]
    if {$r < 12} {
        set v [fresh v]
        note var $v
        if {[rchance 40]} {
            note num $v
            return "set $v [rpick $POOL_INT]"
        }
        if {[rchance 30]} {
            note list $v
            return "set $v [Q [rpick [list "a b c" "1 2 3" "" "a" "x \{y z\}" "a b  c"]]]"
        }
        return "set $v [word 1]"
    }
    if {$r < 18} {
        if {[llength $NVARS] == 0} {
            return "puts [word 1]"
        }
        return "incr [rpick $NVARS] [rpick $POOL_SMALL]"
    }
    if {$r < 24} {
        if {[llength $VARS] == 0} {
            return "puts [word 1]"
        }
        return "append [rpick $VARS] [value] [value]"
    }
    if {$r < 40} {
        if {[rchance 20]} {
            return "puts -nonewline [word 2]"
        }
        return "puts [word 2]"
    }
    if {$r < 50} {
        return [list_stmt]
    }
    if {$r < 58} {
        return [string_stmt]
    }
    if {$r < 64} {
        # tclrs refuses `array` and `dict` on a procedure-local variable
        # (README [0x05]), so inside a body those would classify the case as a
        # skip rather than compare it.
        if {$INPROC} {
            return "puts [word 1]"
        }
        return [array_stmt]
    }
    if {$r < 70} {
        if {$INPROC} {
            return "puts [word 1]"
        }
        return [dict_stmt]
    }
    if {$r < 74} {
        return "puts \[format [Q [rpick $POOL_FMT]] [word 1]\]"
    }
    if {$r < 78} {
        if {[llength $PROCS] == 0} {
            return "puts [word 1]"
        }
        return [call_stmt]
    }
    if {$r < 82} {
        if {[has $ctx proc] || [has $ctx catch]} {
            return "puts [word 1]"
        }
        # `eval` of a script that is a value, which is the path the compiler
        # cannot see through.
        return "eval [Q "puts [rpick [list a b 1]]"]"
    }
    if {$r < 86} {
        # Inside a procedure the top-level names are not in scope, and tclrs
        # refuses `unset` of a procedure-local one (README [0x05]), so the
        # statement only appears where it can name something real. A loop
        # counter is never a candidate: unsetting one makes the next `incr`
        # start again from zero, and the loop it counts would never end — the
        # one way a generated program could fail to terminate.
        set targets [list]
        foreach cand $VARS {
            if {[lsearch -exact $COUNTERS $cand] < 0} {
                lappend targets $cand
            }
        }
        if {$INPROC || [llength $targets] == 0} {
            return "puts [word 1]"
        }
        set v [rpick $targets]
        set VARS [lsearch -all -inline -not -exact $VARS $v]
        set NVARS [lsearch -all -inline -not -exact $NVARS $v]
        set LVARS [lsearch -all -inline -not -exact $LVARS $v]
        return "unset $v"
    }
    if {$r < 90} {
        if {[has $ctx catch] || ![has $ctx proc]} {
            return "puts [word 1]"
        }
        return "return [word 1]"
    }
    if {$r < 94} {
        return "error [value]"
    }
    if {$r < 99} {
        return "puts \[string index [value] [rpick $POOL_INDEX]\]"
    }
    # Raw text with no guarantee of being well formed at all: the parser's own
    # divergences are as much a parity surface as the commands'. Both engines
    # see the same bytes, so a case that neither can parse still compares their
    # two messages.
    return [rpick [list "puts \{a" "puts \"a" "puts \[list a" "set" \
        "puts \$" "list \{a \{b\}" "puts a\}" "expr \{1 +\}" "incr" \
        "puts \[expr \{1/0\}\]" "puts \[expr \{1 % 0\}\]"]]
}

proc list_stmt {} {
    global LVARS POOL_INDEX POOL_GLOB POOL_SMALL
    set l [rlistvar]
    set r [rint 100]
    if {$r < 12} {
        set v [fresh l]
        note list $v
        return "set $v \[list [value] [value] [value]\]"
    }
    if {$r < 22} {
        if {[llength $LVARS] == 0} {
            return "puts \[llength $l\]"
        }
        return "lappend [rpick $LVARS] [value]"
    }
    if {$r < 32} {
        return "puts \[lindex $l [rpick $POOL_INDEX]\]"
    }
    if {$r < 40} {
        return "puts \[llength $l\]"
    }
    if {$r < 48} {
        return "puts \[lrange $l [rpick $POOL_INDEX] [rpick $POOL_INDEX]\]"
    }
    if {$r < 54} {
        return "puts \[lreverse $l\]"
    }
    if {$r < 60} {
        return "puts \[linsert $l [rpick $POOL_INDEX] [value]\]"
    }
    if {$r < 66} {
        return "puts \[lreplace $l [rpick $POOL_INDEX] [rpick $POOL_INDEX] [value]\]"
    }
    if {$r < 74} {
        set opt [rpick [list "" -exact -glob -all -inline -not -integer -real \
            -increasing -decreasing -start]]
        if {$opt eq "-start"} {
            return "puts \[lsearch -start [rpick $POOL_SMALL] $l [Q [rpick $POOL_GLOB]]\]"
        }
        return "puts \[lsearch $opt $l [Q [rpick $POOL_GLOB]]\]"
    }
    if {$r < 82} {
        set opt [rpick [list "" -ascii -integer -real -increasing -decreasing \
            -unique -indices]]
        return "puts \[lsort $opt $l\]"
    }
    if {$r < 88} {
        return "puts \[join $l [Q [rpick [list , " " "" "--" "\n"]]]\]"
    }
    if {$r < 94} {
        return "puts \[split [value] [Q [rpick [list " " , "" ab " ,"]]]\]"
    }
    return "puts \[concat $l [value] [rlistvar]\]"
}

proc string_stmt {} {
    global POOL_INDEX POOL_GLOB POOL_STRCLASS POOL_SMALL
    set s [rint 100]
    set a [value]
    set b [value]
    if {$s < 8} {
        return "puts \[string length $a\]"
    }
    if {$s < 14} {
        return "puts \[string index $a [rpick $POOL_INDEX]\]"
    }
    if {$s < 22} {
        return "puts \[string range $a [rpick $POOL_INDEX] [rpick $POOL_INDEX]\]"
    }
    if {$s < 28} {
        return "puts \[string compare $a $b\]"
    }
    if {$s < 34} {
        return "puts \[string equal $a $b\]"
    }
    if {$s < 40} {
        return "puts \[string first $a $b\]"
    }
    if {$s < 44} {
        return "puts \[string last $a $b\]"
    }
    if {$s < 50} {
        return "puts \[string match [Q [rpick $POOL_GLOB]] $a\]"
    }
    if {$s < 56} {
        return "puts \[string map [Q [rpick [list "a b" "a b c d" "" "ab X"]]] $a\]"
    }
    if {$s < 62} {
        return "puts \[string repeat $a [rpick $POOL_SMALL]\]"
    }
    if {$s < 68} {
        return "puts \[string replace $a [rpick $POOL_INDEX] [rpick $POOL_INDEX] $b\]"
    }
    if {$s < 72} {
        return "puts \[string reverse $a\]"
    }
    if {$s < 78} {
        return "puts \[string [rpick [list tolower toupper totitle]] $a\]"
    }
    if {$s < 84} {
        return "puts \[string [rpick [list trim trimleft trimright]] $a [Q [rpick [list " " ab "" "\{"]]]\]"
    }
    if {$s < 88} {
        return "puts \[string insert $a [rpick $POOL_INDEX] $b\]"
    }
    if {$s < 94} {
        return "puts \[string is [rpick $POOL_STRCLASS] $a\]"
    }
    return "puts \[string cat $a $b [value]\]"
}

proc array_stmt {} {
    global ARRS POOL_GLOB
    if {[llength $ARRS] == 0 || [rchance 25]} {
        set a [fresh a]
        note array $a
        return "array set $a [Q [rpick [list "x 1 y 2" "k v" "" "a 1 b 2 c 3" \
            "\{\} e" "1 one 2 two"]]]"
    }
    set a [rpick $ARRS]
    set r [rint 100]
    if {$r < 16} {
        return "set ${a}([rpick [list x y k \{\} 0]]) [value]"
    }
    if {$r < 30} {
        return "puts \$${a}([rpick [list x y k 0]])"
    }
    if {$r < 44} {
        # Sorted on purpose: `array names` order is unspecified in Tcl and tclrs
        # sorts where tclsh hashes, so the unsorted form would report a
        # divergence that the manual permits. The allowlist entry for it stays
        # in place for corpora that do not canonicalise.
        return "puts \[lsort \[array names $a\]\]"
    }
    if {$r < 56} {
        return "puts \[lsort \[array get $a\]\]"
    }
    if {$r < 68} {
        return "puts \[array size $a\]"
    }
    if {$r < 78} {
        return "puts \[array exists $a\]"
    }
    if {$r < 88} {
        return "puts \[lsort \[array names $a [rpick [list -exact -glob]] [Q [rpick $POOL_GLOB]]\]\]"
    }
    return "array unset $a [Q [rpick $POOL_GLOB]]"
}

proc dict_stmt {} {
    global DICTS POOL_GLOB
    if {[llength $DICTS] == 0 || [rchance 25]} {
        set d [fresh d]
        note dict $d
        return "set $d \[dict create [value] [value] [value] [value]\]"
    }
    set d [rpick $DICTS]
    set r [rint 100]
    if {$r < 16} {
        return "dict set $d [value] [value]"
    }
    if {$r < 30} {
        return "puts \[dict get \$$d [value]\]"
    }
    if {$r < 42} {
        return "puts \[dict keys \$$d\]"
    }
    if {$r < 52} {
        return "puts \[dict values \$$d\]"
    }
    if {$r < 62} {
        return "puts \[dict size \$$d\]"
    }
    if {$r < 72} {
        return "puts \[dict exists \$$d [value]\]"
    }
    if {$r < 82} {
        return "puts \[dict remove \$$d [value]\]"
    }
    if {$r < 92} {
        return "puts \[dict merge \$$d \[dict create [value] [value]\]\]"
    }
    return "puts \[dict get \[dict merge \$$d\] [value]\]"
}

# A call of a procedure the case defined, with an argument count drawn around
# the signature so both under- and over-supply are exercised.
proc call_stmt {} {
    global PROCS
    set spec [rpick $PROCS]
    set name [lindex $spec 0]
    set required [lindex $spec 1]
    set optional [lindex $spec 2]
    set variadic [lindex $spec 3]
    set n $required
    set r [rint 100]
    if {$r < 20 && $required > 0} {
        set n [expr {$required - 1}]
    } elseif {$r < 55} {
        set n [expr {$required + [rint [expr {$optional + 1}]]}]
    } elseif {$r < 70 || $variadic} {
        set n [expr {$required + $optional + [rint 3]}]
    }
    set args [list]
    for {set i 0} {$i < $n} {incr i} {
        lappend args [word 1]
    }
    return "puts \[$name [join $args { }]\]"
}

# ── whole cases ─────────────────────────────────────────────────────────────

# A procedure definition at the case's top level: required parameters, defaulted
# ones, and a trailing `args`.
proc gen_proc {depth} {
    global PROCS INPROC LOCALS VARS
    set name [fresh p]
    set required [rint 3]
    set optional [rint 2]
    set variadic [rchance 30]
    set formals [list]
    set body [list]
    set locals [list __o]
    for {set i 1} {$i <= $required} {incr i} {
        lappend formals "r$i"
        lappend locals "r$i"
        lappend body "append __o \$r$i"
    }
    for {set i 1} {$i <= $optional} {incr i} {
        lappend formals "\{o$i [value]\}"
        lappend locals "o$i"
        lappend body "append __o \$o$i"
    }
    if {$variadic} {
        lappend formals args
        lappend locals args
        lappend body "append __o \[llength \$args\]"
    }
    set stmts [list "set __o \{\}"]
    foreach b $body {
        lappend stmts $b
    }
    # A `global` declaration, so the body reaches a top-level variable — the one
    # way a procedure legitimately sees one.
    if {[rchance 30] && [llength $VARS] > 0} {
        set g [rpick $VARS]
        lappend stmts "global $g"
        lappend locals $g
        lappend stmts "append __o \$$g"
    }
    # Inside the body only these names are in scope.
    set INPROC 1
    set LOCALS $locals
    lappend stmts [stmt [expr {$depth - 1}] [list proc]]
    if {[rchance 25]} {
        lappend stmts "if \{[expr_body 1]\} \{return -code error [value]\}"
    }
    set INPROC 0
    set LOCALS [list]
    lappend stmts "return \$__o"
    lappend PROCS [list $name $required $optional $variadic]
    return "proc $name \{[join $formals { }]\} \{[join $stmts {; }]\}"
}

# A coroutine: a procedure that yields a bounded number of times, entered once
# and resumed exactly as many times as it yields, so no call ever reaches a
# coroutine whose body has ended.
proc gen_coroutine {} {
    set p [fresh c]
    set yields [expr {1 + [rint 3]}]
    set stmts [list]
    for {set i 1} {$i <= $yields} {incr i} {
        lappend stmts "set __y \[yield y$i\]"
        lappend stmts "puts got:\$__y"
    }
    lappend stmts "return done"
    set out [list "proc $p \{\} \{[join $stmts {; }]\}"]
    set co [fresh k]
    lappend out "puts \[coroutine $co $p\]"
    for {set i 1} {$i <= $yields} {incr i} {
        lappend out "puts \[$co r$i\]"
    }
    return $out
}

# One case: a prologue that creates variables, optional procedures, a body, an
# optional coroutine, and an epilogue that prints every variable the case is
# certain to have set — which is how the final state, and not only what the
# case printed as it ran, is compared.
proc gen_case {depth} {
    global VARS ARRS DICTS LINES POOL_INT POOL_AWKWARD
    reset_case
    set n [expr {2 + [rint 3]}]
    for {set i 0} {$i < $n} {incr i} {
        set v [fresh s]
        set r [rint 100]
        if {$r < 40} {
            note num $v
            emit "set $v [rpick $POOL_INT]"
        } elseif {$r < 65} {
            note list $v
            emit "set $v [Q [rpick [list "a b c" "1 2 3" "" "x \{y z\}" "a b  c" "\{\} a"]]]"
        } else {
            note var $v
            emit "set $v [value]"
        }
    }
    # An array and a dict at the top level, so the later statements have one to
    # read rather than only ever creating a fresh one.
    if {[rchance 45]} {
        emit [array_stmt]
    }
    if {[rchance 45]} {
        emit [dict_stmt]
    }
    set nprocs [rint 3]
    for {set i 0} {$i < $nprocs} {incr i} {
        emit [gen_proc $depth]
    }
    set nstmts [expr {3 + [rint 7]}]
    for {set i 0} {$i < $nstmts} {incr i} {
        emit [stmt $depth [list]]
    }
    if {[rchance 15]} {
        foreach line [gen_coroutine] {
            emit $line
        }
    }
    foreach v $VARS {
        emit "puts $v=\$$v"
    }
    foreach a $ARRS {
        emit "puts $a=\[lsort \[array get $a\]\]"
    }
    return $LINES
}

# ── main ────────────────────────────────────────────────────────────────────

set seed 1
set count 200
set depth 3
if {[llength $argv] > 0} {
    set seed [lindex $argv 0]
}
if {[llength $argv] > 1} {
    set count [lindex $argv 1]
}
if {[llength $argv] > 2} {
    set depth [lindex $argv 2]
}

set S [expr {($seed == 0) ? 1 : ($seed & 0xFFFFFFFF)}]
# Discard the first words: a small seed's early xorshift output is poorly mixed,
# and neighbouring seeds would otherwise generate near-identical corpora.
for {set i 0} {$i < 8} {incr i} {
    rnext
}

for {set i 0} {$i < $count} {incr i} {
    puts "#=== $i"
    foreach line [gen_case $depth] {
        puts $line
    }
}
