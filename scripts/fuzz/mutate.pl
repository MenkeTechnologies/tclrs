#!/usr/bin/env perl
# mutate.pl — build a fuzz corpus by mutating existing cases.
#
#   perl scripts/fuzz/mutate.pl SEED N SOURCE...
#
# Writes N Tcl programs to stdout in exactly the format `scripts/fuzz/gen.tcl`
# writes — each introduced by a `#=== INDEX` line — so `scripts/fuzz_parity.sh`
# splits, runs, classifies and shrinks a mutated corpus through the same code
# path as a generated one. Nothing downstream can tell the two apart, which is
# the point: there is one classifier and one shrinker, so no case is ever
# classified two ways.
#
# SOURCE is a file or a directory. A directory contributes every `*.tcl` in it,
# sorted by name; a file contributes itself. The default source is the committed
# corpus `tests/fuzz_corpus`, which is where every minimised finding lands — so
# the mutator's input is the set of programs already known to reach an
# interesting part of the language, and its job is to recombine them.
#
# Two properties are load-bearing, the same two the generator has:
#
# * **Deterministic.** The PRNG is the same 32-bit xorshift, seeded the same way
#   and warmed the same way, and the source files are read in sorted order, so
#   SEED and the source set fix the corpus byte for byte. A divergence found in
#   mutation mode reproduces from the seed and the case index alone.
# * **Terminating.** The generator's loop bounds are structural, and this
#   preserves them rather than re-deriving them:
#   - Every edit is at *line* granularity or inside a line that has no loop in
#     it. A line that opens a `while`, `for` or `foreach` is only ever moved,
#     duplicated, deleted or inserted **whole**, never rewritten, so every loop
#     in a mutant is byte-for-byte a loop the generator emitted and its bound is
#     the bound the generator proved.
#   - Nothing is ever inserted *into* a body. A generated body is inlined inside
#     braces on one line, so a line-granular insert cannot land between a loop's
#     counter and its test, or inside the body that increments it.
#   - Both halves of the guard are checked against the mutant as a whole before
#     it is emitted: every loop-bearing line must appear verbatim in some source
#     case (`loops_are_original`), and no loop's counter may be assigned off the
#     loop's own line (`counter_hazard`). A mutant that fails either is redrawn,
#     and after eight redraws the base case is emitted unmutated — the base is a
#     committed finding whose behavior under both engines is recorded, so it
#     terminates. Both counts are reported on stderr.
#
# The mutation operators, and what each is for:
#
#   splice     a line from another case, at a random position — the operator that
#              crosses two findings, which is the whole reason to mutate a corpus
#              of minimised cases rather than generate fresh ones
#   duplicate  a line, immediately after itself — a second `coroutine` resume, a
#              second `unset`, a command run twice against state it changed
#   delete     a line — the state a later line depends on stops existing
#   swap       two lines — a read moves ahead of the write that made it valid
#   literal    one literal replaced by another from the awkward pool
#   operator   one `expr` operator replaced by another of the same arity
#
# stderr carries the parameters and the counts, so a corpus can be reproduced and
# what shaped it is on the record rather than implied.

use strict;
use warnings;

my ($seed, $count, @sources) = @ARGV;
die "usage: mutate.pl SEED N SOURCE...\n" unless defined $count && @sources;
$count = int($count);

# ── PRNG: the generator's, so the two modes mix the same way ────────────────
my $S = ($seed == 0 ? 1 : ($seed & 0xFFFFFFFF));

sub rnext {
    $S = ($S ^ ($S << 13)) & 0xFFFFFFFF;
    $S = $S ^ ($S >> 17);
    $S = ($S ^ ($S << 5)) & 0xFFFFFFFF;
    return $S;
}
sub rint { my ($n) = @_; return 0 if $n <= 1; return rnext() % $n; }
sub rpick { my ($l) = @_; return $l->[rint(scalar @$l)]; }
sub rchance { my ($p) = @_; return rint(100) < $p; }

# The generator discards the first eight words for the same reason: a small
# seed's early xorshift output is poorly mixed and neighbouring seeds would
# otherwise produce near-identical corpora.
rnext() for 1 .. 8;

