#!/bin/sh
# The checks a release must pass, in order. CI runs this; so can you.
#
#   scripts/ci.sh            # fmt, clippy, tests, release build, version, simulated bench
#   ARK_UI_PAGE_TESTS=1 scripts/ci.sh   # also the WebKit page tests (needs Node + Playwright)
set -eu
cd "$(dirname "$0")/.."

step() { printf '\n== %s\n' "$*"; }

step "format"
cargo fmt --all --check

step "clippy (warnings are errors)"
cargo clippy --workspace --all-targets --locked -- -D warnings

step "tests"
cargo test --workspace --locked

step "release build"
cargo build --workspace --release --locked

step "version"
scripts/check-version.sh

step "simulated benchmark against the committed baseline"
out=$(mktemp -d)
./target/release/ark-bench run --label candidate --out "$out/candidate.json" >/dev/null 2>&1
./target/release/ark-bench compare bench/baseline.json "$out/candidate.json" --md
rm -rf "$out"

printf '\nall checks passed\n'
