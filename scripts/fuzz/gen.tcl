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
#
# What it reaches, and the three things it deliberately does not:
#
# * **Built, not listed.** `format`'s specifier matrix (`fmt_spec`: flags ×
#   width × precision × conversion, `*` included), the `lsearch` and `lsort`
#   option matrices (`lsearch_opts`, `lsort_opts`), and every `string`
#   subcommand in every argument shape its synopsis allows. A hand-written list
#   of two dozen spellings never reaches a combination.
# * **Stateful, not only nested.** Coroutines are resumed from a counted loop,
#   from inside a procedure and inside a `catch`, and suspend inside a loop and
#   inside an open `catch` region; procedures call procedures along a call graph
#   that cannot cycle; `eval` nests several levels; a `catch` wraps a counted
#   loop that calls into all of it.
# * **The corners are generated, not avoided** — at `RARE_SHAPE_RATE`. See the
#   comment there.
#
# Out of reach, and correctly so: `{*}` expansion, `regexp`, `upvar`,
# `namespace` and file I/O are outside tclrs's command set entirely, so
# generating them would produce nothing but `invalid command name` and would say
# nothing about parity. They belong here on the day the commands exist.

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
#
# The last four rows are the classes that have already caught a bug and are kept
# together so a value that stops appearing is visible as a deleted row:
#
# * the integer grammar's own corner cases — `1_0`, `0d9`, `0x_10`, `-0`, a `_`
#   in every radix — which `expr::parse_number` and `runtime::parse_number` read
#   with two different grammars (BUGS.md, "expr's *literal* number grammar");
# * `nan` / `inf` and their spellings, which are a bareword to the integer parser
#   and a value to the double parser;
# * values that straddle the i64 ends from both sides, one below, one at, one
#   above, so an off-by-one in the boundary test is reachable rather than only
#   the far-outside case;
# * multi-byte text with the non-ASCII character *at* a string boundary — first
#   character, last character, alone — which is where a byte index passes for a
#   character index everywhere except at the edge, plus astral-plane text that is
#   two UTF-16 units and one character;
# * list-shaped strings, so a scalar position receives something whose list
#   reading has a different length than its string reading.

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
    "a b c" "1 2 3" "\{a b\} c" "end" "end-1" "*" "a*b" "?" "\[ab\]" \
    0x_10 0b_101 0o_17 1_0_0 0d_9 0d09 -0d9 +0 -0x10 08 09 0_1 \
    nan inf -inf Inf NaN infinity -nan 1e999 -1e999 \
    9223372036854775806 -9223372036854775807 -9223372036854775809 \
    18446744073709551615 0x7fffffffffffffff 0x8000000000000000 \
    4611686018427387904 -4611686018427387905 \
    "é" "éa" "aé" "ée" "  é  " "日" "日a" "a日" "ñ" "Ω" "øx" "xø" \
    "😀" "a😀" "😀a" "😀😀" "x😀y" "𝄞" "a𝄞" \
    "a b" "\{a\} b" "a \{b c\}" "\{\}" "\{\} \{\}" " a" "a " "a  b"]

# Integers that stay inside i64 under the arithmetic the generator emits.
set POOL_INT [list 0 1 2 3 4 5 7 8 10 16 42 -1 -2 -7 100 255 1000 65535 -65536 \
    123456789 4611686018427387903 -4611686018427387904]

# Integers at or over the i64 boundary: tclsh promotes to a bignum, tclrs
# reports `integer value too large to represent` (BUGS.md, "Arbitrary-precision
# integers"). Drawn rarely so the corpus is not mostly that one skip.
set POOL_BIG [list 9223372036854775807 -9223372036854775808 \
    9223372036854775808 99999999999999999999]

# `nan` and `inf` are floating-point *literals* to `expr(n)` — `expr {inf > 1}`
# is 1 — and a bareword to an integer parser, so they belong with the floats
# rather than with the awkward strings, and they reach `format`'s conversions
# through `fmt_arg` from here as well.
set POOL_FLOAT [list 0.0 -0.0 1.0 0.5 -1.5 3.14 0.1 1e300 1.0e-7 2.5e-3 1e10 \
    nan inf -inf Inf NaN]

# Small counts: everything that could allocate or iterate is drawn from here.
set POOL_SMALL [list 0 1 2 3 4]

