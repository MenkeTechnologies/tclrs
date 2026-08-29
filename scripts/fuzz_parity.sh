#!/usr/bin/env bash
# fuzz_parity.sh — differential fuzzer: tclrs vs the reference tclsh.
#
# Generates a seeded corpus of random Tcl programs (scripts/fuzz/gen.tcl), runs
# every one under BOTH `tclsh` (ground truth) and `tclrs` (subject) through the
# same driver (scripts/fuzz/drive.tcl), and reports every case whose stdout,
# exit status or error message differs. Those are parity gaps; what they are and
# which are known is recorded in BUGS.md.
#
#   bash scripts/fuzz_parity.sh                        # 200 cases, seed 1
#   bash scripts/fuzz_parity.sh -n 2000 -s 42          # bigger corpus, new seed
#   bash scripts/fuzz_parity.sh -n 500 -m              # minimise what it finds
#   bash scripts/fuzz_parity.sh -c target/fuzz/corpus.tcl   # re-check a corpus
#   bash scripts/fuzz_parity.sh -M -n 500 -m           # mutate the committed corpus
#   bash scripts/fuzz_parity.sh -M -c other/corpus     # ... plus another source
#   bash scripts/fuzz_parity.sh -R tests/fuzz_corpus   # replay committed findings
#
#   -n N       corpus size (default 200); long form --iterations N
#   -s SEED    PRNG seed (default 1); same seed => same corpus, so a divergence
#              reproduces exactly on any machine. Long form --seed SEED
#   -d DEPTH   max statement nesting (default 3); generation only
#   -c FILE    use an existing corpus file instead of generating one. With -M it
#              adds a source — a file or a directory — to mutate instead
#   -M         mutation mode: build the corpus by mutating existing cases rather
#              than generating fresh ones (scripts/fuzz/mutate.pl). The sources
#              are tests/fuzz_corpus — every minimised finding — plus whatever
#              -c names. Seeded and reproducible the same way generation is, and
#              the corpus it writes goes through the same split, the same
#              classifier and the same shrinker, so no case is classified two
#              ways. Long form --mutate
#   -t SECS    per-process timeout (default 10); a case that outlasts it is a
#              classified hang, not a wedged run. Long form --timeout
#   -T SECS    stop starting new cases after SECS of wall clock; the report says
#              how many of N were reached. Long form --time-budget
#   -m         minimise every divergence and write it to the corpus directory.
#              Long form --shrink
#   -b N       shrink budget in candidate checks per case (default 400)
#   -C DIR     corpus directory for minimised findings (default tests/fuzz_corpus).
#              Long form --corpus DIR
#   -R DIR     replay a corpus directory instead of fuzzing: every committed
#              case is re-run and compared against its recorded observations
#   -r DIR     re-record a corpus directory: every case is re-run and its
#              `.expected` rewritten. For use after a fix, once the change in
#              behavior has been read in a `-R` report — never to make `-R` quiet
#   -q         summary only
#   -x         self-check: run the generator under both engines and compare the
#              two corpora byte for byte before fuzzing
#   -h         this message
#
# Artifacts land in target/fuzz/ (gitignored): corpus.tcl, cases/, the captured
# behavior of both engines per case, and diverge.txt (case, both results, and
# the minimised reproducer). The summary counts every bucket and prints a hit
# count for every allowlist entry — an over-broad suppression shows up as an
# implausible count rather than as a clean run.
#
# Exit status is the number of cases that diverged and were NOT allowlisted,
# plus the critical ones (0 = parity), capped at 250 — the same contract as
# elisprs/scripts/fuzz_parity.sh, so CI consumes both the same way. Skips,
# allowlisted divergences and tclsh's own crashes do not enter the status, and
# all three are printed.
set -uo pipefail
cd "$(dirname "$0")/.."

N=200 SEED=1 DEPTH=3 CORPUS= TMO=10 QUIET=0 SHRINK=0 BUDGET=400
CORPUS_DIR=tests/fuzz_corpus REPLAY= RERECORD= TIME_BUDGET=0 SELFCHECK=0
MUTATE=0
TCLSH="${TCLSH:-}"
TCLRS="${TCLRS:-}"

