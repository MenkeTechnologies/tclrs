#!/bin/sh
# Fetch the official Tk source release and unpack its `tests/` directory.
#
# The Tk test suite is not part of a binary Tk install; it ships only in the
# source release. This script downloads that release, verifies it against a
# pinned SHA-256, and unpacks it under tk-conformance/vendor/. It is idempotent:
# a vendor tree that already carries the right stamp is left alone.
#
# Source of the artifact:
#   https://sourceforge.net/projects/tcl/files/Tcl/9.0.4/tk9.0.4-src.tar.gz
# SourceForge is the Tcl project's own release channel; its file index at
#   https://sourceforge.net/projects/tcl/rss?path=/Tcl/9.0.4
# publishes md5 1b133d4ddf3dec2c01340cdc68c87096 for this file, which is what
# the pinned SHA-256 below was taken from.
#
# Override TK_VERSION to measure against a different release. The checksum is
# only pinned for the default version; any other version prints a warning and
# is fetched unverified, because there is nothing to compare it against.

set -eu

TK_VERSION="${TK_VERSION:-9.0.4}"
PINNED_VERSION=9.0.4
PINNED_SHA256=d7a146d2917eb8b5cc95276dbf0e3d03c7464d2b19c1675357857c989301dbb4

here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
vendor="$here/vendor"
tarball="$vendor/tk$TK_VERSION-src.tar.gz"
tree="$vendor/tk$TK_VERSION"
url="https://downloads.sourceforge.net/project/tcl/Tcl/$TK_VERSION/tk$TK_VERSION-src.tar.gz"

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
if [ "$TK_VERSION" = "$PINNED_VERSION" ]; then
	if [ "$sum" != "$PINNED_SHA256" ]; then
		echo "checksum mismatch for $tarball" >&2
		echo "  expected $PINNED_SHA256" >&2
		echo "  got      $sum" >&2
		exit 1
	fi
	echo "sha256 ok: $sum"
else
	echo "warning: TK_VERSION=$TK_VERSION has no pinned checksum" >&2
	echo "warning: fetched sha256 is $sum, unverified" >&2
fi

tar xzf "$tarball" -C "$vendor"
test -d "$tree/tests" || {
	echo "no tests/ directory in $tarball" >&2
	exit 1
}
echo "unpacked $(ls "$tree/tests"/*.test | wc -l | tr -d ' ') test files into $tree/tests"
