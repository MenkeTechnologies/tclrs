#!/usr/bin/env perl
# classify.pl — put one fuzz case in exactly one bucket.
#
#   perl classify.pl CASE REF_OUT REF_ERR REF_STATUS SUB_OUT SUB_ERR SUB_STATUS \
#                    SUB_DISASM_ERR SUB_DISASM_STATUS
#
# Prints one line: BUCKET<TAB>KEY<TAB>SIGNATURE
#
#   PASS         both engines agree, whether they both succeeded or both failed
#                with the same message
#   SKIP         tclrs refused a construct it documents as unimplemented; KEY is
#                the refusal's own wording, so an over-broad pattern shows up as
#                a fat bucket in the report rather than as silence
#   ALLOWED      an enumerated known divergence; KEY is its id (see %ALLOW)
#   DIVERGENCE   anything else the two engines did differently; KEY is which
#                channel diverged (stdout / status / message / location)
#   CRITICAL     tclrs panicked, aborted, hung, or exited with a status its
#                driver cannot produce. Never suppressible by anything below.
#   EXCLUDED     tclsh crashed or hung, so there is no reference behavior to
#                compare against. Never charged against tclrs.
#
# The buckets are tested in that order, and CRITICAL is first on purpose: a
# tclrs crash is a finding even when tclsh also crashed on the same input.
#
# SIGNATURE is what the shrinker preserves: a normalised description of the
# divergence, stable under deleting an unrelated statement, so a reduction that
# happens to produce a *different* divergence is rejected.
#
# The exit status is 0 whatever the bucket; the caller reads the printed line.
# There is no flag, environment variable or file that changes any rule here —
# a measurement tool with a tuning knob measures the knob.

use strict;
use warnings;

my ($case, $ref_out, $ref_err, $ref_status, $sub_out, $sub_err, $sub_status,
    $dis_err, $dis_status) = @ARGV;
die "usage: classify.pl CASE REF_OUT REF_ERR REF_STATUS SUB_OUT SUB_ERR SUB_STATUS "
  . "SUB_DISASM_ERR SUB_DISASM_STATUS\n"
  unless defined $dis_status;

sub slurp {
    my ($path) = @_;
    open my $fh, "<:raw", $path or return "";
    local $/;
    my $s = <$fh>;
    close $fh;
    return defined $s ? $s : "";
}

my $src   = slurp($case);
my $rout  = slurp($ref_out);
my $rerr  = slurp($ref_err);
my $sout  = slurp($sub_out);
my $serr  = slurp($sub_err);

# The status the driver uses for "killed by the timeout" (SIGALRM, 128 + 14).
my $TIMEOUT = 142;

sub first_line {
    my ($s) = @_;
    my ($l) = split /\n/, $s, 2;
    return defined $l ? $l : "";
}

my $rmsg = first_line($rerr);
my $smsg = first_line($serr);

# Did tclrs fail while *compiling*? `tclrs --disasm` lowers the script and prints
# its bytecode without running it, so the same message from that run means the
# script's shape was refused rather than a value computed at run time. This is
# what distinguishes a `wrong # args` tclsh reports when a call is reached from
# one tclrs reports before anything runs — measured, not guessed from wording.
# `--disasm` reports through the driver's usage path, which prefixes `tclrs: `
# and keeps the ` (line N)` the message carries when the failure was located
# (`src/main.rs`, `TclError::fmt`). Both are stripped so the comparison is
# between the messages themselves.
my $dis_msg = first_line(slurp($dis_err));
$dis_msg =~ s/^tclrs: //;
$dis_msg =~ s/ \(line \d+\)$//;
my $compile_time = ($sub_status != 0 && $dis_status != 0 && $dis_msg eq $smsg);
my $when = $compile_time ? "-compile-time" : "";

# One line of output for the caller. `key` and `sig` are collapsed to one line
# each so the caller can read the record with a split, and the path of the file
# the engines actually ran is replaced by a fixed token: it names a scratch
# directory, so leaving it in would make a verdict recorded in one run fail to
# match the same behavior replayed from another.
sub verdict {
    my ($bucket, $key, $sig) = @_;
    for my $f (\$key, \$sig) {
        $$f = "" unless defined $$f;
        $$f =~ s/[\t\n\r]+/ /g;
        $$f =~ s/\(file "[^"]*" line (\d+)\)/(file line $1)/g;
        $$f =~ s/\s+/ /g;
    }
    print "$bucket\t$key\t$sig\n";
    exit 0;
}

# ── CRITICAL: tclrs did something no script can ask it to do ────────────────
#
# `src/main.rs` returns only `ExitCode::SUCCESS` or `ExitCode::FAILURE`, so any
# other status is the process dying rather than the driver reporting.
verdict("CRITICAL", "hang", "tclrs did not finish within the timeout")
  if $sub_status == $TIMEOUT;
verdict("CRITICAL", "panic", first_line($serr))
  if $serr =~ /panicked at|fatal runtime error|stack overflow|core dumped/;
