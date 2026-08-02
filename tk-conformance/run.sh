#!/bin/sh
# Regenerate tk-conformance/REPORT.md from an actual run of the official Tk test
# suite against tclrs hosting the real Tk.
#
# Everything the report claims comes from this one command: it fetches and
# verifies the suite, extracts every case, runs each one under tclsh-with-Tk and
# under tclrs's Tk host, and writes the comparison out. From a fresh checkout,
# with a `tclsh` that can `package require Tk` on PATH and a stable Rust
# toolchain, this is the whole reproduction.
#
# It needs a window server. Both sides open real windows, which is the point of
# hosting the real Tk, so a headless machine measures nothing here.
#
# Intermediate artifacts land in tk-conformance/work/ and are reused on a rerun,
# which makes an interrupted run cheap to resume. Delete that directory to force
# everything to be recomputed.
#
# Environment:
#   TCLSH  the reference interpreter (default: tclsh from PATH)
#   JOBS   how many suite files to process at once (default: min(CPUs, 4) —
#          every process here talks to the window server, so the ceiling is the
#          display rather than the CPU)

set -eu

here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)

"$here/fetch-suite.sh"

cd "$here/runner"
cargo build

exec ./target/debug/tclrs-tk-conformance ${JOBS:+--jobs "$JOBS"} "$@"
