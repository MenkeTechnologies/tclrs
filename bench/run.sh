#!/bin/sh
# Benchmark tclrs against the reference interpreter.
#
# Four measured configurations per script:
#
#   tclsh           the reference implementation, from PATH
#   tclrs interp    tclrs with the JIT off (TCLRS_JIT=off)
#   tclrs JIT       tclrs as it ships, with fusevm's three JIT tiers armed
#   tclrs AOT       the script compiled to a native executable by `tclrs --aot`
#
# Wall clock of the whole process in every row, so the numbers include startup
# and are comparable across all four. `startup.tcl` is the empty script — what
# to subtract to see the work alone.
#
# Usage: bench/run.sh [script.tcl ...]      (default: every script in bench/)
#        RUNS=20 WARMUP=5 bench/run.sh
#
# Requires a release build; a debug build measures nothing useful. Uses
# hyperfine when it is installed and falls back to a warmed timing loop
# otherwise.
set -e

cd "$(dirname "$0")/.."
BENCH=bench
OUT=target/bench
TCLRS=target/release/tclrs
RUNS=${RUNS:-10}
WARMUP=${WARMUP:-3}

# Fallback timing when hyperfine is not installed: the same warmup-then-measure
# shape, mean and minimum over $RUNS runs, in milliseconds.
time_it() {
    label=$1
    command=$2
    i=0
    while [ "$i" -lt "$WARMUP" ]; do
        eval "$command" >/dev/null 2>&1
        i=$((i + 1))
    done
    perl -MTime::HiRes=time -e '
        my ($label, $command, $runs) = @ARGV;
        my @ms;
        for (1 .. $runs) {
            my $start = time();
            system("$command >/dev/null 2>&1");
            push @ms, (time() - $start) * 1000;
        }
        my $mean = 0; $mean += $_ for @ms; $mean /= @ms;
        my ($min) = sort { $a <=> $b } @ms;
        printf "%-14s mean %8.1f ms   min %8.1f ms   (%d runs)\n",
            $label, $mean, $min, scalar @ms;
    ' "$label" "$command" "$RUNS"
}

TCLSH=${TCLSH:-$(command -v tclsh || command -v tclsh9.0 || command -v tclsh8.6 || true)}
if [ -z "$TCLSH" ]; then
    echo "bench: no tclsh on PATH — the reference column would be missing" >&2
    exit 1
fi

echo "building tclrs (release)"
cargo build --release

mkdir -p "$OUT"
scripts=${*:-$(ls "$BENCH"/*.tcl)}

echo "compiling AOT binaries"
for script in $scripts; do
    "$TCLRS" --aot "$OUT/$(basename "$script" .tcl)" "$script"
done

echo
echo "tclsh: $TCLSH $(echo 'puts [info patchlevel]' | "$TCLSH")"
echo "host:  $(uname -sm), $(sysctl -n machdep.cpu.brand_string 2>/dev/null || echo unknown)"
echo "load:  $(uptime | perl -pe 's/.*(load aver)/$1/')"
echo

for script in $scripts; do
    name=$(basename "$script" .tcl)
    echo "── $name ──────────────────────────────────────────────"
    # Every row runs through `env`, including the ones that need no variable:
    # only the JIT rows would otherwise pay for the extra exec, which is the
    # same order of magnitude as the whole `startup` benchmark.
    if command -v hyperfine >/dev/null 2>&1; then
        hyperfine --warmup "$WARMUP" --runs "$RUNS" \
            --time-unit millisecond \
            --export-markdown "$OUT/$name.md" \
            -n "tclsh"        "env $TCLSH $script" \
            -n "tclrs interp" "env TCLRS_JIT=off $TCLRS $script" \
            -n "tclrs JIT"    "env TCLRS_JIT=on $TCLRS $script" \
            -n "tclrs AOT"    "env $OUT/$name"
    else
        time_it "tclsh"        "env $TCLSH $script"
        time_it "tclrs interp" "env TCLRS_JIT=off $TCLRS $script"
        time_it "tclrs JIT"    "env TCLRS_JIT=on $TCLRS $script"
        time_it "tclrs AOT"    "env $OUT/$name"
    fi
    echo
done