verdict("CRITICAL", "status $sub_status", $smsg)
  if $sub_status != 0 && $sub_status != 1;

# ── EXCLUDED: no reference behavior exists ──────────────────────────────────
#
# tclsh exits 0 or 1 for a script that runs or fails; anything else is the
# reference interpreter itself dying. BUGS.md records one such input:
# `puts [lsearch -start -1 {} e1]` is a SIGSEGV in tclsh 9.0.4.
verdict("EXCLUDED", "tclsh hang", "tclsh did not finish within the timeout")
  if $ref_status == $TIMEOUT;
verdict("EXCLUDED", "tclsh status $ref_status", $rmsg)
  if $ref_status != 0 && $ref_status != 1;

# ── PASS: identical observable behavior ─────────────────────────────────────
#
# The contract is `tests/cli_differential.rs`'s: stdout in full, the exit
# status, and the first line of stderr — the message. tclsh follows the message
# with the stack of commands that raised it, which tclrs has no equivalent of,
# so only the first line is compared. tclrs may print one further line, the
# source location, and that line must appear verbatim in tclsh's report.
my @locs = grep { /^\s+\(file / } split /\n/, $serr;
my $loc_ok = 1;
for my $l (@locs) {
    $loc_ok = 0 unless index($rerr, $l) >= 0;
}

my $same_out    = $rout eq $sout;
my $same_status = $ref_status == $sub_status;
my $same_msg    = $rmsg eq $smsg;

verdict("PASS", ($ref_status == 0 ? "ran" : "failed alike"), "")
  if $same_out && $same_status && $same_msg && $loc_ok;

# ── SKIP: tclrs refused something it documents as unimplemented ─────────────
#
# Each pattern is one of the refusal wordings in the source, with where it is
# emitted. README [0x05] is the table these come from; nothing here matches a
# message tclsh produces, and a case where both engines printed the same
# message has already been called a PASS above.
my @REFUSALS = (
    [qr/ is not supported yet$/,                      "src/compiler.rs, src/assoc.rs, src/cmd_list.rs, src/cmd_string.rs"],
    [qr/ (?:is|are) not supported(?: yet)?[:.]?/,      "README [0x05]"],
    [qr/which (?:is|are) not built yet/,               "src/assoc.rs:803, src/cmd_string.rs:372"],
    [qr/^"[^"]+" is only supported at the top level/,  "src/compiler.rs — proc / coroutine placement"],
    [qr/only "info coroutine" is supported/,           "src/coro.rs:287"],
    [qr/^integer value too large to represent$/,       "BUGS.md — arbitrary-precision integers"],
    [qr/needs Unicode category tables/,                "src/cmd_string.rs:372, :1193"],
    [qr/needs regexp support/,                         "src/assoc.rs:803"],
);
if ($sub_status != 0) {
    for my $r (@REFUSALS) {
        my ($re, $where) = @$r;
        next unless $smsg =~ $re;
        verdict("SKIP", $smsg, $where);
    }
}

# ── ALLOWED: the enumerated known divergences ───────────────────────────────
#
# Every entry carries why it is allowed and where that is written down. The
# report prints a hit count per entry, so an entry that is broader than its
# reason claims shows up as an implausible count rather than as a clean run.
# Adding an entry to make a run come back clean is the one thing this file must
# never be used for.

# A1 — an unset variable reads as the empty string in tclrs (`src/assoc.rs:20`:
# no-such-variable, unset element and empty all collapse to `Undef`), where
# tclsh raises an error. Narrow on purpose: tclrs must have run the whole case
# without failing, so a case where tclrs *also* failed is still a divergence.
if (   $ref_status == 1
    && $sub_status == 0
    && $rmsg =~ /^can't read "[^"]*": no such variable$/)
{
    verdict("ALLOWED", "A1-unset-variable", $rmsg);
}

# A1b — the same deviation, seen from the other side: tclsh stopped at the unset
# read and tclrs carried on and failed somewhere further down. Everything tclsh
# printed, tclrs printed too — that is the `index(...) == 0` test — and past the
# point where tclsh stopped there is no reference behavior to compare with, so
# the rest of tclrs's run is unmeasured rather than measured-and-excused.
# Counted under its own key so widening A1 to cover it stays visible in the
# report instead of hiding inside A1's number.
if (   $ref_status == 1
    && $sub_status != 0
    && $rmsg =~ /^can't read "[^"]*": no such variable$/
    && index($sout, $rout) == 0)
{
    verdict("ALLOWED", "A1b-unset-variable-then-failed-later",
        "tclsh stopped at: $rmsg / tclrs went on and failed: $smsg");
}

# A1c — the same deviation seen through `catch`: both engines ran to the end, and
# the line where their output first parts is one where tclsh printed the
# no-such-variable message a `catch` had captured and tclrs printed something
# else, because its read produced the empty string and the `catch` caught
# nothing. Narrow: tclsh's line must literally carry that message. Whatever the
# two engines printed after that point followed from a value one of them never
# had, so it is unmeasured rather than excused.
if (   $ref_status == $sub_status
    && $same_msg
    && !$same_out)
{
    my @r = split /\n/, $rout;
    my @s = split /\n/, $sout;
    my $i = 0;
    $i++ while $i < @r && $i < @s && $r[$i] eq $s[$i];
    if (   $i < @r
        && $r[$i] =~ /can't read "[^"]*": no such variable/
        && (!defined $s[$i] || $s[$i] !~ /can't read "[^"]*": no such variable/))
    {
        verdict("ALLOWED", "A1c-unset-variable-caught", $r[$i]);
    }
}

# A2 — an unterminated brace is reported at the line where the input ran out,
# not where the brace opened, so tclrs's location line is not one tclsh
# printed. The message itself matches; only the location differs.
if (   $same_out
    && $same_status
    && $same_msg
    && !$loc_ok
    && $smsg =~ /^(?:missing close-brace|missing close-bracket|missing "|extra characters after close-brace)/)
{
    verdict("ALLOWED", "A2-brace-line-number", "$smsg / " . join(" ", @locs));
}

# A3 — `array names` and `array get` are sorted in tclrs and hash-ordered in
# tclsh. array(n) leaves the order unspecified, so both are conformant. The
# generator wraps both in `lsort` for exactly this reason, so this entry should
# only ever fire on a corpus that does not — a non-zero count here means the
# case came from somewhere else, not that the rule got broader.
if (   $ref_status == $sub_status
    && $same_msg
    && !$same_out
    && $src =~ /array (?:names|get)/)
{
    my @r = split /\n/, $rout;
    my @s = split /\n/, $sout;
    if (@r && @r == @s) {
        my ($permuted, $differing) = (1, 0);
        for my $i (0 .. $#r) {
            next if $r[$i] eq $s[$i];
            $differing++;
            my $a = join " ", sort(split ' ', $r[$i]);
            my $b = join " ", sort(split ' ', $s[$i]);
            $permuted = 0 unless $a eq $b;
        }
        # `$differing` must be non-zero: two outputs that differ only in how many
        # lines they have — one blank line against none — would otherwise walk
        # into this entry without a single line being compared.
        verdict("ALLOWED", "A3-array-order", "$differing line(s), same elements in another order")
          if $permuted && $differing;
    }
}

# A4 — tclrs resolves arity while compiling, so a call with the wrong number of
# arguments fails before the script runs; tclsh reports it when the call is
# reached. The message is the same, but tclsh has already printed whatever ran
# before the bad call, and a `catch` around it can see the failure where tclrs's
# cannot (README [0x05], BUGS.md). Narrow: tclrs's stdout must be a prefix of
# tclsh's — anything else is a divergence in what ran, not in when it was
# reported.
if (   $sub_status == 1
    && $smsg =~ /^wrong # args:/
    && $rmsg eq $smsg
    && length($sout) < length($rout)
    && index($rout, $sout) == 0)
{
    verdict("ALLOWED", "A4-compile-time-arity", $smsg);
}

# A5 — a tclrs error located while compiling carries a trailing ` (line N)` in
# its message when the message is read through the library rather than printed
# by the binary (`TclError::fmt`, src/runtime.rs:89). The binary prints the
# location on its own line instead, so this entry is unreachable through the
# file interface this harness uses; it is kept because the corpus can be
# re-checked through the library, and its count says which of the two paths a
# report came from.
if (   $same_out
    && $same_status
    && $smsg ne $rmsg
    && $smsg =~ /^\Q$rmsg\E \(line \d+\)$/)
{
    verdict("ALLOWED", "A5-trailing-line-number", $smsg);
}

# ── DIVERGENCE ──────────────────────────────────────────────────────────────
#
# The signature names the channel and the first place the two engines parted,
# normalised so it survives deleting an unrelated statement: that is what makes
# the shrinker keep reducing the *same* divergence instead of drifting to
# another one.
sub norm {
    my ($s) = @_;
    $s = "" unless defined $s;
    $s =~ s/\s+/ /g;
    $s =~ s/^ | $//g;
    return $s;
}

if (!$same_msg) {
    verdict("DIVERGENCE", "message$when",
        "msg tclsh=" . norm($rmsg) . " tclrs=" . norm($smsg));
}
if (!$same_out) {
    my @r = split /\n/, $rout;
    my @s = split /\n/, $sout;
    my $i = 0;
    $i++ while $i < @r && $i < @s && $r[$i] eq $s[$i];
    my $rl = $i < @r ? $r[$i] : "<no more output>";
    my $sl = $i < @s ? $s[$i] : "<no more output>";
    verdict("DIVERGENCE", "stdout$when",
        "line tclsh=" . norm($rl) . " tclrs=" . norm($sl));
}
if (!$same_status) {
    verdict("DIVERGENCE", "status$when",
        "status tclsh=$ref_status tclrs=$sub_status");
}
verdict("DIVERGENCE", "location$when",
    "tclrs located a failure where tclsh did not: " . join(" ", @locs));