while [ $# -gt 0 ]; do
  case "$1" in
    -n|--iterations)  N="$2"; shift 2 ;;
    -s|--seed)        SEED="$2"; shift 2 ;;
    -d|--depth)       DEPTH="$2"; shift 2 ;;
    -c)               CORPUS="$2"; shift 2 ;;
    -t|--timeout)     TMO="$2"; shift 2 ;;
    -T|--time-budget) TIME_BUDGET="$2"; shift 2 ;;
    -M|--mutate)      MUTATE=1; shift ;;
    -m|--shrink)      SHRINK=1; shift ;;
    -b|--budget)      BUDGET="$2"; shift 2 ;;
    -C|--corpus)      CORPUS_DIR="$2"; shift 2 ;;
    -R|--replay)      REPLAY="$2"; shift 2 ;;
    -r|--rerecord)    RERECORD="$2"; shift 2 ;;
    -q)               QUIET=1; shift ;;
    -x|--self-check)  SELFCHECK=1; shift ;;
    -h|--help)        grep '^#' "$0" | cut -c3- ; exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done

if [ -t 1 ]; then C='\033[36m'; G='\033[32m'; R='\033[31m'; Y='\033[33m'; D='\033[2m'; N_='\033[0m'
else C= G= R= Y= D= N_=; fi
say() { printf "${C}==>${N_} %s\n" "$1"; }

if [ -z "$TCLSH" ]; then
  for name in tclsh tclsh9.0 tclsh8.6; do
    if command -v "$name" >/dev/null 2>&1; then TCLSH="$(command -v "$name")"; break; fi
  done
fi
[ -n "$TCLSH" ] || { echo "no tclsh on PATH — the fuzzer needs the reference interpreter as ground truth" >&2; exit 2; }
if [ -z "$TCLRS" ]; then
  if   [ -x target/debug/tclrs ];   then TCLRS=target/debug/tclrs
  elif [ -x target/release/tclrs ]; then TCLRS=target/release/tclrs
  else echo "no tclrs binary — run \`cargo build' first" >&2; exit 2; fi
fi
export TCLSH TCLRS TMO

OUT=target/fuzz
rm -rf "$OUT/cases" "$OUT/work"
mkdir -p "$OUT" "$OUT/cases" "$OUT/work"

# Every bucket is a counter, and every counter is printed. There is no bucket
# that is silently dropped: a case lands in exactly one of these.
PASS=0 SKIP=0 ALLOWED=0 DIVERGE=0 CRITICAL=0 EXCLUDED=0 CHECKED=0
: >"$OUT/diverge.txt"
: >"$OUT/skips.txt"
: >"$OUT/allowed.txt"
: >"$OUT/critical.txt"
: >"$OUT/excluded.txt"
: >"$OUT/excluded_detail.txt"

# ── the reference interpreter's version, for the record ─────────────────────
REF_VERSION=$("$TCLSH" <<<'puts [info patchlevel]' 2>/dev/null | tr -d '\r')
SUB_VERSION=$("$TCLRS" --version 2>/dev/null)

# ── generator self-check ────────────────────────────────────────────────────
if [ "$SELFCHECK" -eq 1 ]; then
  say "self-check: generating the same corpus under both engines"
  "$TCLSH" scripts/fuzz/gen.tcl "$SEED" 50 "$DEPTH" >"$OUT/self.tclsh" 2>&1
  "$TCLRS" scripts/fuzz/gen.tcl "$SEED" 50 "$DEPTH" >"$OUT/self.tclrs" 2>&1
  if cmp -s "$OUT/self.tclsh" "$OUT/self.tclrs"; then
    printf "    ${G}the generator itself is a parity case: %s lines, identical${N_}\n" \
      "$(grep -c '' "$OUT/self.tclsh")"
  else
    printf "    ${R}the generator diverges between the two engines${N_} (%s)\n" "$OUT/self.tclsh"
    diff "$OUT/self.tclsh" "$OUT/self.tclrs" | head -20
  fi
fi

