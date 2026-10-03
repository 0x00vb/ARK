# Roadmap

Where ARK is today and what is worth doing next. Keep it short: update it when a release
ships, a limit is found or fixed, or a priority changes. Numbers live in
[`bench/RESULTS.md`](bench/RESULTS.md); per-version detail in [`CHANGELOG.md`](CHANGELOG.md).

## Now

| | |
|---|---|
| Stable release | **v0.5.0** (2026-10-01) |
| `main` | v0.5.0 plus orchestration, managed vendor sessions, rule sets and diagrams (unreleased; breaking, so the next release is 0.6.0) |
| Consumers | SecRes (pins v0.5.0), ARK Desktop (`ark-desktop` repo; needs 0.6.0), `examples/minimal-agent`, `examples/writing-agent`, `examples/agent-team` |
| Benchmark | live qwen3:4b: success 0.85, cost per successful task $0.0298 (v0.4.0, `bench/RESULTS.md`) |
| Phase | functionally complete for now. No milestone in progress. |

## State

| Area | Status |
|---|---|
| Runtime (`ark-core` to `ark-sdk`) | stable seams, bounded context, retries, crash recovery, flat memory over 2,000 turns |
| Orchestration (`ark-orchestrate`) | new: agents, tasks, delegation, capacity-aware scheduling, a graph and event stream; tested with scripted models, run live on Claude and ChatGPT subscriptions |
| Models (`ark-model`) | Ollama, Anthropic and OpenAI APIs, Claude and ChatGPT subscriptions through their CLIs, mock |
| Interaction (`ark-interact`) | commands, reports, activity, traces, context and usage views shared by both front-ends |
| Terminal (`ark-cli`) | interactive shell, approvals, steering, resume, headless `-p` |
| Workspace (`ark-ui`) | Tauri window and loopback browser page; page tested in WebKit; orchestration control in the protocol, app views in the main column |
| Measurement (`ark-bench`) | 13 generic workloads, simulated and live, regression gate in `scripts/ci.sh` |
| Release | `scripts/ci.sh`, `scripts/verify-release.sh`, immutable tags, two remotes |

## Known limitations

| Limitation | Where it shows |
|---|---|
| Tauri IPC path of the native window has no automated test (the page is tested in WebKit; SecRes checks its bundle through the browser transport) | `ark-ui` |
| Anthropic and OpenAI prompt-cache reads are not measured against a live provider, only by unit tests and the simulated cache model | `ark-model`, `ark-bench` |
| Every live benchmark uses one 4B local model (qwen3:4b-instruct through Ollama) | `bench/RESULTS.md` |
| With qwen3:4b, `tool-lookup` and `approval` fail 3 of 3: the model picks a plausible neighbor tool, or files the note in its own working state | `bench/RESULTS.md` |
| Time to first token rose 137 to 259 ms (median) with the working state last in the request; cause not proven | `bench/RESULTS.md` |
| Subscription providers (`claude`, `codex`) start a process per call and do not stream. A plain `Ark` still resends the request on every call; only orchestrated agents use managed sessions | `ark-model`, `ark-sdk` |
| `claude:haiku` tries host tools natively before answering in the structured form | `bench/RESULTS.md` |
| Orchestration is measured on one program, one run per setting | `bench/RESULTS.md` |
| The window has no graph view of its own; ARK Desktop draws one from `graph` and `orch` events through `ark.view` | `ark-ui` |
| Agents created from a window use the app's base profile; a custom `AgentProfile` needs code | `ark-orchestrate`, `ark-ui` |
| Approval grants are not persisted across processes | CLI, UI |
| One `Host` serves one live session; two browser tabs share it | `ark-ui` |
| `/context` token numbers are estimates; provider numbers are in `/trace` | `ark-interact` |
| Not on crates.io; consumers pin a git tag | release |

## Next

In order. Each item should show its value in a benchmark or a consumer before it grows.

1. **Orchestration in a consumer**: run a real multi-agent task through SecRes or the writing
   agent, then decide on a managed session for the single-agent `Ark` and a graph panel in
   the window. Add an orchestration workload to `ark-bench` (several runs, cost per success).
2. **Live cache measurement** on Anthropic and OpenAI: run the generic suite with a key and
   record `cached_tokens` and cost per success next to the simulated estimate.
3. **Small-model tool choice**: find out why `tool-lookup` and `approval` fail live, and fix
   it in the request (descriptions, index layout), not with a bigger model. Measure live.
4. **Native window test**: drive the Tauri IPC path in CI, or document why it cannot be.
5. **API trim at the next minor**: public items no caller uses (`Interaction::ark_mut`,
   `App::ark_builder`, `Harness::budget_tokens`, `budget::WorkingSet::round_count`,
   `Registry::from_adapters`, `Store::get_artifact`, `Store::get_session`,
   `ActionDescriptor::with_output_schema`, `SurfaceRef::with_title`,
   `PendingApproval::is_closed`, `Ark::ask_simple`). Remove or keep with a reason; it breaks
   semver, so it waits for 0.5.

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
index, planner on a smaller model, planner off. See `bench/RESULTS.md`.

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