# Shift counts. `<<` and `>>` are the one place where the right operand's *sign*
# and its size against the word width both decide the answer: `expr(n)` makes a
# negative count an error, and a left shift past 63 bits is where tclsh promotes
# to a bignum. A count from `POOL_SMALL`, which is what this used to be, reaches
# neither. Bounded at 1000 because tclsh computes the bignum for real and the
# digits are the output being compared.
set POOL_SHIFT [list 0 1 2 3 4 7 8 15 16 31 32 62 63 64 65 127 1000 \
    -1 -2 -8 -64]

# Index forms. `Tcl_GetIntForIndex` takes `end`, `end±n` and `m±n`, the integer
# grammar underneath it takes every radix and `_`, and an index past the i64 ends
# is where tclsh's arbitrary-precision arithmetic and tclrs's saturation part
# company (BUGS.md, "Indices outside i64") — so all three are drawn here rather
# than only the small in-range ones.
set POOL_INDEX [list 0 1 2 3 -1 end end-1 end+1 5 0x2 1_0 \
    end-0 end+0 end--1 end-end -0 +1 0x_2 0d3 007 1_0_0 \
    9223372036854775807 -9223372036854775808 9223372036854775808 \
    end-9223372036854775807 end+9223372036854775807 \
    1.5 1e2 a end-a "" "end "]

set POOL_GLOB [list * a* *b "a?c" "\[ab\]*" "" x "a*b*c" \
    "\\*" "\[a-c\]" "\[!ab\]" "?" "**" "é*" "*😀*" "\[\]" "a\[b"]

# `string is` classes, in two lists. The second held the four that needed the
# Unicode category tables and were refused until those landed; all four are
# answered now and are compared like the rest, so the split is a draw weight and
# no longer a skip. It is kept because the four are still the ones a class table
# is most likely to get wrong. What *is* still refused is a code point tclsh 9.0.4
# categorises and Unicode 16.0 does not, which the value pool reaches through the
# answered classes — that one is a skip the report counts.
set POOL_STRCLASS [list alnum alpha ascii boolean control digit double entier \
    false integer list lower space true upper wideinteger wordchar xdigit]

set POOL_STRCLASS_RARE [list graph print punct dict]

# `format`'s specifier matrix is built rather than listed — see `fmt_spec`. This
# pool is the hand-written spellings that a random build does not reach: the
# literal `%%`, the XPG positional form, and the length modifiers.
set POOL_FMT [list %s %d %i %u %o %x %X %b %c %e %E %f %g %G \
    %5d %-8s %+d %08.3f %.3s %5.2f %#x %#o %lld %hd \
    %% %ld %lu %llx %hhd %j %q %a %A %p %n %S %v \
    "%1\$s" "%2\$s%1\$s" "%s%%" "%*d" "%.*f" "%-*.*s" "% d" "%+.0f" \
    "%#b" "%#.4o" "%08s" "%-0d" "%.0d" "%.20f" "%40s" "%-40s"]

