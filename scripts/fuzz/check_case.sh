#!/usr/bin/env bash
# check_case.sh — run one case under both engines and print its bucket.
#
#   bash scripts/fuzz/check_case.sh CASE.tcl WORKDIR
#
# Prints one line: BUCKET<TAB>KEY<TAB>SIGNATURE (see scripts/fuzz/classify.pl),
# and leaves the captured behavior in WORKDIR:
#
#   driven.tcl  the case spliced into scripts/fuzz/drive.tcl — what both ran
#   tclsh.out / tclsh.err / tclsh.status
#   tclrs.out  / tclrs.err  / tclrs.status
#
# Both `scripts/fuzz_parity.sh` and the shrinker go through here, so a case is
# never classified two different ways by two different code paths.
#
# Environment:
#   TCLSH   reference interpreter (default: first tclsh on PATH)
#   TCLRS   subject binary (default: target/debug/tclrs)
#   TMO     per-process timeout in seconds (default 10)
set -uo pipefail

CASE="${1:?usage: check_case.sh CASE.tcl WORKDIR}"
WORK="${2:?usage: check_case.sh CASE.tcl WORKDIR}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

TCLSH="${TCLSH:-tclsh}"
TCLRS="${TCLRS:-$ROOT/target/debug/tclrs}"
TMO="${TMO:-10}"

mkdir -p "$WORK"

# Splice the case into the driver at its marker line. `perl -0777` so a case
# containing anything at all — braces, quotes, backslashes, newlines — is
# substituted as bytes and never re-interpreted as a pattern.
CASE_FILE="$CASE" perl -0777 -e '
  my ($drive) = @ARGV;
  open my $d, "<:raw", $drive or die "drive: $!";
  local $/; my $tpl = <$d>; close $d;
  open my $c, "<:raw", $ENV{CASE_FILE} or die "case: $!";
  my $body = <$c>; close $c;
  # Only the marker on a line of its own is the splice point; the one inside the
  # header comment must survive.
  $tpl =~ s/^\#<<CASE>>\n/$body/m or die "no case marker in $drive\n";
  print $tpl;
' "$ROOT/scripts/fuzz/drive.tcl" >"$WORK/driven.tcl" || exit 2

# Portable timeout: SIGALRM survives exec, so the alarm set here fires in the
# exec'd engine. GNU `timeout` is not on a stock macOS. A killed process reports
# 128 + 14 = 142, which is the status classify.pl reads as a hang.
run_to() { # run_to SECS CMD...
    perl -e 'alarm shift; exec @ARGV or die' "$@"
}

for engine in tclsh tclrs; do
    case "$engine" in
        tclsh) bin="$TCLSH" ;;
        tclrs) bin="$TCLRS" ;;
    esac
    run_to "$TMO" "$bin" "$WORK/driven.tcl" \
        >"$WORK/$engine.out" 2>"$WORK/$engine.err"
    echo $? >"$WORK/$engine.status"
done

# When tclrs failed, ask whether it failed while *compiling*: `--disasm` lowers
# the script and prints its bytecode without running it, so a failure there is
# one the script's shape decided rather than one a value decided. That answers
# the compile-time-versus-run-time question by measurement instead of by
# pattern-matching the message.
: >"$WORK/tclrs.disasm.err"
echo 0 >"$WORK/tclrs.disasm.status"
if [ "$(cat "$WORK/tclrs.status")" != "0" ]; then
    run_to "$TMO" "$TCLRS" --disasm "$WORK/driven.tcl" \
        >/dev/null 2>"$WORK/tclrs.disasm.err"
    echo $? >"$WORK/tclrs.disasm.status"
fi

perl "$ROOT/scripts/fuzz/classify.pl" \
    "$CASE" \
    "$WORK/tclsh.out" "$WORK/tclsh.err" "$(cat "$WORK/tclsh.status")" \
    "$WORK/tclrs.out" "$WORK/tclrs.err" "$(cat "$WORK/tclrs.status")" \
    "$WORK/tclrs.disasm.err" "$(cat "$WORK/tclrs.disasm.status")"