# ── the source cases ────────────────────────────────────────────────────────
my @files;
for my $src (@sources) {
    if (-d $src) {
        opendir my $dh, $src or die "mutate: $src: $!\n";
        push @files, map { "$src/$_" } sort grep { /\.tcl$/ } readdir $dh;
        closedir $dh;
    }
    elsif (-f $src) {
        push @files, $src;
    }
    else {
        die "mutate: no such corpus source: $src\n";
    }
}
die "mutate: no .tcl cases in: @sources\n" unless @files;

# A source file may itself be a whole corpus — `#=== N` separated — or one case.
# Both are read into the same list of cases, so `-c target/fuzz/corpus.tcl` and
# `-c tests/fuzz_corpus` are the same kind of input.
my @cases;
for my $f (@files) {
    open my $fh, "<:raw", $f or die "mutate: $f: $!\n";
    local $/;
    my $text = <$fh>;
    close $fh;
    next unless defined $text && length $text;
    if ($text =~ /^\#=== \d+$/m) {
        for my $chunk (split /^\#=== \d+\n/m, $text) {
            my @l = grep { length } split /\n/, $chunk;
            push @cases, \@l if @l;
        }
    }
    else {
        my @l = grep { length } split /\n/, $text;
        push @cases, \@l if @l;
    }
}
die "mutate: every source case was empty\n" unless @cases;

# Every line of every source, for the splice operator and for the loop check.
my @all_lines;
my %from_source;
for my $c (@cases) {
    for my $l (@$c) {
        push @all_lines, $l;
        $from_source{$l} = 1;
    }
}

