# Roadmap

Where ARK is today and what is worth doing next. Keep it short: update it when a release
ships, a limit is found or fixed, or a priority changes. Numbers live in
[`bench/RESULTS.md`](bench/RESULTS.md); per-version detail in [`CHANGELOG.md`](CHANGELOG.md).

## Now

| | |
|---|---|
| Stable release | **v0.7.0** (2026-10-03): the measured runtime P0 to P2 (naive baseline, team workloads, faults, record and replay; preloading, the ready list, small arrays whole, planner off), the window's projects, top bar, Settings, Terminal, Browser and Files, and the chat that creates agents |
| `main` | v0.7.0 |
| Consumers | SecRes (pins v0.5.0), ARK Desktop (`ark-desktop` repo; pins v0.6.0), `examples/minimal-agent`, `examples/writing-agent`, `examples/agent-team` |
| Benchmark | live qwen3:4b: success 0.85, cost per successful task $0.0298 (v0.4.0, `bench/RESULTS.md`) |
| Phase | **Measured runtime**: P0 to P2 shipped in v0.7.0, measured live on qwen3:4b. Still open: the Haiku regression check of the P2 defaults (not run for v0.7.0), and overhead on `parallel` and `team` (about 1.4 times the naive loop per success on qwen3:4b). See Next. |

## State

| Area | Status |
|---|---|
| Runtime (`ark-core` to `ark-sdk`) | stable seams, bounded context, retries, crash recovery, flat memory over 2,000 turns |
| Orchestration (`ark-orchestrate`) | new: agents, tasks, delegation, capacity-aware scheduling, a graph and event stream; tested with scripted models, run live on Claude and ChatGPT subscriptions |
| Models (`ark-model`) | Ollama, Anthropic and OpenAI APIs, Claude and ChatGPT subscriptions through their CLIs, mock |
| Interaction (`ark-interact`) | commands, reports, activity, traces, context and usage views shared by both front-ends |
| Terminal (`ark-cli`) | interactive shell, approvals, steering, resume, headless `-p` |
| Workspace (`ark-ui`) | Tauri window and loopback browser page; page tested in WebKit; orchestration control in the protocol, app views in the main column |
| Measurement (`ark-bench`) | 14 generic workloads (one orchestrated team), simulated and live, a naive baseline loop, faults, record and replay; regression gate in `scripts/ci.sh` |
| Release | `scripts/ci.sh`, `scripts/verify-release.sh`, immutable tags, two remotes |

## Known limitations

| Limitation | Where it shows |
|---|---|
| Tauri IPC path of the native window has no automated test (the page is tested in WebKit; SecRes checks its bundle through the browser transport) | `ark-ui` |
| Anthropic and OpenAI prompt-cache reads are not measured against a live provider, only by unit tests and the simulated cache model | `ark-model`, `ark-bench` |
| Every live benchmark uses one 4B local model (qwen3:4b-instruct through Ollama) | `bench/RESULTS.md` |
| With qwen3:4b, `tool-lookup` and `approval` fail 3 of 3: the model picks a plausible neighbor tool, or files the note in its own working state | `bench/RESULTS.md` |
| Time to first token rose 137 to 259 ms (median) with the working state last in the request; cause not proven | `bench/RESULTS.md` |
| Subscription providers (`claude`, `codex`) start a process per call and do not stream | `ark-model` |
| `claude:haiku` sometimes tries host tools natively; it recovered in 6 of 6 tasks in the last runs, and a check asks again when it gives up, but that check has only a scripted test | `bench/RESULTS.md` |
| Orchestration is measured on one program, three runs per setting; managed sessions cut uncached input but not wall time | `bench/RESULTS.md` |
| The window has no graph view of its own; ARK Desktop draws one from `graph` and `orch` events through `ark.view` | `ark-ui` |
| Agents created from a window use the app's base profile; a custom `AgentProfile` needs code | `ark-orchestrate`, `ark-ui` |
| Approval grants are not persisted across processes | CLI, UI |
| One `Host` serves one live session; two browser tabs share it | `ark-ui` |
| `/context` token numbers are estimates; provider numbers are in `/trace` | `ark-interact` |
| Not on crates.io; consumers pin a git tag | release |

## Next

### Milestone: a measured runtime (0.7)

The claim to prove: agents on ARK do more work, more reliably, for fewer tokens. Each phase
ends with numbers in `bench/RESULTS.md`; nothing in P2 lands without a before/after.

| Metric | What it says |
|---|---|
| success rate, cost per successful task | the headline; tokens alone can hide a lost success |
| uncached input tokens per success | what a provider bills at full price |
| wall time per success, parallel speedup | whether delegation and parallel calls pay |
| recovery rate | runs that hit a fault and still succeeded |
| interventions per task | approvals, messages and retries a person had to give |
| variance across repetitions | whether a result is reproducible |

