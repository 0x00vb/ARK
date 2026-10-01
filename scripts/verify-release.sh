#!/bin/sh
# Verify a release the way a consumer gets it: clone the canonical remote at the tag into an
# empty directory, check the tag matches the version, and run every CI check there.
#
#   scripts/verify-release.sh v0.4.0            # remote: this checkout's `origin`
#   scripts/verify-release.sh v0.4.0 <url>
#
# CARGO_TARGET_DIR is honored, so repeated runs can share a build cache.
set -eu
tag=$1
here=$(cd "$(dirname "$0")/.." && pwd)
remote=${2:-$(git -C "$here" remote get-url origin)}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
echo "cloning $remote at $tag"
git clone --quiet --depth 1 --branch "$tag" "$remote" "$work/ark"
cd "$work/ark"
scripts/check-version.sh "$tag"
scripts/ci.sh
echo "release $tag verified from a fresh clone of $remote"
