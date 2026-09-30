# ARK — Agent Runtime Kit

**A domain-agnostic runtime, SDK, CLI, and UI foundation for building agents.**

ARK is the machinery that makes an LLM agent *effective, efficient, inspectable, and
reliable* — extracted from a real, working security-research workstation and stripped of
every domain assumption. It gives you an agent loop, context/token management, model
routing, a tool/permission model, an evidence-and-working-state memory, persistence, and
usage accounting, so you can start a new agent product without rebuilding any of it.

> **The test ARK holds itself to:** *Could this exact runtime power an agent whose only job
> is helping an author write a novel?* If not, the abstraction is too domain-specific. See
> [`examples/writing-agent`](examples/writing-agent) — it does, on the same runtime a
> security product uses, with zero changes to any ARK crate.

Not a framework you fight: **headless runtime first, optional CLI, optional GUI.** The
runtime never requires a UI, and adding a new domain is *configuration + extensions*, not
edits to the harness.

---

## Why ARK exists

Every serious agent re-implements the same hard parts: keeping the context window bounded as
a task runs long, distilling noisy tool output into something a model can reason over,
routing cheap vs. expensive models, gating dangerous actions, remembering what's established
vs. merely believed, and making all of it observable. ARK is those parts, done once, behind
small stable interfaces — with the domain (tools, prompts, vocabulary) injected by you.

## What's in the box

| Crate | Responsibility |
|---|---|
| **`ark-core`** | Pure domain types. **No tool or domain name appears here.** Artifacts, actions, invocations, the capability *catalog*, the evidence/working-state model, workspace focus. |
| **`ark-model`** | Bring-your-own-model: one `Provider` trait for chat + tool-calling. Anthropic, OpenAI(-compatible), Ollama, and a scriptable Mock. |
| **`ark-store`** | Content-addressed blob store + SQLite metadata + full-text search. Domain-agnostic. |
| **`ark-tools`** | The `Adapter`/`Registry` tool boundary, a subprocess helper, and an **MCP plugin bridge** for out-of-process tools in any language. Ships **no** domain tools. |
| **`ark-runtime`** | The agent loop(s): context **budget**, model **router**, deterministic **distillation**, progressive tool disclosure, a persistent **working state + investigation graph**, self-check + critic, **metrics**, and the approval gate. |
| **`ark-sdk`** | The small, stable facade — `Ark::builder()`, an `AgentProfile`, and a `prelude`. |
| **`ark-cli`** | The interactive terminal shell every ARK app opens into: streaming, approvals, cancel, sessions and resume, slash commands, context inspection, and a headless `-p` mode. See [`docs/CLI.md`](docs/CLI.md). |

