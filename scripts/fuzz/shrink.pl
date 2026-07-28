#!/usr/bin/env perl
# shrink.pl — minimise a diverging case while the divergence survives.
#
#   perl shrink.pl CASE.tcl OUT.tcl WORKDIR BUDGET
#
# Reads the case's current verdict from `check_case.sh`, then reduces the case
# and keeps every reduction whose verdict is *byte-identical* — same bucket,
# same key, same signature. A reduction that turns the case into a different
# divergence, into a pass, or into a skip is rejected, so what comes out
# reproduces the finding that went in rather than some neighbour of it.
#
# Four passes, cheapest first, repeated until a whole round changes nothing:
#
#   1. delete statements — chunks of lines, halving the chunk size down to one
#      (delta debugging's granularity schedule). One statement per line is the
#      corpus contract, so a deleted line is a deleted statement and every
#      candidate stays brace-balanced.
#   2. simplify literals — each substitution in @SIMPLIFY replaces one class of
#      awkward value with a plain one, everywhere at once. What survives is the
#      part of the value that matters: if a case still diverges with `x` in
#      place of `naïve café`, the non-ASCII text was not the cause.
#   3. empty a body — replace the last brace group on a line with `{}`, which
#      collapses a loop or a branch without unbalancing anything.
#   4. drop a statement inside a body — delete one `;`-separated segment of a
#      line when what is left is still balanced.
#   5. unwrap a body — replace a statement with the contents of one of its brace
#      groups, so a loop or a branch that only surrounds the interesting
#      statement disappears in one step.
#
# BUDGET caps how many candidates are checked, because every candidate costs two
# engine processes. The cap and how much of it was used are printed to stderr, so
# a case that stopped shrinking because it ran out of budget says so rather than
# looking minimal.

use strict;
use warnings;

my ($case, $out, $work, $budget) = @ARGV;
die "usage: shrink.pl CASE.tcl OUT.tcl WORKDIR BUDGET\n" unless defined $budget;
$budget = int($budget);

my $root = $0;
$root =~ s{/scripts/fuzz/shrink\.pl$}{};
$root = "." if $root eq $0;

my $spent = 0;

sub write_case {
    my ($path, $text) = @_;
    open my $fh, ">:raw", $path or die "write $path: $!";
    print $fh $text;
    close $fh;
}

sub verdict_of {
    my ($text) = @_;
    my $tmp = "$work/candidate.tcl";
    write_case($tmp, $text);
    $spent++;
    my $line = `bash $root/scripts/fuzz/check_case.sh $tmp $work/shrink 2>/dev/null`;
    $line = "" unless defined $line;
    chomp $line;
    return $line;
}

open my $fh, "<:raw", $case or die "read $case: $!";
local $/;
my $text = <$fh>;
close $fh;

my $target = verdict_of($text);
die "shrink: the case does not reproduce any verdict\n" unless length $target;

# A candidate is only accepted when the whole verdict line matches.
sub keeps {
    my ($cand) = @_;
    return 0 if $spent >= $budget;
    return 0 unless length $cand;
    return verdict_of($cand) eq $target;
}

sub lines_of { return [split /\n/, $_[0], -1] }
sub text_of {
    my ($l) = @_;
    my @keep = @$l;
    pop @keep while @keep && $keep[-1] eq "";
    return join("\n", @keep) . "\n";
}

# ── pass 1: delete statements ───────────────────────────────────────────────
sub pass_delete {
    my ($t) = @_;
    my $l = lines_of($t);
    my $n = scalar @$l;
    my $chunk = int($n / 2) || 1;
    while ($chunk >= 1) {
        my $i = 0;
        while ($i < scalar @$l) {
            my @cand = @$l;
            splice @cand, $i, $chunk;
            my $ct = text_of(\@cand);
            if (@cand && keeps($ct)) {
                $l = \@cand;
                next;    # same index: the next statement moved into this slot
            }
            $i += $chunk;
            last if $spent >= $budget;
        }
        last if $spent >= $budget;
        $chunk = int($chunk / 2);
    }
    return text_of($l);
}

# ── pass 2: simplify literals ───────────────────────────────────────────────
#
# Ordered from the most to the least aggressive, and each is tried on its own so
# the report can say which class of value the divergence actually needs.
my @SIMPLIFY = (
    [qr/"[^"\n]*\\[^"\n]*"/,            '"x"'],       # a quoted word with escapes
    [qr/\{[^{}\n]{4,}\}/,               '{x}'],       # a long braced literal
    [qr/héllo|日本語|αβγ|ÜñîçøðÉ|naïve café/, 'x'],   # non-ASCII text
    [qr/-?9{4,}\d*|-?\d{10,}/,          '1'],         # huge integers
    [qr/-?\d+\.\d+e[-+]?\d+|-?\d+e[-+]?\d+/, '1.5'],  # exponent-form floats
    [qr/(?<![\w.])0+(?=\d)/,            ''],          # leading zeros
    [qr/0[xob]\w+/,                     '7'],         # radix prefixes
    [qr/(?<=\d)_(?=\d)/,                ''],          # digit separators
    [qr/  +/,                           ' '],         # runs of spaces
);

