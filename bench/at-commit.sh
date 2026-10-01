#!/bin/sh
# Build the current ark-bench against the runtime at an earlier commit, so a comparison uses
# the same measurement code on both sides.
#
#   bench/at-commit.sh <commit> <output-binary>
#
# ARK_BENCH_TARGET sets a reusable target directory (faster repeat builds).
set -eu
commit=$1
out=$2
repo=$(git rev-parse --show-toplevel)
work=$(mktemp -d)
trap 'git -C "$repo" worktree remove --force "$work/src" >/dev/null 2>&1 || true; rm -rf "$work"' EXIT
git -C "$repo" worktree add -q --detach "$work/src" "$commit"
rsync -a --delete "$repo/crates/ark-bench/" "$work/src/crates/ark-bench/"
target=${ARK_BENCH_TARGET:-$work/target}
CARGO_TARGET_DIR="$target" cargo build -q --release -p ark-bench --manifest-path "$work/src/Cargo.toml"
cp "$target/release/ark-bench" "$out"
echo "built $out from $commit"