# ── re-record mode ──────────────────────────────────────────────────────────
#
# Rewrite the record of every committed case from a fresh run of both engines.
# What changed is printed as it goes: re-recording is how a fix is accepted into
# the corpus, and an unexplained change in that list means something else moved.
if [ -n "$RERECORD" ]; then
  [ -d "$RERECORD" ] || { echo "no corpus directory $RERECORD" >&2; exit 2; }
  say "re-recording $(find "$RERECORD" -name '*.tcl' | wc -l | tr -d ' ') case(s) in $RERECORD"
  rewritten=0
  for case in "$RERECORD"/*.tcl; do
    [ -e "$case" ] || continue
    rec="${case%.tcl}.expected"
    was=$(grep '^# verdict: ' "$rec" 2>/dev/null | head -1 | cut -c12-)
    prov=$(grep -m1 '^# seed: ' "$rec" 2>/dev/null | cut -c3-)
    [ -n "$prov" ] || prov="re-recorded, provenance unknown"
    bash scripts/fuzz/record.sh "$case" "$OUT/work/record" "$prov" >"$rec.new" || exit 2
    now=$(grep '^# verdict: ' "$rec.new" | head -1 | cut -c12-)
    mv "$rec.new" "$rec"
    if [ "$was" != "$now" ]; then
      rewritten=$((rewritten + 1))
      printf "${Y}%s${N_}\n  was: %s\n  now: %s\n" "$case" "$was" "$now"
    fi
  done
  echo
  printf "re-recorded every case; %d verdict(s) changed.\n" "$rewritten"
  exit 0
fi

# ── replay mode ─────────────────────────────────────────────────────────────
#
# Re-run every committed finding and compare it with what was recorded when it
# was found. A finding that no longer reproduces is not silently dropped: it is
# reported, because either the engine changed or the record is stale.
if [ -n "$REPLAY" ]; then
  [ -d "$REPLAY" ] || { echo "no corpus directory $REPLAY" >&2; exit 2; }
  say "replaying $(find "$REPLAY" -name '*.tcl' | wc -l | tr -d ' ') committed case(s) from $REPLAY"
  changed=0 replayed=0
  for case in "$REPLAY"/*.tcl; do
    [ -e "$case" ] || continue
    replayed=$((replayed + 1))
    want=$(grep '^# verdict: ' "${case%.tcl}.expected" 2>/dev/null | head -1 | cut -c12-)
    got=$(bash scripts/fuzz/check_case.sh "$case" "$OUT/work/replay" 2>/dev/null)
    if [ "$got" != "$want" ]; then
      changed=$((changed + 1))
      printf "${Y}%s${N_}\n  recorded: %s\n  now:      %s\n" "$case" "$want" "$got"
    fi
  done
  echo
  if [ "$changed" -eq 0 ]; then
    printf "${G}CORPUS: %d/%d committed cases behave exactly as recorded.${N_}\n" "$replayed" "$replayed"
    exit 0
  fi
  printf "${R}%d/%d committed cases no longer match their record${N_}\n" "$changed" "$replayed"
  [ "$changed" -gt 250 ] && changed=250
  exit "$changed"
fi

# ── corpus ──────────────────────────────────────────────────────────────────
#
# Three ways to get one, and all three end at the same file: the split below,
# `check_case.sh`, `classify.pl` and `shrink.pl` never learn which it was.
MUTATE_SOURCES=
if [ "$MUTATE" -eq 1 ]; then
  # The committed corpus, not `$CORPUS_DIR`: -C says where *findings are
  # written*, and pointing that at a scratch directory must not silently change
  # what is being mutated. Another source is added with -c.
  MUTATE_SOURCES=tests/fuzz_corpus
  [ -n "$CORPUS" ] && MUTATE_SOURCES="$MUTATE_SOURCES $CORPUS"
  say "mutating $N cases (seed $SEED) from: $MUTATE_SOURCES"
  # Written to a temporary and moved, because a source given with -c may *be*
  # `$OUT/corpus.tcl` — re-mutating the corpus of the previous run is a
  # reasonable thing to ask for, and a direct redirection would truncate it
  # before the mutator ever opened it.
  # shellcheck disable=SC2086
  perl scripts/fuzz/mutate.pl "$SEED" "$N" $MUTATE_SOURCES >"$OUT/corpus.new" \
    2>"$OUT/mutate.log" || { cat "$OUT/mutate.log" >&2; exit 2; }
  mv "$OUT/corpus.new" "$OUT/corpus.tcl"
  perl -pe 's/^/    /' "$OUT/mutate.log"
elif [ -n "$CORPUS" ]; then
  cp "$CORPUS" "$OUT/corpus.tcl"
  say "using corpus $CORPUS"
else
  say "generating $N cases (seed $SEED, depth $DEPTH)"
  "$TCLSH" scripts/fuzz/gen.tcl "$SEED" "$N" "$DEPTH" >"$OUT/corpus.tcl" || exit 2
fi

# Split the corpus into one file per case. `#=== INDEX` introduces a case; the
# generator never emits that text anywhere else.
perl -e '
  my ($corpus, $dir) = @ARGV;
  open my $c, "<:raw", $corpus or die "corpus: $!";
  my ($fh, $n) = (undef, 0);
  while (my $line = <$c>) {
    if ($line =~ /^\#=== (\d+)$/) {
      close $fh if $fh;
      open $fh, ">:raw", sprintf("%s/%05d.tcl", $dir, $1) or die "case: $!";
      $n++;
      next;
    }
    print {$fh} $line if $fh;
  }
  close $fh if $fh;
  print "$n\n";
' "$OUT/corpus.tcl" "$OUT/cases" >"$OUT/count" || exit 2
TOTAL=$(cat "$OUT/count")
[ "$TOTAL" -gt 0 ] || { echo "empty corpus" >&2; exit 2; }

mkdir -p "$CORPUS_DIR"

# How many minimised cases with the *same* verdict reach the committed corpus.
# Everything the fuzzer finds is in target/fuzz/diverge.txt whatever this is set
# to; the cap only stops one divergence class from filling the regression corpus
# with a hundred cases that pin the same behavior. The report prints the cap and
# how many cases it turned away.
CORPUS_CAP=3
CAPPED=0
: >"$OUT/corpus_signatures.txt"

# ── check every case ────────────────────────────────────────────────────────
say "checking $TOTAL cases against $REF_VERSION (tclsh) with $SUB_VERSION"
STARTED=$(date +%s)
STOPPED_EARLY=0

for case in "$OUT/cases"/*.tcl; do
  if [ "$TIME_BUDGET" -gt 0 ] && [ $(( $(date +%s) - STARTED )) -ge "$TIME_BUDGET" ]; then
    STOPPED_EARLY=1
    break
  fi
  idx=$(basename "$case" .tcl)
  work="$OUT/work/$idx"
  line=$(bash scripts/fuzz/check_case.sh "$case" "$work" 2>/dev/null)
  CHECKED=$((CHECKED + 1))
  bucket=${line%%	*}
  rest=${line#*	}
  key=${rest%%	*}
  sig=${rest#*	}

  record() { # record FILE
    {
      printf '#%s\t%s\t%s\n' "$idx" "$key" "$sig"
      printf -- '--- case ---\n'
      cat "$case"
      printf -- '--- tclsh (status %s) ---\n' "$(cat "$work/tclsh.status")"
      cat "$work/tclsh.out"
      head -1 "$work/tclsh.err"
      printf -- '--- tclrs (status %s) ---\n' "$(cat "$work/tclrs.status")"
      cat "$work/tclrs.out"
      head -3 "$work/tclrs.err"
      printf '\n'
    } >>"$1"
  }

  case "$bucket" in
    PASS)     PASS=$((PASS + 1)) ;;
    SKIP)     SKIP=$((SKIP + 1)); printf '%s\t%s\n' "$key" "$idx" >>"$OUT/skips.txt" ;;
    ALLOWED)  ALLOWED=$((ALLOWED + 1)); printf '%s\t%s\n' "$key" "$idx" >>"$OUT/allowed.txt" ;;
    EXCLUDED) EXCLUDED=$((EXCLUDED + 1)); printf '%s\t%s\n' "$key" "$idx" >>"$OUT/excluded.txt"; record "$OUT/excluded_detail.txt" ;;
    CRITICAL) CRITICAL=$((CRITICAL + 1)); record "$OUT/critical.txt"; record "$OUT/diverge.txt" ;;
    DIVERGENCE) DIVERGE=$((DIVERGE + 1)); record "$OUT/diverge.txt" ;;
    *) echo "classifier returned no bucket for case $idx: $line" >&2; exit 2 ;;
  esac

  # Minimise and commit anything that counts against the subject. The verdict a
  # case already has is what the shrinker preserves, so the cap is applied before
  # shrinking rather than after: a case whose verdict the corpus already pins
  # three times is not worth two hundred more engine runs to reduce.
  if [ "$SHRINK" -eq 1 ] && { [ "$bucket" = "DIVERGENCE" ] || [ "$bucket" = "CRITICAL" ]; }; then
    seen=$(grep -Fxc "$bucket	$key	$sig" "$OUT/corpus_signatures.txt" 2>/dev/null || true)
    printf '%s\t%s\t%s\n' "$bucket" "$key" "$sig" >>"$OUT/corpus_signatures.txt"
    min="$OUT/work/$idx.min.tcl"
    if [ "${seen:-0}" -ge "$CORPUS_CAP" ]; then
      CAPPED=$((CAPPED + 1))
      min=
    else
      mkdir -p "$OUT/work/shrink"
      perl scripts/fuzz/shrink.pl "$case" "$min" "$OUT/work" "$BUDGET" >/dev/null 2>"$OUT/work/$idx.shrink.log"
    fi
    if [ -n "$min" ] && [ -s "$min" ]; then
      hash=$(perl -MDigest::MD5 -e 'print substr(Digest::MD5::md5_hex(do { local $/; open my $f, "<:raw", $ARGV[0]; <$f> }), 0, 8)' "$min")
      slug=$(printf '%s' "$key" | tr -cs 'A-Za-z0-9' '-' | tr 'A-Z' 'a-z' | cut -c1-24 | sed 's/-*$//')
      name="$CORPUS_DIR/${slug}-${hash}"
      cp "$min" "$name.tcl"
      # The record: everything needed to re-check the case without the fuzzer.
      # One writer for the format (scripts/fuzz/record.sh), so the fuzzer and the
      # re-record mode cannot drift apart.
      bash scripts/fuzz/record.sh "$name.tcl" "$OUT/work/record" \
        "seed: $SEED   case: $idx   depth: $DEPTH   generator: scripts/fuzz/gen.tcl   $(tr -d '\n' <"$OUT/work/$idx.shrink.log")" \
        >"$name.expected"
      printf -- '--- minimised: %s ---\n' "$name.tcl" >>"$OUT/diverge.txt"
      cat "$name.tcl" >>"$OUT/diverge.txt"
      printf '\n' >>"$OUT/diverge.txt"
    fi
  fi
done

# ── report ──────────────────────────────────────────────────────────────────
BAD=$((DIVERGE + CRITICAL))
ELAPSED=$(( $(date +%s) - STARTED ))

echo
say "buckets ($CHECKED case(s) checked in ${ELAPSED}s)"
printf '  %-26s %6d\n' "PASS (identical)"            "$PASS"
printf '  %-26s %6d\n' "SKIP (documented refusal)"   "$SKIP"
printf '  %-26s %6d\n' "ALLOWED (known divergence)"  "$ALLOWED"
printf '  %-26s %6d\n' "DIVERGENCE"                  "$DIVERGE"
printf '  %-26s %6d\n' "CRITICAL (tclrs died/hung)"  "$CRITICAL"
printf '  %-26s %6d\n' "EXCLUDED (tclsh died/hung)"  "$EXCLUDED"
printf '  %-26s %6d\n' "total"                       "$((PASS + SKIP + ALLOWED + DIVERGE + CRITICAL + EXCLUDED))"

echo
say "allowlist (every entry, with its hit count)"
# The table is the allowlist. Every entry `scripts/fuzz/classify.pl` can produce
# is a row here, with the reason it is allowed and where that reason is written
# down — and the rows are summed against the bucket, so an entry that exists in
# the classifier but not in this table cannot hide inside the total.
ALLOW_KEYS="A1c-unset-variable-caught \
A3-array-order A4-compile-time-arity A5-trailing-line-number"
allow_reason() {
  case "$1" in
    A1c-unset-variable-caught)
                        echo 'a procedure-local unset read: a frame slot has no name to report — BUGS.md' ;;
    A3-array-order)     echo 'array names/get sorted vs hashed — array(n) leaves the order unspecified' ;;
    A4-compile-time-arity)
                        echo 'arity refused before anything ran — README [0x05], BUGS.md' ;;
    A5-trailing-line-number)
                        echo 'message carries " (line N)" through the library — src/runtime.rs' ;;
    *)                  echo 'NO DOCUMENTED REASON — this entry must not exist' ;;
  esac
}
listed=0
for k in $ALLOW_KEYS; do
  c=$(grep -c "^$k	" "$OUT/allowed.txt" 2>/dev/null || true)
  c=${c:-0}
  listed=$((listed + c))
  printf '  %-38s %5d  %s\n' "$k" "$c" "$(allow_reason "$k")"
done
if [ "$listed" -ne "$ALLOWED" ]; then
  printf "${R}  %d allowlisted case(s) are not accounted for by the rows above:${N_}\n" \
    "$((ALLOWED - listed))"
  cut -f1 "$OUT/allowed.txt" | LC_ALL=C sort -u | while read -r k; do
    case " $ALLOW_KEYS " in *" $k "*) ;; *) printf '    %s\n' "$k" ;; esac
  done
fi

if [ "$SKIP" -gt 0 ]; then
  echo
  say "skips by refusal (what tclrs declined to run)"
  cut -f1 "$OUT/skips.txt" | LC_ALL=C sort | LC_ALL=C uniq -c | LC_ALL=C sort -rn | head -25
fi

if [ "$EXCLUDED" -gt 0 ]; then
  echo
  say "excluded — tclsh itself crashed or hung, never charged against tclrs"
  cut -f1 "$OUT/excluded.txt" | LC_ALL=C sort | LC_ALL=C uniq -c | LC_ALL=C sort -rn | head -10
  printf '  %s\n' "the cases themselves: $OUT/excluded_detail.txt"
fi

echo
say "what was bounded in this run"
if [ "$MUTATE" -eq 1 ]; then
  printf '  corpus built by            mutating %s (scripts/fuzz/mutate.pl)\n' "$MUTATE_SOURCES"
  printf '  cases requested            %s\n' "$N"
elif [ -n "$CORPUS" ]; then
  printf '  corpus read from           %s (the -n default of %s did not apply)\n' "$CORPUS" "$N"
else
  printf '  cases requested            %s\n' "$N"
fi
printf '  cases checked              %s%s\n' "$CHECKED" \
  "$([ "$STOPPED_EARLY" -eq 1 ] && printf ' (stopped by the %ss time budget)' "$TIME_BUDGET")"
if [ -z "$CORPUS" ] || [ "$MUTATE" -eq 1 ]; then
  printf '  seed                       %s\n' "$SEED"
fi
if [ "$MUTATE" -eq 1 ]; then
  printf '  max statement nesting      n/a (mutation does not nest; -d %s ignored)\n' "$DEPTH"
else
  printf '  max statement nesting      %s\n' "$DEPTH"
fi
printf '  per-process timeout        %ss (both engines)\n' "$TMO"
printf '  shrinking                  %s\n' \
  "$([ "$SHRINK" -eq 1 ] && printf 'on, %s candidate checks per case' "$BUDGET" || printf 'off (-m to enable)')"
printf '  loop trip counts           at most 5, structural (scripts/fuzz/gen.tcl)\n'
printf '  format width / precision   at most 2 digits; the two unbounded ones are\n'
printf '                             already-recorded aborts (BUGS.md)\n'
if [ "$MUTATE" -eq 1 ]; then
  printf '  loops in a mutant          verbatim from a source case, checked per mutant\n'
fi
printf '  corpus directory           %s\n' "$CORPUS_DIR"
printf '  cases per verdict in it    at most %s%s\n' "$CORPUS_CAP" \
  "$([ "${CAPPED:-0}" -gt 0 ] && printf ' (%s further case(s) not written there; all of them are in %s/diverge.txt)' "$CAPPED" "$OUT")"
printf '  artifacts                  %s\n' "$OUT"

echo
if [ "$CRITICAL" -gt 0 ]; then
  printf "${R}CRITICAL: %d case(s) made tclrs die or hang${N_}  ${D}(%s)${N_}\n" \
    "$CRITICAL" "$OUT/critical.txt"
fi
if [ "$BAD" -eq 0 ]; then
  printf "${G}PARITY: %d/%d compared cases agree with tclsh.${N_}\n" "$PASS" "$((PASS + DIVERGE + CRITICAL))"
  exit 0
fi

printf "${R}%d/%d compared cases diverge from tclsh${N_}  ${D}(%s)${N_}\n" \
  "$BAD" "$((PASS + DIVERGE + CRITICAL))" "$OUT/diverge.txt"
echo
say "divergences by signature (channel, then what the two engines said)"
# LC_ALL=C, and on every sort over fuzz output above: a generated case may hold
# bytes that are not valid UTF-8 in the ambient locale, and GNU/BSD `sort` then
# fails the whole pipeline with "sort: Illegal byte sequence" and writes a
# partial histogram. The ranking is what the report is read for, so it must
# count every line; byte order is a fine order for a frequency table.
perl -ne '
  next unless /^\#\d+\t([^\t]+)\t(.*)$/;
  my ($key, $sig) = ($1, $2);
  $sig = substr($sig, 0, 110) . "..." if length $sig > 113;
  print "$key  $sig\n";
' "$OUT/diverge.txt" | LC_ALL=C sort | LC_ALL=C uniq -c | LC_ALL=C sort -rn | head -30
if [ "$QUIET" -eq 0 ]; then
  echo
  say "first divergences"
  head -60 "$OUT/diverge.txt"
fi
[ "$BAD" -gt 250 ] && BAD=250
exit "$BAD"