# `format`'s flag / width / precision / conversion axes, built into a specifier
# by `fmt_spec` so the combinations are reached rather than the two dozen
# spellings a hand-written list can hold.
#
# Width and precision are bounded at two digits on purpose, and the bound is
# printed in the run's report. `format`'s unbounded field width and its
# precision above 65535 are two *already recorded* crashes (BUGS.md, "Crashes
# reachable from a script": `format %9223372036854775807d 1` aborts the process
# on the allocation, `format %.65536f 1.0` panics), each pinned by its own test.
# Drawing them here would spend most of a run re-finding the same two aborts —
# an abort takes the process down rather than reporting — instead of reaching the
# combinations that are not yet known. This bounds a value pool, exactly as the
# loop trip count is bounded; it changes no classification, and a case that does
# reach either crash from any other route is still CRITICAL.
set POOL_FMT_FLAGS [list "" - + " " 0 # -+ 0# "-0" "+ " "#0-"]
set POOL_FMT_WIDTH [list "" 0 1 2 5 8 12 40 *]
set POOL_FMT_PREC [list "" .0 .1 .2 .3 .8 .17 .40 .*]
set POOL_FMT_CONV [list d i u o x X b c s e E f g G]

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

# How often a statement is drawn from the corner of a command rather than its
# middle — `array` on a procedure local, `eval` inside a procedure body,
# `lsort -command`, `string is punct`, `string wordstart`, `dict update`, and the
# rest.
#
# Every one of those was a *refusal* when this rate was chosen, which is why the
# name and the comments here said so and why the rate is low: a refusal is
# decided while compiling, so one of them anywhere in a case took the whole case
# out of comparison and into the SKIP bucket. All of them have since landed.
# Measured on 200 cases at depth 4, seed 1: the SKIP bucket is 1, and the one
# entry in it is `format %a`.
#
# So this is now a draw weight and not a trade. The shapes are still the corners
# — the argument forms each command gained last, and the ones most likely to be
# got wrong — and drawing them rarely means most statements exercise the common
# path. Whether the rate should now go *up*, since these cost a comparison
# nothing any more, is a real question and a separate change: raising it changes
# what every seed generates, and that belongs in a run of its own with the bucket
# counts before and after.
set RARE_SHAPE_RATE 8

proc rare_shape {} {
    global RARE_SHAPE_RATE
    return [rchance $RARE_SHAPE_RATE]
}

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
    global POOL_SMALL POOL_AWKWARD POOL_SHIFT
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
        # A shift's right operand comes from the shift pool — negative counts
        # and counts past the word width — and the other bitwise operators keep
        # the small one.
        set op [rpick $OPS_BIT]
        if {$op eq "<<" || $op eq ">>"} {
            return "[operand $d] $op [rpick $POOL_SHIFT]"
        }
        return "[operand $d] $op [rpick $POOL_SMALL]"
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
    global VARS NVARS LVARS ARRS DICTS PROCS INPROC COUNTERS LOCALS
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
        # Inside a procedure body an `array` command names a frame slot, which
        # tclrs refuses (`"array set" of the procedure-local variable "a" is not
        # supported yet`). The statement is generated anyway, at a rate low
        # enough not to turn most procedure-bearing cases into skips: the case
        # lands in SKIP under that wording, which is the coverage that becomes a
        # comparison the day the refusal goes. `global` first is the spelling
        # that already works, and it is drawn alongside so the array path
        # through a procedure is measured too.
        if {$INPROC} {
            if {[rare_shape]} {
                return [local_array_stmt]
            }
            return "puts [word 1]"
        }
        return [array_stmt]
    }
    if {$r < 70} {
        # `dict` on a procedure local is *not* refused — a dict is an ordinary
        # value in a slot — so this is a comparison rather than a skip, and it
        # was being generated around for no reason.
        if {$INPROC} {
            return [local_dict_stmt]
        }
        return [dict_stmt]
    }
    if {$r < 74} {
        return [fmt_stmt]
    }
    if {$r < 78} {
        if {[llength $PROCS] == 0} {
            return "puts [word 1]"
        }
        return [call_stmt]
    }
    if {$r < 82} {
        # `eval` of a script that is a value, which is the path the compiler
        # cannot see through. Inside a procedure it is refused outright
        # (`"eval" inside a procedure is not supported: …`), and it is generated
        # there anyway so the refusal is a counted skip rather than a hole; a
        # `catch` around it is a comparison in both engines, so that context is
        # no longer routed around either.
        if {$INPROC && ![rare_shape]} {
            return "puts [word 1]"
        }
        return [eval_stmt]
    }
    if {$r < 86} {
        # A loop counter is never a candidate: unsetting one makes the next
        # `incr` start again from zero, and the loop it counts would never end —
        # the one way a generated program could fail to terminate.
        #
        # Inside a procedure the top-level names are not in scope and `unset` of
        # a frame slot is refused (`"unset" of the procedure-local variable "x"
        # is not supported yet`); that spelling is generated too, so the refusal
        # is counted.
        set targets [list]
        foreach cand $VARS {
            if {[lsearch -exact $COUNTERS $cand] < 0} {
                lappend targets $cand
            }
        }
        if {$INPROC} {
            if {[rare_shape] && [llength $LOCALS] > 0} {
                return "unset [rpick $LOCALS]"
            }
            return "puts [word 1]"
        }
        if {[llength $targets] == 0} {
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
        "puts \[expr \{1/0\}\]" "puts \[expr \{1 % 0\}\]" \
        "puts \[expr \{1 %% 2\}\]" "puts \[expr \{\}\]" "puts \[expr\]" \
        "puts \{\$\}" "puts \[\]" "puts \$\{" "puts \$\{a" \
        "puts \[expr \{0x\}\]" "puts \[expr \{1_\}\]" "puts \[expr \{0d\}\]" \
        "puts \[expr \{nan == nan\}\]" "puts \[expr \{inf - inf\}\]" \
        "puts \[expr \{2 ** 64\}\]" "puts \[expr \{-9223372036854775808\}\]" \
        "puts \[string is \]" "puts \[lsort -bogus \{a b\}\]" \
        "puts \[format\]" "puts \[format %\]" "puts \[string\]"]]
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
        return "puts \[lsearch [lsearch_opts] $l [Q [rpick $POOL_GLOB]]\]"
    }
    if {$r < 82} {
        return "puts \[lsort [lsort_opts] $l\]"
    }
    if {$r < 88} {
        return "puts \[join $l [Q [rpick [list , " " "" "--" "\n"]]]\]"
    }
    if {$r < 94} {
        return "puts \[split [value] [Q [rpick [list " " , "" ab " ,"]]]\]"
    }
    return "puts \[concat $l [value] [rlistvar]\]"
}