sub pass_simplify {
    my ($t) = @_;
    for my $s (@SIMPLIFY) {
        my ($re, $to) = @$s;
        my $cand = $t;
        my $hits = ($cand =~ s/$re/$to/g);
        next unless $hits;
        $t = $cand if keeps($cand);
        last if $spent >= $budget;
    }
    return $t;
}

# ── pass 3: empty a body ────────────────────────────────────────────────────
sub pass_bodies {
    my ($t) = @_;
    my $l = lines_of($t);
    for my $i (0 .. $#$l) {
        next unless $l->[$i] =~ /\{.*\}/;
        my @cand = @$l;
        # The last balanced group on the line, which is the body of whatever
        # command the line is.
        next unless $cand[$i] =~ s/\{[^{}]*\}(?!.*\{)/{}/;
        next if $cand[$i] eq $l->[$i];
        my $ct = text_of(\@cand);
        $l = \@cand if keeps($ct);
        last if $spent >= $budget;
    }
    return text_of($l);
}

# ── pass 4: drop a statement inside a body ──────────────────────────────────
sub balanced {
    my ($s) = @_;
    my ($b, $k) = (0, 0);
    for my $c (split //, $s) {
        $b++ if $c eq "{";
        $b-- if $c eq "}";
        $k++ if $c eq "[";
        $k-- if $c eq "]";
        return 0 if $b < 0 || $k < 0;
    }
    return $b == 0 && $k == 0;
}

sub pass_segments {
    my ($t) = @_;
    my $l = lines_of($t);
    for my $i (0 .. $#$l) {
        next unless $l->[$i] =~ /; /;
        my @segs = split /; /, $l->[$i];
        my $j = 0;
        while ($j < scalar @segs) {
            my @rest = @segs;
            splice @rest, $j, 1;
            my $line = join "; ", @rest;
            if (@rest && balanced($line)) {
                my @cand = @$l;
                $cand[$i] = $line;
                if (keeps(text_of(\@cand))) {
                    $l = \@cand;
                    @segs = @rest;
                    next;
                }
            }
            $j++;
            last if $spent >= $budget;
        }
        last if $spent >= $budget;
    }
    return text_of($l);
}

# ── pass 5: unwrap a body ───────────────────────────────────────────────────
#
# Replace a whole statement with the contents of one of its brace groups, which
# is what turns `for {...} {...} {...} {puts [expr {"a" - 1}]}` into the one
# statement that matters. Tried largest group first, so a loop or a branch
# disappears in one step rather than being whittled down.
sub groups_of {
    my ($s) = @_;
    my @groups;
    my ($depth, $start) = (0, 0);
    my @ch = split //, $s;
    for my $i (0 .. $#ch) {
        if ($ch[$i] eq "{") {
            $start = $i if $depth == 0;
            $depth++;
        }
        elsif ($ch[$i] eq "}") {
            $depth--;
            push @groups, [$start, $i] if $depth == 0;
        }
    }
    return @groups;
}

sub pass_unwrap {
    my ($t) = @_;
    my $l = lines_of($t);
    for my $i (0 .. $#$l) {
        my @g = groups_of($l->[$i]);
        next unless @g;
        for my $g (sort { ($b->[1] - $b->[0]) <=> ($a->[1] - $a->[0]) } @g) {
            my $inner = substr($l->[$i], $g->[0] + 1, $g->[1] - $g->[0] - 1);
            next unless length $inner && balanced($inner);
            next if $inner eq $l->[$i];
            my @cand = @$l;
            $cand[$i] = $inner;
            if (keeps(text_of(\@cand))) {
                $l = \@cand;
                last;
            }
            last if $spent >= $budget;
        }
        last if $spent >= $budget;
    }
    return text_of($l);
}

# ── drive the passes to a fixpoint ──────────────────────────────────────────
my $before = $text;
my $rounds = 0;
while (1) {
    my $start = $text;
    $text = pass_delete($text);
    $text = pass_unwrap($text);
    $text = pass_segments($text);
    $text = pass_bodies($text);
    $text = pass_simplify($text);
    $rounds++;
    last if $text eq $start || $spent >= $budget;
}

# The minimised case must still reproduce the finding: check the final text
# rather than trusting the passes.
my $final = verdict_of($text);
if ($final ne $target) {
    $text = $before;
    warn "shrink: reduction lost the finding, keeping the original case\n";
}
write_case($out, $text);

my $lines_before = scalar @{ lines_of($before) };
my $lines_after  = scalar @{ lines_of($text) };
warn sprintf(
    "shrink: %d -> %d lines in %d round(s), %d/%d candidates checked%s\n",
    $lines_before, $lines_after, $rounds, $spent, $budget,
    $spent >= $budget ? " (BUDGET EXHAUSTED — the case may not be minimal)" : ""
);
print "$target\n";