# ── the termination guard ───────────────────────────────────────────────────
#
# A line that opens a loop. `while`, `for` and `foreach` are the only unbounded
# constructs the language has, and the generator's bound for each is structural:
# a fresh counter, a literal limit, and the `incr` as the body's first statement.
# The guard is that every loop-bearing line in a mutant is a line some source
# case had — so the bound is the one the generator proved, not one this script
# re-derived.
my $LOOP_RE = qr/(?:^|[\{;]\s*)(?:while|for|foreach)\s/;

sub has_loop { return $_[0] =~ $LOOP_RE; }

sub loops_are_original {
    my ($lines) = @_;
    for my $l (@$lines) {
        next unless has_loop($l);
        return 0 unless $from_source{$l};
    }
    return 1;
}

# The second half of the guard: a loop's counter must not be assigned anywhere
# but on the loop's own line.
#
# The generator emits a counted loop as one line — `set w1 0; while {$w1 < 4}
# {incr w1; …}` — so the initialisation, the test and the increment cannot be
# separated by any line-granular edit. A *shrunk* case can still leave a bare
# `set w1 …` or `puts $w1` behind after the loop it belonged to was unwrapped,
# and splicing such a line into a case that still has a loop over `w1` would put
# an arbitrary value in the counter ahead of the test: `set w1
# -9223372036854775808` before `while {$w1 < 4}` is nine quintillion trips, which
# is the one way a mutant could fail to terminate. Rejected, and the mutant falls
# back to its unmutated base.
sub counter_hazard {
    my ($lines) = @_;
    my %counter;
    for my $l (@$lines) {
        next unless has_loop($l);
        $counter{$1} = $l while $l =~ /(?:while|for)\s*\{\s*\$(\w+)\s*[<>]/g;
        $counter{$1} = $l while $l =~ /for\s*\{\s*set\s+(\w+)\s/g;
    }
    return 0 unless %counter;
    for my $l (@$lines) {
        for my $c (keys %counter) {
            next if $l eq $counter{$c};
            return 1 if $l =~ /(?:^|[\{;]\s*)set\s+\Q$c\E\s/;
        }
    }
    return 0;
}

# ── the operators ───────────────────────────────────────────────────────────

# Literals, replaced whole-token. The pool is the generator's awkward pool cut
# down to the values that are a single bare word: a replacement has to be one
# word without re-quoting, or the line's word count would change.
my @LITERALS = qw(
    0 1 2 5 42 -1 -7 255 1000 007 010 0x1f 0o17 0b101 0d9 1_0 -0 +5
    0x_10 0b_101 0d09 0_1 08 09 1_0_0
    9223372036854775807 -9223372036854775808 9223372036854775808
    9223372036854775806 -9223372036854775809 18446744073709551615
    1e300 1.0e-7 0.1 1.5 -0.0 3.0 2.5e-3 1e999
    nan inf -inf NaN Inf infinity
    a b abc xyz hello end end-1 end+1
);
my $LITERAL_TOKEN = qr/(?<![\w\$\-.])(-?(?:\d[\w.+\-]*|end(?:[-+]\w+)?))(?![\w.])/;

# `expr` operators, grouped by arity and by what they can stand in for, so a
# swap produces an expression that still parses.
my @OP_GROUPS = (
    [qw(+ - * / % **)],
    [qw(< > <= >= == !=)],
    [qw(lt gt le ge eq ne)],
    [qw(& | ^ << >>)],
    [qw(&& ||)],
    [qw(in ni)],
);

sub swap_operator {
    my ($line) = @_;
    my @hits;
    for my $g (@OP_GROUPS) {
        for my $op (@$g) {
            my $q = quotemeta $op;
            while ($line =~ /(?<= )$q(?= )/g) {
                push @hits, [pos($line) - length($op), $op, $g];
            }
        }
    }
    return $line unless @hits;
    my $hit = rpick(\@hits);
    my ($at, $op, $group) = @$hit;
    my $to = rpick($group);
    substr($line, $at, length $op) = $to;
    return $line;
}

sub perturb_literal {
    my ($line) = @_;
    my @hits;
    while ($line =~ /$LITERAL_TOKEN/g) {
        push @hits, [$-[1], $+[1] - $-[1]];
    }
    return $line unless @hits;
    my $hit = rpick(\@hits);
    substr($line, $hit->[0], $hit->[1]) = rpick(\@LITERALS);
    return $line;
}

# One mutation of one case, as a new list of lines.
sub mutate_once {
    my ($lines) = @_;
    my @l = @$lines;
    my $op = rint(100);

    if ($op < 24) {    # splice a line from anywhere in the corpus
        splice @l, rint(scalar(@l) + 1), 0, rpick(\@all_lines);
        return \@l;
    }
    if ($op < 40) {    # duplicate a line
        my $i = rint(scalar @l);
        splice @l, $i, 0, $l[$i];
        return \@l;
    }
    if ($op < 55) {    # delete a line
        return \@l if @l <= 1;
        splice @l, rint(scalar @l), 1;
        return \@l;
    }
    if ($op < 66) {    # swap two lines
        return \@l if @l < 2;
        my ($i, $j) = (rint(scalar @l), rint(scalar @l));
        @l[$i, $j] = @l[$j, $i];
        return \@l;
    }

    # The two in-line operators. A loop-bearing line is never rewritten — see
    # the termination guard — so one is chosen from the rest of the lines.
    my @editable = grep { !has_loop($l[$_]) } 0 .. $#l;
    return \@l unless @editable;
    my $i = $editable[ rint(scalar @editable) ];
    $l[$i] = $op < 85 ? perturb_literal($l[$i]) : swap_operator($l[$i]);
    return \@l;
}

sub safe {
    my ($lines) = @_;
    return @$lines && loops_are_original($lines) && !counter_hazard($lines);
}

# ── emit ────────────────────────────────────────────────────────────────────
my ($redrawn, $fellback) = (0, 0);

for my $n (0 .. $count - 1) {
    my $base = rpick(\@cases);
    my $lines;
    for my $try (1 .. 8) {
        $lines = [@$base];
        # One to four mutations, so a mutant is between a near-copy of a known
        # finding and a recombination of several.
        my $rounds = 1 + rint(4);
        $lines = mutate_once($lines) for 1 .. $rounds;
        last if safe($lines);
        $lines = undef;
        $redrawn++;
    }
    if (!defined $lines) {
        # Every redraw hit the guard. The base case is a committed finding whose
        # behavior under both engines is recorded, so it terminates; emitting it
        # unmutated is the one fallback that cannot introduce a hang.
        $lines = [@$base];
        $fellback++;
    }
    print "#=== $n\n";
    print "$_\n" for @$lines;
}

printf STDERR
  "mutate: %d case(s) from %d source case(s) in %d file(s), seed %s; "
  . "%d redraw(s) for the termination guard, %d fell back to the unmutated base\n",
  $count, scalar(@cases), scalar(@files), $seed, $redrawn, $fellback;
