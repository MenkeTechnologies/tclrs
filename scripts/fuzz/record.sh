#!/usr/bin/env bash
# record.sh — write the `.expected` record for one corpus case, on stdout.
#
#   bash scripts/fuzz/record.sh CASE.tcl WORKDIR PROVENANCE >CASE.expected
#
# PROVENANCE is the free-text line that says where the case came from — the seed
# and case index the fuzzer found it at, or how it was re-recorded.
#
# Every field is one header line, and both engines' output is escaped onto that
# line (`\n`, `\t`, `\xNN`). The captured stdout of a case ends with the driver's
# record separator and no newline, so a record that pasted the bytes in raw would
# run the next field's header onto the end of the output and could not be read
# back unambiguously — which is exactly what `tests/parity_fuzz_corpus.rs` reads.
#
# One writer for this format, used by the fuzzer when it commits a finding and by
# `fuzz_parity.sh -r` when a corpus is re-recorded, so a record can never be
# written two slightly different ways.
set -uo pipefail

CASE="${1:?usage: record.sh CASE.tcl WORKDIR PROVENANCE}"
WORK="${2:?usage: record.sh CASE.tcl WORKDIR PROVENANCE}"
PROV="${3:-}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

TCLSH="${TCLSH:-tclsh}"
TCLRS="${TCLRS:-$ROOT/target/debug/tclrs}"

verdict=$(bash "$ROOT/scripts/fuzz/check_case.sh" "$CASE" "$WORK" 2>/dev/null)

# Escape a captured stream onto one line. `head -n N` first where the record only
# keeps part of it.
esc() {
    perl -0777 -e '
      my $s = <STDIN>;
      $s = "" unless defined $s;
      $s =~ s/\\/\\\\/g;
      $s =~ s/\n/\\n/g;
      $s =~ s/\t/\\t/g;
      $s =~ s/([\x00-\x1f\x7f])/sprintf("\\x%02x", ord $1)/ge;
      print $s;
    '
}

printf '# tclrs parity fuzz finding\n'
printf '# %s\n' "$PROV"
printf '# verdict: %s\n' "$verdict"
printf '# tclsh: %s\n' "$("$TCLSH" <<<'puts [info patchlevel]' 2>/dev/null | tr -d '\r')"
printf '# tclrs: %s\n' "$("$TCLRS" --version 2>/dev/null)"
printf '# tclsh-status: %s\n' "$(cat "$WORK/tclsh.status")"
printf '# tclsh-stdout: %s\n' "$(esc <"$WORK/tclsh.out")"
printf '# tclsh-stderr: %s\n' "$(head -1 "$WORK/tclsh.err" | esc)"
printf '# tclrs-status: %s\n' "$(cat "$WORK/tclrs.status")"
printf '# tclrs-stdout: %s\n' "$(esc <"$WORK/tclrs.out")"
# Every line, not a window. It was `head -3`, sized when a tclrs diagnostic was
# at most a message and its location; a refusal that carries tclsh's context is
# four lines now, so the window silently stopped recording the `(file … line N)`
# trailer for that whole class — the record went on comparing equal while
# verifying less. Any fixed window has that failure mode, so there is none.
printf '# tclrs-stderr: %s\n' "$(esc <"$WORK/tclrs.err")"
printf '#\n# The case itself is the sibling .tcl file. It is run spliced into\n'
printf '# scripts/fuzz/drive.tcl, which is what the captured output above is of.\n'
