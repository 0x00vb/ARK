#!/bin/sh
# Every crate uses the workspace version, and the changelog has an entry for it. With a tag
# argument (vX.Y.Z), the tag must match the version and point at HEAD.
set -eu
cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ -n "$version" ] || { echo "no workspace version"; exit 1; }
for m in crates/*/Cargo.toml examples/*/Cargo.toml; do
    grep -q '^version.workspace = true' "$m" || { echo "$m does not use the workspace version"; exit 1; }
done
grep -q "^## \[$version\]" CHANGELOG.md || { echo "CHANGELOG.md has no entry for $version"; exit 1; }
if [ $# -gt 0 ]; then
    [ "$1" = "v$version" ] || { echo "tag $1 does not match version $version"; exit 1; }
    [ "$(git rev-parse "$1^{commit}")" = "$(git rev-parse HEAD)" ] || { echo "tag $1 is not HEAD"; exit 1; }
fi
echo "version $version ok"
