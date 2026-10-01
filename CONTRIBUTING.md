# Developing ARK

Rules and the change lifecycle are in [`AGENTS.md`](AGENTS.md). This file is the commands.

Needs Rust 1.90 or newer. No model, key or network is needed for any test: a scripted
`MockProvider` drives the loops.

## Commands

| Command | What |
|---|---|
| `cargo test --workspace` | all tests |
| `scripts/ci.sh` | everything a release is held to: fmt, clippy `-D warnings`, tests, release build, version check, simulated benchmark against `bench/baseline.json` |
| `ARK_UI_PAGE_TESTS=1 scripts/ci.sh` | also the WebKit page tests (setup below) |
| `cargo fmt --all` | format |
| `cargo run -p writing-agent` | a real interactive app (needs a model, see below) |
| `ARK_PROVIDER=mock ARK_MOCK_SCRIPT=script.json cargo run -p writing-agent` | the same app with a scripted model ([`docs/CLI.md`](docs/CLI.md#testing-an-app-without-a-model)) |
| `cargo build --release -p ark-bench && ./target/release/ark-bench run --out new.json` | simulated benchmark ([`docs/BENCH.md`](docs/BENCH.md)) |

A live model: `ARK_PROVIDER=ollama ARK_MODEL=qwen3:4b-instruct` (local Ollama), or
`anthropic` / `openai` with a key ([`docs/OPERATIONS.md`](docs/OPERATIONS.md)).

Page tests, once:

```sh
cd crates/ark-ui/tests/web && npm ci && npx playwright-core install webkit
```

## Tests that guard an invariant

| Test | Guards |
|---|---|
| `ark-ui/tests/neutral.rs` | no domain words in the runtime or front-ends |
| `ark-sdk/tests/long_session.rs` | request size stays flat over 500 turns (no transcript replay) |
| `ark-ui/tests/host.rs` | the window sends the model what the terminal sends |
| `ark-ui/tests/resource_bounds.rs` | flat memory, fds and turn time (`ARK_STRESS_TURNS=2000` for the long run) |
| `ark-sdk/tests/crash_recovery.rs` | a kill mid-turn leaves nothing marked done |
| `ark-runtime/tests/loop_guard.rs` | a repeating model is stopped; a working one is not |
| `ark-runtime/tests/reuse_semantics.rs`, `result_reuse.rs` | no stale or failed result is reused |
| `ark-runtime/tests/retry.rs`, `ark-model/tests/http_resilience.rs` | retries are bounded and never re-run a tool |
| `ark-runtime/tests/request_profile.rs` | every model request is explained by a context report |
| `ark-bench/tests/suite.rs` | the simulated benchmark is deterministic |

## Small builds

The full-debuginfo `target/debug` reaches 15+ GB. On a tight disk:

```sh
CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_INCREMENTAL=0 scripts/ci.sh
```

`target/` is always safe to delete.
