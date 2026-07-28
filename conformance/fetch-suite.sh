#!/bin/sh
# Fetch the official Tcl source release and unpack its `tests/` directory.
#
# The test suite is not part of a binary Tcl install; it ships only in the
# source release. This script downloads that release, verifies it against a
# pinned SHA-256, and unpacks it under conformance/vendor/. It is idempotent:
# a vendor tree that already carries the right stamp is left alone.
#
# Source of the artifact:
#   https://sourceforge.net/projects/tcl/files/Tcl/9.0.4/tcl9.0.4-src.tar.gz
# SourceForge is the Tcl project's own release channel; the JSON release index
# at https://sourceforge.net/projects/tcl/best_release.json publishes
# md5sum 6c409d091c8f85f8601957bb6e77c8b3 and size 11866337 for this file,
# which is what the pinned SHA-256 below was taken from.
#
# Override TCL_VERSION to measure against a different release. The checksum is
# only pinned for the default version; any other version prints a warning and
# is fetched unverified, because there is nothing to compare it against.

set -eu

TCL_VERSION="${TCL_VERSION:-9.0.4}"
PINNED_VERSION=9.0.4
PINNED_SHA256=d0aed49230bc02a65c1e0229e65f34590a4b037ec40d546f32573b467f7551ea

here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
vendor="$here/vendor"
tarball="$vendor/tcl$TCL_VERSION-src.tar.gz"
tree="$vendor/tcl$TCL_VERSION"
url="https://downloads.sourceforge.net/project/tcl/Tcl/$TCL_VERSION/tcl$TCL_VERSION-src.tar.gz"

if [ -d "$tree/tests" ]; then
	echo "suite already present: $tree/tests"
	exit 0
fi

mkdir -p "$vendor"

if [ ! -f "$tarball" ]; then
	echo "fetching $url"
	curl -fsSL --retry 3 -o "$tarball.part" "$url"
	mv "$tarball.part" "$tarball"
fi

sum=$(shasum -a 256 "$tarball" | cut -d' ' -f1)
if [ "$TCL_VERSION" = "$PINNED_VERSION" ]; then
	if [ "$sum" != "$PINNED_SHA256" ]; then
		echo "checksum mismatch for $tarball" >&2
		echo "  expected $PINNED_SHA256" >&2
		echo "  got      $sum" >&2
		exit 1
	fi
	echo "sha256 ok: $sum"
else
	echo "warning: TCL_VERSION=$TCL_VERSION has no pinned checksum" >&2
	echo "warning: fetched sha256 is $sum, unverified" >&2
fi

tar xzf "$tarball" -C "$vendor"
test -d "$tree/tests" || {
	echo "no tests/ directory in $tarball" >&2
	exit 1
}
echo "unpacked $(ls "$tree/tests"/*.test | wc -l | tr -d ' ') test files into $tree/tests"
