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

step "generated protocol contract"
scripts/protocol.sh

step "version and release state"
scripts/check-version.sh

if [ "$(uname -s)" = Darwin ]; then
  step "macOS 11 consumer deployment target"
  MACOSX_DEPLOYMENT_TARGET=11.0 cargo check -p ark-computer --locked
fi

step "clippy (warnings are errors)"
cargo clippy --workspace --all-targets --locked -- -D warnings

step "tests"
page_tests=${ARK_UI_PAGE_TESTS:-}
unset ARK_UI_PAGE_TESTS
if [ -n "${RUST_TEST_THREADS:-}" ]; then
  cargo test --workspace --locked -- --test-threads "$RUST_TEST_THREADS"
else
  cargo test --workspace --locked
fi

if [ -n "$page_tests" ]; then
  node --experimental-vm-modules --test crates/ark-ui/tests/web/browser-route.test.mjs crates/ark-ui/tests/web/avatar-initial.test.mjs crates/ark-ui/tests/web/transport-reconnect.test.mjs
  step "WebKit page tests"
  node --test --test-concurrency=1 crates/ark-ui/tests/web/computer-recovery.test.mjs crates/ark-ui/tests/web/project-section.test.mjs crates/ark-ui/tests/web/upload-drafts.test.mjs crates/ark-ui/tests/web/work-state.test.mjs crates/ark-ui/tests/web/automations-library.test.mjs crates/ark-ui/tests/web/automation-inputs.test.mjs crates/ark-ui/tests/web/context-menu.test.mjs crates/ark-ui/tests/web/controls.test.mjs crates/ark-ui/tests/web/board-canvas.test.mjs crates/ark-ui/tests/web/calendar-data.test.mjs crates/ark-ui/tests/web/calendar.test.mjs
  ARK_UI_PAGE_TESTS="$page_tests" cargo test -p ark-ui --test page --locked -- --test-threads 1
  ARK_UI_PAGE_TESTS="$page_tests" cargo test -p ark-ui --lib --locked desktop_notification
fi

step "release build"
cargo build --workspace --release --locked

step "simulated benchmark against the committed baseline"
out=$(mktemp -d)
bench="${CARGO_TARGET_DIR:-target}/release/ark-bench"
"$bench" run --label candidate --out "$out/candidate.json" >/dev/null 2>&1
"$bench" compare bench/baseline.json "$out/candidate.json" --md
rm -rf "$out"

printf '\nall checks passed\n'