# ── the lsearch and lsort option matrices ───────────────────────────────────
#
# `lsearch(n)` and `lsort(n)` are almost entirely option surface, and a single
# option drawn from a flat list — which is what this used to be — never reaches a
# combination. Both build a run of one to three options instead, from the whole
# documented set. It is split in two lists: the options each command had first,
# and the ones it gained last — `-regexp`, `-sorted`, `-dictionary`, `-nocase`,
# `-index`, `-stride`, `-command`, which were refusals when the split was made
# and are all answered now. Every one is compared against tclsh today; the second
# list is drawn rarely, which is what `RARE_SHAPE_RATE` is.
#
# `-start`'s index comes from the small pool and never from `POOL_INDEX`: a
# negative start against an empty list is a SIGSEGV in tclsh 9.0.4 (BUGS.md,
# "Defects in the reference implementation"), and a case the reference cannot
# survive has no behavior to compare against.

# Every option in the manual is in one list or the other, so the split is a rate
# and not a filter: nothing is unreachable.
set OPTS_LSEARCH_MODE [list -exact -glob]
set OPTS_LSEARCH_TYPE [list -ascii -integer -real]
set OPTS_LSEARCH_MOD [list -all -inline -not -increasing -decreasing --]
set OPTS_LSEARCH_RARE [list -regexp -sorted -dictionary -nocase -bisect \
    -subindices "-index 0" "-index 1" "-stride 2" "-stride 3"]

proc lsearch_opts {} {
    global OPTS_LSEARCH_MODE OPTS_LSEARCH_TYPE OPTS_LSEARCH_MOD
    global OPTS_LSEARCH_RARE POOL_SMALL
    set opts [list]
    if {[rare_shape]} {
        lappend opts [rpick $OPTS_LSEARCH_RARE]
    }
    if {[rchance 55]} {
        lappend opts [rpick $OPTS_LSEARCH_MODE]
    }
    if {[rchance 30]} {
        lappend opts [rpick $OPTS_LSEARCH_TYPE]
    }
    if {[rchance 40]} {
        lappend opts [rpick $OPTS_LSEARCH_MOD]
    }
    if {[rchance 20]} {
        lappend opts [rpick $OPTS_LSEARCH_MOD]
    }
    if {[rchance 15]} {
        # `-start`'s index is never negative: see the note above.
        lappend opts "-start [rpick $POOL_SMALL]"
    }
    return [join $opts { }]
}

set OPTS_LSORT_ORDER [list -ascii -integer -real]
set OPTS_LSORT_MOD [list -increasing -decreasing -unique -indices --]
set OPTS_LSORT_RARE [list -dictionary -nocase "-index 0" "-index end" \
    "-stride 2" "-stride 3" "-command cmp0" "-command cmp1"]

proc lsort_opts {} {
    global OPTS_LSORT_ORDER OPTS_LSORT_MOD OPTS_LSORT_RARE
    set opts [list]
    if {[rare_shape]} {
        lappend opts [rpick $OPTS_LSORT_RARE]
    }
    if {[rchance 50]} {
        lappend opts [rpick $OPTS_LSORT_ORDER]
    }
    if {[rchance 45]} {
        lappend opts [rpick $OPTS_LSORT_MOD]
    }
    if {[rchance 20]} {
        lappend opts [rpick $OPTS_LSORT_MOD]
    }
    return [join $opts { }]
}

# ── the `string` ensemble ───────────────────────────────────────────────────
#
# Every subcommand `string(n)` documents, in every argument shape its synopsis
# allows, including the optional arguments that a fixed spelling never reaches:
# `-nocase` and `-length` on the comparisons, the start index of `first` and the
# last index of `last`, the first/last range of the case conversions, `-strict`
# and `-failindex` on `is`, and the two subcommands `wordstart` and `wordend`.
# Those three were refusals when this was written and are answered now; they are
# still drawn rarely, which is what `RARE_SHAPE_RATE` is.

