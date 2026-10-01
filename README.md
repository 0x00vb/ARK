# ARK, the Agent Runtime Kit

A Rust runtime, SDK, terminal shell and workspace UI for building agents in any domain. ARK
provides the agent loop, context and token management, model routing, the tool and approval
model, an evidence and working-state memory, persistence, usage accounting and a benchmark
harness. An application supplies its tools, prompts and vocabulary.

ARK came out of a security research workstation (SecRes) and keeps no trace of that domain.
The check it holds itself to: the same runtime must be able to run an agent that helps an
author write a novel. [`examples/writing-agent`](examples/writing-agent) does, with no change
to any ARK crate.

The runtime is headless. The shell and the workspace are optional front-ends on top of it.

## Crates

| Crate | Responsibility |
|---|---|
| `ark-core` | Pure types: artifacts, actions, invocations, the capability catalog, evidence cards, working state, workspace focus. No tool or domain name. |
| `ark-model` | One `Provider` trait for chat with tool calls: Anthropic, OpenAI and compatible endpoints, Ollama, and a scriptable mock. Timeouts, output caps, error classes, retry policy, prompt caching. |
| `ark-store` | Content-addressed blobs, SQLite metadata, full-text search. |
| `ark-tools` | The `Adapter` / `Registry` tool boundary, a subprocess helper, and an MCP bridge for out-of-process tools. Ships no domain tools. |
| `ark-runtime` | The agent loop: context budget, model router, deterministic distillation, progressive tool disclosure, result reuse, parallel reads, stall guard, retries, working state, self-check and critic, metrics, sessions, approvals, cancellation, steering. |
| `ark-sdk` | The facade: `Ark::builder()`, `AgentProfile`, a `prelude`. |
| `ark-interact` | Between the runtime and any front-end: `App`, `Interaction`, commands that return `Report`s, activity labels, traces, context and usage views, logging. Draws nothing. |
| `ark-cli` | The terminal shell every ARK app opens into, plus a headless `-p` mode. [`docs/CLI.md`](docs/CLI.md) |
| `ark-ui` | The same app in a window (Tauri) or a local browser page. [`docs/UI.md`](docs/UI.md) |
| `ark-bench` | Benchmark and eval harness for any ARK app. [`docs/BENCH.md`](docs/BENCH.md) |

Consumers: [`minimal-agent`](examples/minimal-agent) (calculator and clock),
[`writing-agent`](examples/writing-agent) (a novelist's editor), and SecRes, in its own
repository, which pins ARK by release tag.

## Quick start

```rust
use ark_sdk::prelude::*;
use std::sync::Arc;

// A tool: any number of actions, each with a JSON Schema input.
struct Weather;
#[async_trait::async_trait]
impl Adapter for Weather {
    fn name(&self) -> &str { "weather" }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new("weather.today", "get today's forecast", RiskTier::Observe)
            .with_input_schema(serde_json::json!({
                "type": "object", "required": ["city"],
                "properties": { "city": { "type": "string" } }
            }))]
    }
    async fn run(&self, _a: &str, input: serde_json::Value, _c: &RunCtx<'_>) -> ark_tools::Result<ActionOutput> {
        let city = input.get("city").and_then(|v| v.as_str()).unwrap_or("");
        Ok(ActionOutput::json(serde_json::json!({ "city": city, "high_c": 24, "sky": "clear" })))
    }
}

# async fn run() -> Result<(), ArkError> {
let ark = Ark::builder()
    .store_root("./workspace")
    .tool(Arc::new(Weather))
    .profile(AgentProfile::builder()
        .instructions("You are a concise travel assistant. Use tools; never guess a forecast.")
        .build())
    .provider(ark_model::from_env_arc()?)  // ARK_PROVIDER / ARK_MODEL
    .build()?;

let project = ark.create_project("trip", "weekend planning")?;
let outcome = ark.ask(&project.id, "what's the weather in Lisbon today?").await?;
println!("{}", outcome.answer);
# Ok(()) }
```

```bash
ARK_PROVIDER=ollama ARK_MODEL=qwen3:4b-instruct cargo run -p minimal-agent -- "what is 6 * 7?"
ARK_PROVIDER=ollama ARK_MODEL=qwen3:4b-instruct cargo run -p writing-agent   # the interactive writer
cargo test --workspace   # no live model needed: the mock provider drives the loops
scripts/ci.sh            # everything a release is held to
```

## The shell and the workspace

An ARK app opens into a session: type in plain words, watch what the agent does, approve
changes, press esc to stop, leave with ctrl+d and continue with `--resume`. The same app runs in
a window or a browser page through `ark-ui`. The terminal transcript is never replayed into
the model: a 500-turn session sends the same size request on turn 500 as on turn 50.

```rust
let app = ark_interact::App::new("Writer").ark(ark).command(std::sync::Arc::new(ChapterCmd));
ark_cli::ArkCli::new(app).run().await
```

## How it keeps cost down

| Mechanism | Where |
|---|---|
| Each request is assembled under a token budget from whole rounds; old rounds drop in chunks | `ark-runtime::budget` |
| Tool output becomes a short evidence card; full rows stay in the store and `get_evidence` reads a filtered slice | `distill`, `evidence` |
| The model sees a compact index of actions and loads a schema only when it needs one; four runtime tools by default | `catalog`, `capmap` |
| A request layout a prefix cache can serve: fixed system prompt, tools in load order, working state last; Anthropic cache breakpoints | `harness`, `ark-model` |
| Identical successful calls are reused within the rules of `Reuse` | `harness` |
| Independent reads from one reply run together | `harness` |
| A loop that stops making progress is ended | `harness` |
| Working state and a bounded conversation carry-over instead of transcript replay | `session` |
| Every request reports where its tokens came from | `ContextReport` |

Measured before and after in [`bench/RESULTS.md`](bench/RESULTS.md).

## Configuration

`Ark::builder()` has a default for every setting.

| Builder method | Default | Purpose |
|---|---|---|
| `.store_root(path)` | `./ark-workspace` | store location; also loads `<root>/plugins.json` |
| `.tool(adapter)` / `.registry(r)` | empty | the application's tools |
| `.profile(AgentProfile)` | generic assistant | persona, capabilities, distillers, knowledge, core actions |
| `.provider(p)` / `.router(r)` | from the environment | one model, or a fast and a strong tier |
| `.context_budget(tokens)` | 6000 | request budget |
| `.max_steps(n)` | 16 | step limit per turn |
| `.planner(bool)` / `.critic(bool)` | on | planning and critic passes |
| `.allow_mutate(bool)` | off | run state-changing actions without approval |

Environment variables (models, timeouts, retries, logs, traces) are listed in
[`docs/OPERATIONS.md`](docs/OPERATIONS.md). The `AgentProfile` is where an application
injects its domain: [`docs/EXTENSION.md`](docs/EXTENSION.md).

## Dependency direction

```
ark-core -> ark-model, ark-store, ark-tools -> ark-runtime -> ark-sdk -> ark-interact -> ark-cli, ark-ui
                                                                    \-> ark-bench
```

No ARK crate depends on a consumer, and there are no cycles. Details:
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

## Versions

Consumers pin a release tag and upgrade on purpose. Tags never move; a correction is a new
version. See [`docs/VERSIONING.md`](docs/VERSIONING.md) and [`CHANGELOG.md`](CHANGELOG.md).

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md). The one hard rule: no tool name and no domain name in
`ark-core`.

## License

MIT OR Apache-2.0.