Two unrelated example consumers ship in-tree to prove generality:
[`minimal-agent`](examples/minimal-agent) (calculator + clock) and
[`writing-agent`](examples/writing-agent) (a novelist's editor). A third, real consumer — a
security-research workstation — lives in its own repo and consumes ARK as a version-pinned
dependency, deleting ~5,300 lines of its own harness in the process.

## Quick start

```rust
use ark_sdk::prelude::*;
use std::sync::Arc;

// 1. Define a tool (an Adapter). Any number of actions, each with a JSON-Schema input.
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
// 2. Wire the agent: tools + persona + model. That's the whole integration surface.
let ark = Ark::builder()
    .store_root("./workspace")
    .tool(Arc::new(Weather))
    .profile(AgentProfile::builder()
        .instructions("You are a concise travel assistant. Use tools; never guess a forecast.")
        .build())
    .provider(ark_model::from_env_arc()?)  // ARK_PROVIDER / ARK_MODEL
    .build()?;

// 3. Run.
let project = ark.create_project("trip", "weekend planning")?;
let outcome = ark.ask(&project.id, "what's the weather in Lisbon today?").await?;
println!("{}", outcome.answer);
# Ok(()) }
```

Run the examples:

```bash
ARK_PROVIDER=ollama ARK_MODEL=qwen3:4b-instruct cargo run -p minimal-agent -- "what is 6 * 7?"
ARK_PROVIDER=ollama ARK_MODEL=qwen3:4b-instruct cargo run -p writing-agent   # the interactive writer
cargo test   # 114 tests, no live model required (the Mock provider drives the loops)
```

## The interactive shell

An ARK app is something you enter, not a command you call. `ark-cli` gives every app the same
terminal session: type in plain words, watch what the agent does, approve changes, press esc to
stop, leave with ctrl+d and come back with `--resume`. The app supplies its name, tools,
commands, labels and colours:

```rust
ark_cli::ArkCli::builder()
    .name("Writer")
    .ark(ark)
    .command(std::sync::Arc::new(ChapterCommand))
    .theme(ark_cli::Theme::default().accent(ark_cli::Accent::Magenta).prompt("✎ "))
    .run()
    .await
```

The terminal transcript is never replayed into the model. A 500-turn session sends the same
size request on turn 500 as on turn 50. Full guide: [`docs/CLI.md`](docs/CLI.md).

## Configuration reference

Wire everything through `Ark::builder()`; every knob has a sane default.

| Builder method | Default | Purpose |
|---|---|---|
| `.store_root(path)` | `./ark-workspace` | Store location; also loads `<root>/plugins.json`. |
| `.tool(Arc<dyn Adapter>)` / `.registry(r)` | empty | Register domain tools. |
| `.profile(AgentProfile)` | generic assistant | Persona, capabilities, distillers, knowledge, core actions, markers. |
| `.provider(Arc<dyn Provider>)` | — | Use one model for every role. |
| `.router(ModelRouter)` | — | Two-tier (fast/strong) routing by role. |
| `.context_budget(tokens)` | `6000` | Target prompt size; older rounds compress out. |
| `.max_steps(n)` | `16` | Tool-loop step cap. |
| `.planner(bool)` / `.critic(bool)` | `true` | Toggle the planning and self-critic passes. |
| `.allow_mutate(bool)` | `false` | Let `Mutate` actions run without a separate approval step. |

**Environment:** `ARK_PROVIDER` (`ollama`|`openai`|`anthropic`|`mock` with `ARK_MOCK_SCRIPT`), `ARK_MODEL`,
`ARK_MODEL_FAST`/`ARK_MODEL_STRONG` (+ `ARK_PROVIDER_*`) for two-tier routing,
`ARK_CTX_BUDGET`, and the vendor keys `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` /
`OLLAMA_HOST`.

The `AgentProfile` is the single injection point for everything domain-specific — see
[`docs/EXTENSION.md`](docs/EXTENSION.md).

## Token & LLM efficiency (preserved from the source)

ARK keeps the optimizations that made the original agent cheap, and makes them observable:

- **Bounded context.** The transcript is a list of *rounds*; each model call is assembled
  under a token budget, dropping whole oldest rounds (never half a tool-call pair). Context
  stays roughly flat no matter how long a task runs. (`ark-runtime::budget`)
- **Distillation, not dumping.** Raw tool output never enters the prompt; a deterministic
  distiller turns each result into a compact evidence card with the salient identifiers, and
  the full rows stay retrievable on demand. No model call spent summarizing. (`distill`)
- **Progressive tool disclosure.** The model sees a one-line capability index, not every
  tool's full schema, and loads a schema only when it needs it. (`catalog`)
- **Dedup.** An identical `(action, input)` call reuses prior evidence instead of re-running.
- **Working state over replay.** A resumed session loads a compact structured state, not the
  whole chat history.
- **Model routing.** Cheap models plan/summarize/extract; the strong model reasons and
  synthesizes. Deterministic work (classification, dedup, self-check) spends no model call.
- **Full accounting.** Every run reports input/output/peak-prompt tokens, calls by model and
  role, tool calls, dedup/retrieval hits, and tokens-per-finding. (`Metrics`)

## Architecture & dependency direction

```
ark-core ──► ark-model, ark-store, ark-tools ──► ark-runtime ──► ark-sdk ──► your app
```

No ARK crate depends on a consumer; there are no cycles. `ark-core` depends on nothing but
serde/uuid/blake3/time. Full detail in [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md), and
the extraction rationale in [`docs/EXTRACTION_MAP.md`](docs/EXTRACTION_MAP.md).

## Consuming & upgrading ARK

ARK is a Cargo workspace publishing independently-versioned crates; consumers depend on them
as ordinary dependencies and **never copy the source**. Pin a compatible range and update
intentionally:

```toml
[dependencies]
ark-sdk = "0.1"   # pulls the matching ark-core/model/store/tools/runtime
```

Versioning, compatibility guarantees, and the release process are in
[`docs/VERSIONING.md`](docs/VERSIONING.md); changes are tracked in
[`CHANGELOG.md`](CHANGELOG.md).

## Status & roadmap

Done and green (114 tests): the headless runtime, the model/tool/store abstractions, the
SDK, the extension model (adapters + MCP plugins + `AgentProfile`), token/context
optimization, usage accounting, docs, and two unrelated example agents.

Since v0.1 (see [`CHANGELOG.md`](CHANGELOG.md)): an injectable `AcquisitionProvider` seam, and
the **origin security product migrated onto ARK** as a versioned dependency (its harness
duplication deleted; behavior and token efficiency verified unchanged) — ARK's third unrelated
consumer.

v0.2 adds `ark-cli`, the interactive shell, and the runtime seams it needs (streaming,
approvals, cancellation, sessions, context reports). Next (tracked in
[`docs/ROADMAP.md`](docs/ROADMAP.md)): `ark-ui`, a themeable GUI shell on the same seams.

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md). The one rule that matters: **no tool name and no
domain name in `ark-core`** — the invariant that keeps ARK reusable.

## License

MIT OR Apache-2.0.
