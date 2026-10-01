# ARK Architecture

ARK is a layered set of Rust crates. Each layer depends only on the ones beneath it, and
nothing depends on a consumer. The design goal is a **small, stable public API over a headless
core**, with every domain concern injected rather than baked in.

```
                   ┌──────────────┐   ┌──────────────────┐
 your application →│   ark-cli    │   │      ark-ui      │  presentation (optional; neither
                   │  (terminal)  │   │ (window, browser)│  depends on the other)
                   └──────┬───────┘   └────────┬─────────┘
                      ┌───▼────────────────────▼──┐
                      │       ark-interact         │  interaction: App, Interaction, commands
                      │                            │  → Report, activity, trace, inspect
                      └─────────────┬─────────────┘
                      ┌─────────────▼─────────────┐
                      │          ark-sdk           │  Ark::builder(), AgentProfile, prelude
                      └─────────────┬─────────────┘
                      ┌─────────────▼─────────────┐
                      │        ark-runtime         │  Engine, Harness, Agent, budget, router,
                      │                            │  distill, catalog, capmap, metrics, notebook
                      └───┬─────────┬─────────┬────┘
             ┌────────────▼──┐ ┌────▼─────┐ ┌─▼────────────┐
             │  ark-model    │ │ ark-store│ │  ark-tools   │
             │ Provider/…    │ │ blobs+db │ │ Adapter/…/MCP│
             └───────┬───────┘ └────┬─────┘ └──────┬───────┘
                     └──────────────┼──────────────┘
                             ┌──────▼──────┐
                             │  ark-core   │   pure types; no tool/domain name
                             └─────────────┘
```

## The layers

### `ark-core` — the domain, made neutral
Pure data types with one invariant: **no tool name and no domain name appears here.** The
generalization from the origin workstation was concentrated here — the fixed security
vocabularies became open, consumer-supplied registries:

- **`Capability` / `CapabilityCatalog`** — "reason about capabilities, not tool names" is the
  crown-jewel idea; the *taxonomy* is now yours to declare (`drafting`, `citation-lookup`, …).
- **`ArtifactKind`** — universal kinds (`Text`, `Json`, `Source`, `Image`, `Archive`,
  `Binary`, `Blob`) plus `Other(String)` and a `Sniffer` seam for domain formats.