| Phase | Work | Done means |
|---|---|---|
| Before P0 | Orchestration control (spawn, submit, pause, resume, end, message, cancel, retry) moves from `ark-ui`'s Host into `ark-interact`, so the terminal can operate agents and an eval driven from it can too | `/agent` and `/task` in both front-ends; the Host forwards |
| P0 foundations | In `ark-bench`: a naive baseline harness (full transcript every call, every tool offered, raw results, no guards) graded by the same checks; orchestration workloads (a lead and workers); deterministic fault injection (temporary provider errors, malformed calls); record and replay of a live run; a trace per run | `ark-bench run --harness naive`, `--faults`, `--record`/`--replay`, `--traces`; simulated numbers for both harnesses |
| P1 measure | Live runs of both harnesses on the suite (2 or 3 models, 3 repetitions) | a baseline table in `bench/RESULTS.md`, no harness change |
| P2 improve | One change at a time, each measured. Done: action preloading (default 2); tool choice on small models (preload floor, ready-to-call list, small arrays whole: qwen3:4b 35/42 to 41/42, ahead of the naive loop's 36/42; Haiku not yet re-measured). Done: the planner off by default (its steps caused most of the extra calls on small tasks; qwen3:4b 42/42 at $0.0181 per success, ADR 0008). Next: `parallel` (1.4x naive: a small model calls one item per reply) and `team` (1.4x: each agent pays a final answer). Then step checkpoints (resume a task where it stopped), a model routing policy, tool-result compression, a cost model for delegating, recovery rules per failure class | before/after per change |
| P3 prove | One script and one report: ARK against the naive baseline on the same tasks and models | `bench/RESULTS.md` headline, reproducible |

Folded in from the earlier list:

- **Orchestration workload in `ark-bench`**: P0. ARK Desktop is the second orchestration
  consumer (agents and tasks through the protocol), so no other app is needed for that.
- **Live cache measurement** on Anthropic and OpenAI: P1, when an API key is at hand.
- **Small-model tool choice** (`tool-lookup` and `approval` fail live on qwen3:4b): P2,
  measured against the P1 baseline.

Outside the milestone:

1. **Native window test**: drive the Tauri IPC path in CI, or document why it cannot be.
2. **API trim at 0.7** (breaking, so with the milestone's release): public items no caller
   uses (`Interaction::ark_mut`, `App::ark_builder`, `Harness::budget_tokens`,
   `budget::WorkingSet::round_count`, `Registry::from_adapters`, `Store::get_artifact`,
   `Store::get_session`, `ActionDescriptor::with_output_schema`, `SurfaceRef::with_title`,
   `PendingApproval::is_closed`, `Ark::ask_simple`). Remove, or keep with a reason.
3. **ark-cli in its own repo**: not before 0.8. First orchestration control in
   `ark-interact`, then one or two releases without a breaking `ark-interact` change.

## Worth investigating

| Idea | Why |
|---|---|
| A second hosted model in the live benchmark | all live numbers come from one 4B model |
| Working state placement vs time to first token | the 137 to 259 ms rise above |
| Drop the `list_actions` / `record_note` handlers | no longer offered; kept so an older prompt still works |
| Persisted approval grants per project | asked for once per process today |

## Benchmark and optimization opportunities

| Opportunity | Measure with |
|---|---|
| Cost per success on a hosted model with real cache reads | live `ark-bench` with an API key |
| Fewer reasoning steps per task (228 of 257 live calls are `reason`) | live, `Model calls / run` |
| Uncached input in workloads that never record state (+2% simulated) | simulated, `uncached` |

Already measured and rejected (do not retry without a new reason): action summaries in the
index, planner on a smaller model. (Planner off was rejected in M5 and adopted after P2: ADR 0008.) See `bench/RESULTS.md`.

## Notable changes

| Version | Change |
|---|---|
| 0.4 | `ark-bench`; cheaper request layout and smaller tool set (cost per success -65% live); timeouts, retries, crash recovery, flat memory; release scripts |
| 0.3 | `ark-interact` shared by both front-ends; `ark-ui` window and browser workspace; steering |
| 0.2 | `ark-cli` interactive shell; streaming, approvals, cancel, sessions |
| 0.1 | extraction from SecRes; runtime, SDK, MCP bridge, two example apps; SecRes migrated onto ARK |

## Breaking changes and migrations

Upgrade steps: [`docs/RELEASING.md`](docs/RELEASING.md#upgrading-an-ark-app). What to change
per version: the `Breaking` sections of [`CHANGELOG.md`](CHANGELOG.md).

| From | To | Main changes for an app |
|---|---|---|
| 0.3 | 0.4 | `ActionDescriptor::reusable: bool` becomes `reuse: Reuse`; `Selection` marks; new event variants `Retry`, `Stall` |
| 0.2 | 0.3 | `ArkCli::new(app)` with an `ark_interact::App`; `Command::run` returns `Reply`; `ToolRenderer` becomes `ToolLabeler` |
| 0.1 | 0.2 | `HarnessEvent` owned and serializable with new variants |
