#!/bin/sh
# Regenerate conformance/REPORT.md from an actual run of the official Tcl test
# suite against tclrs.
#
# Everything the report claims comes from this one command: it fetches and
# verifies the suite, extracts every case, runs each one under tclsh and under
# tclrs, and writes the comparison out. From a fresh checkout, with a `tclsh`
# on PATH and a stable Rust toolchain, this is the whole reproduction.
#
# Intermediate artifacts land in conformance/work/ and are reused on a rerun,
# which makes an interrupted run cheap to resume. Delete that directory to
# force everything to be recomputed.
#
# Environment:
#   TCLSH  the reference interpreter (default: tclsh from PATH)
#   JOBS   how many suite files to process at once (default: CPU count)

set -eu

here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)

"$here/fetch-suite.sh"

cd "$here/runner"
cargo build

exec ./target/debug/tclrs-conformance ${JOBS:+--jobs "$JOBS"} "$@"
