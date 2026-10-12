#!/bin/sh
# Verify a release the way a consumer gets it: clone the canonical remote at the tag into a
# clean checkout, install the locked page-test dependency when needed, check the tag matches the
# version, and run every CI check there. The checkout path and target cache stay stable so Cargo
# can reuse local-crate fingerprints across releases.
#
#   scripts/verify-release.sh v0.4.0            # remote: this checkout's `origin`
#   scripts/verify-release.sh v0.4.0 <url>
#
# ARK_RELEASE_VERIFY_DIR and CARGO_TARGET_DIR can override the stable caches.
set -eu
tag=$1
here=$(cd "$(dirname "$0")/.." && pwd)
remote=${2:-$(git -C "$here" remote get-url origin)}
cache_root=${ARK_RELEASE_VERIFY_DIR:-${TMPDIR:-/tmp}/ark-release-verify-$(id -u)}
case "$cache_root" in
    ""|/|"${HOME:-__no_home__}"|"${TMPDIR:-/tmp}")
        echo "refusing unsafe verifier directory: $cache_root" >&2
        exit 1
        ;;
esac
marker="$cache_root/.ark-release-verifier"
if [ -e "$cache_root" ] && [ ! -f "$marker" ]; then
    echo "refusing verifier directory without its marker: $cache_root" >&2
    exit 1
fi
mkdir -p "$cache_root"
: > "$marker"
lock="$cache_root/lock"
mkdir "$lock" 2>/dev/null || {
    echo "another release verification is using $cache_root" >&2
    exit 1
}
trap 'rmdir "$lock" 2>/dev/null || true' EXIT HUP INT TERM
source="$cache_root/source"
rm -rf "$source"
echo "cloning $remote at $tag"
git clone --quiet --depth 1 --branch "$tag" "$remote" "$source"
cd "$source"
if [ -n "${ARK_UI_PAGE_TESTS:-}" ]; then
    (
        cd crates/ark-ui/tests/web
        npm ci --prefer-offline --no-audit --no-fund
        npx playwright-core install webkit
    )
fi
: "${CARGO_TARGET_DIR:=$cache_root/target}"
export CARGO_TARGET_DIR
scripts/check-version.sh "$tag"
scripts/ci.sh
echo "release $tag verified from a fresh clone of $remote"