# `-nocase` is answered now — it was a refusal, waiting on a case-folding table
# that matches Tcl's — and is still drawn at the low rate that split gave it.
proc nocase {} {
    if {[rare_shape]} {
        return " -nocase"
    }
    return ""
}

proc string_stmt {} {
    global POOL_INDEX POOL_GLOB POOL_STRCLASS POOL_STRCLASS_RARE POOL_SMALL
    set s [rint 100]
    set a [value]
    set b [value]
    if {$s < 6} {
        return "puts \[string length $a\]"
    }
    if {$s < 11} {
        return "puts \[string index $a [rpick $POOL_INDEX]\]"
    }
    if {$s < 17} {
        return "puts \[string range $a [rpick $POOL_INDEX] [rpick $POOL_INDEX]\]"
    }
    if {$s < 24} {
        set opt [nocase]
        if {[rchance 25]} {
            append opt " -length [rpick $POOL_SMALL]"
        }
        return "puts \[string [rpick [list compare equal]]$opt $a $b\]"
    }
    if {$s < 31} {
        # `first` takes a start index and `last` takes a last index; both are the
        # optional third argument the fixed two-argument spelling never reached.
        set sub [rpick [list first last]]
        if {[rchance 40]} {
            return "puts \[string $sub $a $b [rpick $POOL_INDEX]\]"
        }
        return "puts \[string $sub $a $b\]"
    }
    if {$s < 37} {
        return "puts \[string match[nocase] [Q [rpick $POOL_GLOB]] $a\]"
    }
    if {$s < 43} {
        return "puts \[string map[nocase] [Q [rpick [list "a b" "a b c d" "" \
            "ab X" "é E" "a \{b c\}" "a a" "aa b a c"]]] $a\]"
    }
    if {$s < 47} {
        return "puts \[string repeat $a [rpick $POOL_SMALL]\]"
    }
    if {$s < 53} {
        # `replace`'s replacement is optional: without it the range is deleted.
        if {[rchance 30]} {
            return "puts \[string replace $a [rpick $POOL_INDEX] [rpick $POOL_INDEX]\]"
        }
        return "puts \[string replace $a [rpick $POOL_INDEX] [rpick $POOL_INDEX] $b\]"
    }
    if {$s < 56} {
        return "puts \[string reverse $a\]"
    }
    if {$s < 64} {
        # The case conversions take an optional first and last index.
        set sub [rpick [list tolower toupper totitle]]
        set r [rint 100]
        if {$r < 20} {
            return "puts \[string $sub $a [rpick $POOL_INDEX] [rpick $POOL_INDEX]\]"
        }
        if {$r < 32} {
            return "puts \[string $sub $a [rpick $POOL_INDEX]\]"
        }
        return "puts \[string $sub $a\]"
    }
    if {$s < 71} {
        set sub [rpick [list trim trimleft trimright]]
        if {[rchance 25]} {
            return "puts \[string $sub $a\]"
        }
        return "puts \[string $sub $a [Q [rpick [list " " ab "" "\{" "é" "abc" \
            " \t\n" "0" "\\"]]]\]"
    }
    if {$s < 75} {
        return "puts \[string insert $a [rpick $POOL_INDEX] $b\]"
    }
    if {$s < 87} {
        return [string_is_stmt $a]
    }
    if {$s < 91} {
        # `string wordstart` / `wordend`: a refusal when this was written, and
        # a comparison now. Still refused past ASCII, which the value pool
        # reaches.
        if {[rare_shape]} {
            return "puts \[string [rpick [list wordstart wordend]] $a [rpick $POOL_INDEX]\]"
        }
        return "puts \[string range $a [rpick $POOL_INDEX] [rpick $POOL_INDEX]\]"
    }
    if {$s < 95} {
        # `cat` takes any number of arguments, none included.
        set n [rint 5]
        set args [list]
        for {set i 0} {$i < $n} {incr i} {
            lappend args [value]
        }
        return "puts \[string cat [join $args { }]\]"
    }
    # A subcommand that is not one, so the ensemble's own error is compared.
    return "puts \[string [rpick [list bogus tolowerx is1 leng compar \
        "" reverses]] $a\]"
}

# `string is CLASS ?-strict? ?-failindex VAR? STRING`.
#
# The class comes from the common set most of the time and from the rare one the
# rest — the four that needed the Unicode category tables and were refused until
# those landed. A code point tclsh 9.0.4 categorises and Unicode 16.0 does not is
# still refused by *every* class, which the value pool reaches rather than the
# class pool.
proc string_is_stmt {a} {
    global POOL_STRCLASS POOL_STRCLASS_RARE
    set class [rpick $POOL_STRCLASS]
    if {[rchance 15]} {
        set class [rpick $POOL_STRCLASS_RARE]
    }
    set opts ""
    if {[rchance 25]} {
        append opts " -strict"
    }
    if {[rare_shape]} {
        append opts " -failindex [fresh fi]"
    }
    return "puts \[string is $class$opts $a\]"
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

# ── inside a procedure body ─────────────────────────────────────────────────
#
# These three are the shapes the generator used to route around, because each is
# an unconditional refusal in tclrs and a refusal takes the whole case into SKIP
# rather than into a comparison. They are generated now: a refusal counted under
# its own wording is coverage that becomes a comparison the day the refusal goes,
# where a statement that was never generated is a hole nobody can see.

# An `array` command on a procedure-local variable. `"array set" of the
# procedure-local variable "a" is not supported yet` — an array lives in the
# global table keyed by a name index and a local lives in a frame slot
# (BUGS.md). `global` first is the spelling that *does* work, and it is drawn
# alongside so the array path through a procedure is measured rather than only
# refused.
proc local_array_stmt {} {
    global POOL_GLOB
    set a [fresh la]
    set r [rint 100]
    if {$r < 22} {
        # The working spelling: the name is a global, said so by `global`.
        return "global $a; array set $a [Q [rpick [list "x 1" "a 1 b 2" ""]]]; puts \[lsort \[array get $a\]\]"
    }
    if {$r < 40} {
        return "array set $a [Q [rpick [list "x 1 y 2" "k v" ""]]]"
    }
    if {$r < 55} {
        return "set ${a}([rpick [list x y 0 \{\}]]) [value]"
    }
    if {$r < 68} {
        return "puts \[array exists $a\]"
    }
    if {$r < 80} {
        return "puts \[array size $a\]"
    }
    if {$r < 90} {
        return "puts \[lsort \[array names $a [Q [rpick $POOL_GLOB]]\]\]"
    }
    return "array unset $a [Q [rpick $POOL_GLOB]]"
}

# A `dict` command on a procedure-local variable. This one is *not* refused — a
# dict is an ordinary value and a local is a slot that holds one — so it is a
# comparison, and the generator was routing around it for no reason. The
# subcommands outside the implemented set (`dict unset`, `dict for`, `dict
# append`, …) are drawn too and are refused by name.
proc local_dict_stmt {} {
    set d [fresh ld]
    set r [rint 100]
    if {$r < 26} {
        return "set $d \[dict create [value] [value] [value] [value]\]"
    }
    if {$r < 34} {
        return "set $d \[dict create [value] [value]\]; dict set $d [value] [value]; puts \[dict size \$$d\]"
    }
    if {$r < 44} {
        return "set $d \[dict create a 1 b 2\]; puts \[dict get \$$d [value]\]"
    }
    if {$r < 54} {
        return "set $d \[dict create a 1 b 2\]; puts \[dict keys \$$d\]"
    }
    if {$r < 62} {
        return "set $d \[dict create a 1 b 2\]; puts \[dict values \$$d\]"
    }
    if {$r < 70} {
        return "set $d \[dict create a 1 b 2\]; puts \[dict exists \$$d [value]\]"
    }
    if {$r < 78} {
        return "set $d \[dict create a 1 b 2\]; puts \[dict remove \$$d [value]\]"
    }
    if {$r < 86} {
        return "set $d \[dict create a 1\]; puts \[dict merge \$$d \[dict create [value] [value]\]\]"
    }
    # A subcommand from the far end of the ensemble. Every name here is
    # implemented now; `with` and `info` are the two the list still names that
    # are not, and those two are skips.
    if {[rare_shape]} {
        return "set $d \[dict create a 1 b 2\]; dict [rpick [list unset append \
            incr lappend replace update filter for with getwithdefault]] $d [value]"
    }
    return "set $d \[dict create a 1 b 2\]; puts \[dict get \[dict merge \$$d\] [value]\]"
}

# `eval`, of a script that is a value — the path the compiler cannot see
# through. Nested to several levels on purpose: each level is a separate chunk
# compiled at run time, and the one that fails has to report through all of them.
# Inside a procedure body it is refused outright (`"eval" inside a procedure is
# not supported: the script it builds cannot reach the procedure's local
# variables`), and it is generated there so that refusal is counted.
proc eval_stmt {} {
    set inner [rpick [list "puts a" "puts b" "puts 1" "set ev [value]" \
        "puts \[string length [value]\]" "error [value]" \
        "puts \[expr \{1 + 1\}\]" "puts \[llength [Q "a b c"]\]"]]
    set levels [expr {1 + [rint 3]}]
    set text $inner
    for {set i 0} {$i < $levels} {incr i} {
        set text "eval [Q $text]"
    }
    if {[rchance 25]} {
        # Built rather than written: `eval` over a word the compiler cannot read
        # at all.
        set m [fresh m]
        return "catch \{$text\} $m; puts m:\$$m"
    }
    return $text
}

# ── `format` ────────────────────────────────────────────────────────────────

# One specifier built from the flag / width / precision / conversion axes, as a
# two-element list: the specifier itself, and how many arguments it consumes —
# a `*` width or a `.*` precision takes one of its own, ahead of the value.
proc fmt_spec {} {
    global POOL_FMT_FLAGS POOL_FMT_WIDTH POOL_FMT_PREC POOL_FMT_CONV
    set flags [rpick $POOL_FMT_FLAGS]
    set width [rpick $POOL_FMT_WIDTH]
    set prec [rpick $POOL_FMT_PREC]
    set conv [rpick $POOL_FMT_CONV]
    set n 1
    if {$width eq "*"} {
        incr n
    }
    if {$prec eq ".*"} {
        incr n
    }
    return [list "%$flags$width$prec$conv" $n]
}

# The value a specifier is given. Every conversion is tried against every kind of
# value, not only against one that suits it: `%d` of a float, `%f` of a list,
# `%c` of a huge integer and `%x` of a negative one are where the two engines'
# conversions have to agree on an error as much as on a result.
proc fmt_arg {} {
    global POOL_INT POOL_FLOAT
    set r [rint 100]
    if {$r < 28} {
        return [rpick $POOL_INT]
    }
    if {$r < 46} {
        return [rpick $POOL_FLOAT]
    }
    if {$r < 54} {
        return [rnumvar]
    }
    return [value]
}

# The `*` argument. Bounded with the width and precision pools — see the comment
# there for why, and the run's report prints the bound.
proc fmt_star {} {
    return [rpick [list 0 1 2 5 8 12 40 -1 -8]]
}

proc fmt_stmt {} {
    global POOL_FMT
    if {[rchance 32]} {
        # A hand-written spelling, given a count of arguments that is often not
        # the count it wants, so "not enough arguments" and a trailing unused one
        # are both reached.
        set n [rint 4]
        set args [list]
        for {set i 0} {$i < $n} {incr i} {
            lappend args [fmt_arg]
        }
        return "puts \[format [Q [rpick $POOL_FMT]] [join $args { }]\]"
    }
    set text ""
    set args [list]
    set fields [expr {1 + [rint 3]}]
    for {set f 0} {$f < $fields} {incr f} {
        set spec [fmt_spec]
        if {$f > 0} {
            append text [rpick [list " " "" - :]]
        }
        append text [lindex $spec 0]
        set n [lindex $spec 1]
        for {set i 1} {$i < $n} {incr i} {
            lappend args [fmt_star]
        }
        lappend args [fmt_arg]
    }
    if {[rchance 12]} {
        # One argument short of what the specifiers ask for.
        set args [lrange $args 0 end-1]
    }
    return "puts \[format [Q $text] [join $args { }]\]"
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
    # A procedure that calls one the case defined earlier. The call graph stays
    # acyclic because this procedure is appended to `PROCS` only *after* its body
    # is generated, so it can never call itself and no cycle can close — which is
    # what bounds the depth of a generated call chain.
    if {[llength $PROCS] > 0 && [rchance 45]} {
        lappend stmts [call_stmt]
    }
    set nbody [expr {1 + [rint 3]}]
    for {set i 0} {$i < $nbody} {incr i} {
        lappend stmts [stmt [expr {$depth - 1}] [list proc]]
    }
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
#
# Four resume shapes, all with that same exact count:
#
#   flat    one `puts [$co rN]` per yield, at the case's top level;
#   loop    a `for` counting to the yield count, one resume per trip — the
#           resume is the whole body, so a generated `break` cannot appear and
#           leave the coroutine suspended with resumes still to come;
#   proc    a procedure whose body is one resume, called once per yield;
#   catch   the resumes wrapped in one `catch`, which is where a coroutine that
#           raises has to unwind into the resumer's own guarded region.
#
# The body itself is nested rather than flat at some rate: a yield inside a
# bounded loop, inside a `catch`, and after a nested `eval` are all suspension
# points at a depth the flat body never reaches, and the transfer has to restore
# the same state whichever it is.
proc gen_coroutine {} {
    set p [fresh c]
    set yields [expr {1 + [rint 3]}]
    set stmts [list]
    for {set i 1} {$i <= $yields} {incr i} {
        set shape [rint 100]
        if {$shape < 55} {
            lappend stmts "set __y \[yield y$i\]"
        } elseif {$shape < 73} {
            # Suspended inside a bounded loop: one trip, so still one yield.
            lappend stmts "set __y {}; for \{set __i$i 0\} \{\$__i$i < 1\} \{incr __i$i\} \{set __y \[yield y$i\]\}"
        } elseif {$shape < 90 || ![rare_shape]} {
            # Suspended inside an open `catch` region.
            lappend stmts "set __y {}; catch \{set __y \[yield y$i\]\} __e$i"
        } else {
            # Suspended after a nested `eval` has run in the coroutine's own
            # context. `eval` inside a procedure body was refused when this was
            # written and a coroutine's body is one, so this was a counted skip;
            # it is a comparison now, and still drawn rarely.
            lappend stmts "eval \{puts pre$i\}; set __y \[yield y$i\]"
        }
        lappend stmts "puts got:\$__y"
        if {[rchance 20]} {
            lappend stmts "puts co:\[info coroutine\]"
        }
    }
    lappend stmts "return done"
    set out [list "proc $p \{\} \{[join $stmts {; }]\}"]
    set co [fresh k]
    lappend out "puts \[coroutine $co $p\]"

    set how [rint 100]
    if {$how < 30} {
        # Resumed by a counted loop, exactly `yields` trips.
        set c [fresh cw]
        lappend out "for \{set $c 0\} \{\$$c < $yields\} \{incr $c\} \{puts \[$co r\$$c\]\}"
        return $out
    }
    if {$how < 55} {
        # Resumed from inside a procedure, called once per yield.
        set r [fresh pr]
        lappend out "proc $r \{n\} \{return \[$co r\$n\]\}"
        for {set i 1} {$i <= $yields} {incr i} {
            lappend out "puts \[$r $i\]"
        }
        return $out
    }
    if {$how < 75} {
        # Every resume inside one `catch`, so an error out of the body lands in
        # the resumer's guarded region rather than at the top level.
        set m [fresh m]
        set inner [list]
        for {set i 1} {$i <= $yields} {incr i} {
            lappend inner "puts \[$co r$i\]"
        }
        lappend out "catch \{[join $inner {; }]\} $m; puts m:\$$m"
        return $out
    }
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
    global VARS ARRS DICTS LINES POOL_INT POOL_AWKWARD PROCS COUNTERS
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
    # Up to three procedures, so a call chain of three is reachable: each may
    # call one defined before it and none can call itself.
    set nprocs [rint 4]
    for {set i 0} {$i < $nprocs} {incr i} {
        emit [gen_proc $depth]
    }
    set nstmts [expr {3 + [rint 7]}]
    for {set i 0} {$i < $nstmts} {incr i} {
        emit [stmt $depth [list]]
    }
    # A stateful tail: a `catch` around a counted loop that calls the case's own
    # procedures, so a failure raised several frames down unwinds through a loop
    # and a guarded region rather than at the top level. The trip count is a
    # literal and the counter's `incr` is the first statement of the body, the
    # same bound every other generated loop has.
    if {[llength $PROCS] > 0 && [rchance 25]} {
        set c [fresh t]
        set m [fresh m]
        note num $c
        lappend COUNTERS $c
        set inner [list "incr $c"]
        set n [expr {1 + [rint 3]}]
        for {set i 0} {$i < $n} {incr i} {
            lappend inner [call_stmt]
        }
        # One line, like every other loop the generator emits: the counter's
        # initialisation and the loop that reads it cannot be separated by the
        # shrinker deleting a line or by the mutator inserting one between them.
        emit "set $c 0; catch \{while \{\$$c < [expr {1 + [rint 3]}]\} \{[join $inner {; }]\}\} $m; puts m:\$$m"
        note var $m
    }
    if {[rchance 22]} {
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
