# Working on ARK

Instructions for coding agents (and people). Read this before changing anything.

ARK is a domain-neutral agent runtime in Rust: the loop, context, models, tools, memory, an
interaction layer, a terminal and a window. Applications bring the domain. The goal is the
lowest **cost per successful task** for any app built on it.

## Start here

| File | Holds | Read |
|---|---|---|
| `AGENTS.md` | how to work in this repo | always |
| [`CURRENT_STATE.md`](CURRENT_STATE.md) | what is in progress, blocked, next | always |
| [`ROADMAP.md`](ROADMAP.md) | where ARK is going, limits, rejected ideas | before starting a change |
| [`CHANGELOG.md`](CHANGELOG.md) | what shipped, per version | when touching public API |
| [`docs/adr/`](docs/adr/README.md) | why key decisions were made | before changing a boundary |
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | layers, crates, one turn | when the task needs it |
| [`../WORKSPACE.md`](../WORKSPACE.md) | the other repos and cross-repo rules | when a change crosses repos |

Commands: [`CONTRIBUTING.md`](CONTRIBUTING.md). Releases and upgrades:
[`docs/RELEASING.md`](docs/RELEASING.md). Before ending a session, update `CURRENT_STATE.md`
if what is in progress, blocked or next has changed.

## Boundaries

```text
application   tools, prompts, vocabulary, commands, look      (consumer crates, never ARK)
presentation  ark-cli, ark-ui: drawing, keys, transport       (never each other)
interaction   ark-interact: turns, commands, reports, traces  (never draws)
runtime       ark-core .. ark-orchestrate: loop, context, tools, several agents   (never a front-end)
```

| Rule | Check |
|---|---|
| `ark-core` names no tool, file format or domain concept | `crates/ark-ui/tests/neutral.rs`; ask "would this make sense for an agent that edits novels?" |
| Domain behavior goes through a seam (`Adapter`, `AgentProfile`, `Distiller`, `Sniffer`, `KnowledgeProvider`, `Provider`), never an `if domain` | review |
| Orchestration names no vendor; vendors live in `ark-model`. Team, manager, department are things apps build from `AgentSpec`s | `crates/ark-ui/tests/neutral.rs` |
| A delegated agent receives a brief, never another agent's conversation | `crates/ark-orchestrate/tests/orchestration.rs` |
| The runtime never sees keys, colours or terminals; it emits `HarnessEvent`s and takes `ApprovalGate`, `CancelToken`, `Steering` | review |
| Anything both front-ends show (commands, labels, views, traces) is computed once in `ark-interact`. A feature in one front-end has the same meaning in the other | `crates/ark-ui/tests/host.rs` compares CLI and GUI requests |
| Terminal history is not model context. Never replay the transcript into a prompt | `crates/ark-sdk/tests/long_session.rs` |
| Dependencies point down only; no ARK crate depends on a consumer | `Cargo.toml` |

## How to decide

- **Optimize cost per successful task**, not tokens alone. A change that saves tokens and
  loses success is a regression. `ark-bench` reports both.
- **Benchmark before and after** any change to the harness, prompts, request layout, tools
  offered or distillers. Simulated always; live when model behavior could change
  ([`docs/BENCH.md`](docs/BENCH.md)). Record live results in `bench/` and `bench/RESULTS.md`.
- **Deterministic before inference.** If code can decide it (classify, distill, dedupe, stop
  a loop, compact), code decides it. Add a model call only with a benchmark that pays for it.
- **Keep context bounded.** New context sources go through the budget and show up in
  `ContextReport`. Large data stays in the store and is read on demand.
- **No speculative abstraction.** New public surface needs a real consumer today, ideally
  two domains. Prefer a small change to an existing seam over a new one.
- **Dogfood.** Exercise a change through a real app (`writing-agent`, ARK Desktop) in the
  front-end it affects, not only through unit tests.
- **Check the roadmap** before starting: measured-and-rejected ideas are listed there.

## Lifecycle

```text
Understand -> Change -> Test -> Benchmark (when relevant) -> Dogfood -> Docs/roadmap -> Commit -> Release (when appropriate)
```

| Step | Done means |
|---|---|
| Understand | read the code and the tests that guard it; find the seam the change belongs to |
| Change | the smallest change in the right layer; tests drive loops with `MockProvider` |
| Test | `scripts/ci.sh` passes (fmt, clippy `-D warnings`, tests, release build, version, bench gate) |
| Benchmark | before/after numbers for harness changes; `bench/baseline.json` regenerated in the same commit if numbers move on purpose |
| Dogfood | a real task through the affected app and front-end |
| Docs/roadmap | `CHANGELOG.md` `[Unreleased]` for any user- or developer-visible change; the matching doc in `docs/`; `ROADMAP.md` if a limit, priority or state changed; an ADR in `docs/adr/` for a decision a later change could undo; `CURRENT_STATE.md` |
| Commit | one logical change, message `area: what changed` in plain words |
| Release | only when asked or when a fix must reach consumers; follow [`docs/RELEASING.md`](docs/RELEASING.md) exactly |

## Releases and consumers

- Never move, delete or re-create a published tag. A fix after a tag is a new version.
- Consumers pin a tag. Never pin or suggest `branch = "main"`; never commit a `path =` ARK
  dependency in a consumer.
- A release is reproducible: `scripts/verify-release.sh vX.Y.Z` passes from a fresh clone.
- Push to both remotes (`origin`, `github`) only when the person asked for a push or a
  release.
- Breaking a public API needs a minor bump in `0.x` and a `Breaking` changelog entry with
  what to change.

## Where things go

| You want to | Put it in |
|---|---|
| a new model backend | `ark-model` |
| a loop, context or reuse behavior | `ark-runtime` (`harness`, `budget`, `distill`, `catalog`) |
| vendor session handling or provider capacity | `ark-model` (`session`, `capacity`, `cli`) |
| agents, tasks, delegation, scheduling, the orchestration graph | `ark-orchestrate` |
| rule selection, the `rules` tool | `ark-runtime::rules`; types in `ark-core::rules` |
| a tool each agent gets its own of (its own state or model) | a `PerAgent` source (`ark_runtime::per_agent`), like the web and the browser |
| a capability that needs an account (a provider, its sign-in, a service's tools) | `ark-connect`: data in `catalog.json`, the service in `services/` |
| the person's ARK Account (sign-in to ARK Cloud, entitlements) | `ark-account`; never in a runtime crate |
| another agent program an agent can run on (its start, its protocol, its catalog entry) | `ark-acp`; the work model sees only `ark_runtime::worker` |
| a persisted record | `ark-store`, with `#[serde(default)]` on new fields |
| a command, view or label both front-ends show | `ark-interact` |
| terminal rendering or keys | `ark-cli` |
| window rendering, page API, transport | `ark-ui` (`web/` is plain ES modules, no build step) |
| a benchmark workload | `ark-bench/src/suite` (neutral names only) |
| a domain tool, prompt or distiller | the consuming app, not ARK |
| why a boundary or format was chosen | `docs/adr/` |

## Writing style (docs, comments, commit messages, reports)

Write like an engineer explaining something to a colleague.

- No em dashes or en dashes. Use a period, a comma, a colon or parentheses.
- Short sentences, plain words. Lead with what matters.
- Prefer a table or a short list to paragraphs. Cut what the reader knows from the code.
- Say what a thing does, not how good it is. No marketing words ("seamless", "robust",
  "powerful", "leverage", "delve", "comprehensive", "cutting-edge").
- No filler openers or closers.
- Never invent output. Examples in docs come from real runs or tests, and say so.