- **`ActionDescriptor` / `RiskTier`** — a typed tool operation and how consequential it is
  (`Read`/`Observe`/`Mutate`, the approval gate's currency).
- **`Invocation`** — the reproducibility spine: every tool call becomes one.
- **`EvidenceCard` / `ResearchState` / `InvestigationGraph`** — the compact memory that keeps
  context small and answers cited. Generic research methodology, not a domain model.
- **`WorkspaceContext` / `Focus` / `PresentDirective`** — what the user is looking at, and
  what the agent asks a UI to show.

### `ark-model` — bring-your-own-model
One `Provider` trait: `complete(system, messages, tools) -> Completion` with usage. The
runtime never knows which backend answered. Tool-name mangling papers over providers that
forbid `.` in tool names.

### `ark-store` — persistence
Content-addressed blobs (dedup by blake3), a SQLite metadata DB (projects, artifacts,
invocations, notes, evidence cards, working state, checkpoints — each a JSON blob with a few
promoted columns), and a Tantivy full-text index. Fully domain-agnostic.

### `ark-tools` — the tool boundary
`Adapter` (a namespaced set of `ActionDescriptor`s + a `run`), `Registry` (the agent's tool
list; ships **empty**), `RunCtx`/`BlobResolver` (how a tool reads artifacts and its
workspace), a `subprocess` helper, and `McpAdapter` — an out-of-process plugin bridge that
turns any MCP server (any language) into native actions with risk derived from annotations.

### `ark-runtime` — the loop
- **`Engine`** — ingest + `run_action` + the approval gate; pluggable `Sniffer`s; loads MCP
  plugins from `plugins.json`.
- **`Harness`** — the evidence-driven loop (budget, router, distill, dedup, progressive
  disclosure, working state, self-check, critic, stall guard, metrics).
- **`Agent`** — a simpler ReAct loop.
- **`AgentProfile`** — the single injection point: persona, capabilities + inferrer,
  distillers, knowledge, core actions, investigation markers, prompt fragments, and the
  approval classifier.
- **`Session`**: one continuing conversation. Turns are stored in full; the model gets a
  bounded carry-over (recent exchanges plus gists), never the transcript.
- **`ApprovalGate` / `Approver`**: asks a human before `Mutate` actions and remembers
  session grants by approval class. The front-end only renders the question.
- **`CancelToken`**: cooperative stop, checked at every await point in a turn.
- **`ContextReport`**: the token breakdown of each request by source.

### Three layers above the SDK

| Layer | Crate | Owns | Knows nothing of |
|---|---|---|---|
| Runtime | `ark-runtime` (via `ark-sdk`) | reasoning, tools, context, working state, models, approvals, cancel, steering | front-ends |
| Interaction | `ark-interact` | sessions, turns, commands and their `Report`s, activity labels, traces, context/usage/state views | terminals, windows, HTML, ANSI |
| Presentation | `ark-cli`, `ark-ui` | drawing, keys, layout | each other |

### `ark-interact`: the interaction layer
An app is described once as an `App` and opened as an `Interaction`: project, session,
approvals (`ChannelApprover`), turns (`run_turn` with a `TurnControl` of cancel, steering and
a trace recorder). Slash commands return a `Reply` holding a `Report` (title and blocks:
heading, text, fields, table, code, meter, actions) and an optional `Effect`. `activity`
turns `HarnessEvent`s into a person's words, `trace::interpret` turns a recorded turn into
calls with status, approval and context cost, and `inspect` builds the context, usage, state
and session views. Both front-ends call these; neither recomputes them.

### `ark-cli`: the shell
The terminal front-end. It drives a turn as a future and multiplexes runtime events,
approval requests, key presses and a spinner tick in one `select!` loop, and renders
`Report`s and `Activity` as lines. See [`CLI.md`](CLI.md).

### `ark-ui`: the workspace
A `Host` wraps one `Interaction` and exposes `call(method, params)` plus an event stream.
Two transports carry it: a Tauri window (feature `desktop`) and a loopback HTTP server with a
per-run token. The page in `web/` is plain ES modules; apps add panels, artifact viewers,
tool renderers, actions and status lines through `window.ark`, and backend methods through
`ArkUi::handler`. See [`UI.md`](UI.md).

### `ark-bench`: measurement
Beside the front-ends, on `ark-sdk`: workloads, a metered provider, a simulated model,
reports and comparisons. Any application can run its own workloads through it; it names no
domain. See [`BENCH.md`](BENCH.md).

### `ark-sdk` — the facade
`Ark::builder()` composes an `Engine`, a `ModelRouter`, and an `AgentProfile` into a
one-call `.ask()`. A `prelude` re-exports everything a typical consumer names.

## Key design principles (and where they live)

- **Composition over inheritance** — the profile *holds* strategies (distillers, inferrer,
  knowledge) rather than subclassing the harness.
- **Interfaces over domain assumptions** — `Provider`, `Adapter`, `Sniffer`,
  `KnowledgeProvider`, `Distiller`, `CapabilityCatalog` are all injection seams.
- **Headless core over UI coupling** — the runtime emits structured `HarnessEvent`s; a UI is
  optional and downstream. `PresentDirective` degrades to a no-op on a CLI.
- **Explicit state over hidden global state** — no singletons; everything hangs off the
  `Engine`/`Ark` you construct.
- **Lazy context over dumping** — progressive tool disclosure and evidence retrieval on
  demand.
- **Isolated memory over replay** — the working state is the resume unit, not the transcript.

## Events & observability

The loop is a stream of typed, owned, serializable `HarnessEvent`s (step, context report,
text delta, model call, tool call/result, duplicate, retrieval, approval requested/resolved,
state update, note, present, self-check, cancelled, answer). The terminal shell, the `--events`
JSON stream and the `ark-ui` window all consume the same stream. A UI, a logger, or a test
consumes them without coupling to the loop's internals. `Metrics` accounts for tokens (in/out/
peak), model calls by role and model id, tool calls, dedup and retrieval hits, and
tokens-per-finding.
