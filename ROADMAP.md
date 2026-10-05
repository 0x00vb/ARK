# Roadmap

Where ARK is today and what is worth doing next. Keep it short: update it when a release
ships, a limit is found or fixed, or a priority changes. Numbers live in
[`bench/RESULTS.md`](bench/RESULTS.md); per-version detail in [`CHANGELOG.md`](CHANGELOG.md).

## Now

| | |
|---|---|
| Stable release | **v0.10.0** (2026-10-05): ARK without a window (several live chats, schedules that post into the chat that asked, keep running), a runtime other machines, the terminal and the window connect to (ADR 0016, 0024, 0029, 0030), the browser (ADR 0015), the Computer (ADR 0018), Files edited in place (ADR 0019), Run code contained on macOS, Linux and Windows (ADR 0020, 0032), data work (ADR 0033 to 0035), the project's board (ADR 0023), optional ARK Accounts (ADR 0026). v0.9.0 brought Claude Code and Codex first-class |
| `main` | v0.10.0 |
| Consumers | SecRes (pins v0.8.0), ARK Desktop (`ark-desktop` repo; pins v0.10.0), `examples/minimal-agent`, `examples/writing-agent`, `examples/agent-team` |
| Benchmark | live qwen3:4b, 14 workloads x 3: ARK 42/42 at $0.0181 per success, the naive loop 36/42 at $0.0268 (v0.7.0, `bench/RESULTS.md`) |
| Phase | **Measured runtime** closed at v0.7.0 (P0 to P2, measured live on qwen3:4b). What it left open is deferred: see "Deferred: measured runtime". Next: prove ARK as a product (see Next). |

## State

| Area | Status |
|---|---|
| Runtime (`ark-core` to `ark-sdk`) | stable seams, bounded context, retries, crash recovery, flat memory over 2,000 turns |
| Orchestration (`ark-orchestrate`) | new: agents, tasks, delegation, capacity-aware scheduling, a graph and event stream; tested with scripted models, run live on Claude and ChatGPT subscriptions |
| Models (`ark-model`) | Ollama, Anthropic and OpenAI APIs, Claude and ChatGPT subscriptions through their CLIs, mock |
| Interaction (`ark-interact`) | commands, reports, activity, traces, context and usage views shared by both front-ends |
| Terminal (`ark-cli`) | interactive shell, approvals, steering, resume, headless `-p` |
| Workspace (`ark-ui`) | Tauri window and loopback browser page; page tested in WebKit; orchestration control in the protocol, app views in the main column |
| Measurement (`ark-bench`) | 16 generic workloads (one orchestrated team), simulated and live, a naive baseline loop, faults, record and replay; regression gate in `scripts/ci.sh` |
| Release | `scripts/ci.sh`, `scripts/verify-release.sh`, immutable tags, two remotes |

## Known limitations

| Limitation | Where it shows |
|---|---|
| Tauri IPC path of the native window has no automated test (the page is tested in WebKit; SecRes checks its bundle through the browser transport) | `ark-ui` |
| Anthropic and OpenAI prompt-cache reads are not measured against a live provider, only by unit tests and the simulated cache model | `ark-model`, `ark-bench` |
| Every live benchmark uses one 4B local model (qwen3:4b-instruct through Ollama) | `bench/RESULTS.md` |
| With qwen3:4b, `tool-lookup` and `approval` fail 3 of 3: the model picks a plausible neighbor tool, or files the note in its own working state | `bench/RESULTS.md` |
| Time to first token rose 137 to 259 ms (median) with the working state last in the request; cause not proven | `bench/RESULTS.md` |
| A subscription session's first call still pays the CLI's start-up; a Codex thread also starts the MCP servers of the person's Codex setup (about 4 s and 15k tokens measured), since `codex app-server` has no `--ignore-user-config` | `ark-model`, ADR 0012 |
| `codex app-server` is experimental in Codex; a protocol change falls back to `codex exec` (a process per call, no streaming) | `ark-model` |
| `claude:haiku` sometimes tries host tools natively; it recovered in 6 of 6 tasks in the last runs, and a check asks again when it gives up, but that check has only a scripted test | `bench/RESULTS.md` |
| Orchestration is measured on one program, three runs per setting; managed sessions cut uncached input but not wall time | `bench/RESULTS.md` |
| The window has no graph view of its own; ARK Desktop draws one from `graph` and `orch` events through `ark.view` | `ark-ui` |
| Agents created from a window use the app's base profile; a custom `AgentProfile` needs code | `ark-orchestrate`, `ark-ui` |
| Windows on one `Host` show the same chat; other chats work in the background (ADR 0017) but one window cannot show two chats side by side | `ark-ui` |
| The Computer reads apps through macOS accessibility only; apps that expose little (canvases, games, remote desktops) show little, and no other platform has a screen yet | `ark-computer`, ADR 0018 |
| `/context` token numbers are estimates; provider numbers are in `/trace` | `ark-interact` |
| On `workbooks`, `claude:haiku` (0/2) and qwen3.5:27b (0/2) keep orders exported twice even when told each order counts once, and Haiku does not read the `data-analysis` skill there; Sonnet and Codex pass | `bench/RESULTS.md` |
| A run's memory and CPU are not limited: a program that reads a very large file can use most of the machine's memory | `ark-exec`, ADR 0032, 0033 |
| Run code on Linux and Windows is checked by lint and unit tests here, not yet run on a machine of each | `ark-exec`, ADR 0032 |
| Not on crates.io; consumers pin a git tag | release |

## Next

The runtime works and is measured. The next phase proves ARK as a product. Steps run in
this order; each ends when its "Done means" holds.

The product test: **a non-expert installs ARK, picks the model or provider they already
have, creates or uses an agent or team, gives it the capabilities it needs, and gets real
work done without knowing what MCP, skills, harnesses, providers or context management
are.** Advanced controls stay, behind simple primitives (progressive disclosure).

| # | Step | Done means |
|---|---|---|
| 1 | **v0.8.0** (done 2026-10-03): the API trim that was planned for 0.7 and never shipped (below), cleanup and stabilization, stale docs fixed. Since v0.7.0 on `main`: MCP servers added while ARK runs, skills read on demand (ADR 0009) | v0.8.0 tagged; SecRes upgraded from v0.5.0 and running a real task on it. Done: SecRes live on qwen3:4b 12/15 against 10/15 on v0.4.1, cost per success -49% (`workbench/bench/README.md`); ark-desktop on v0.8.0 with the web on |
| 2 | **Claude Code and Codex first-class** (done in v0.9.0, 2026-10-04): a running CLI per vendor session, streamed answers, cancel that drops the vendor session, usage limits that say when they reset (ADR 0012); subscription usage as `ark_model::subscription_usage` and the window method `subscription_usage`; agents follow the session's model (ADR 0014). A longer live task passed on `claude:sonnet`, `claude:haiku` and `codex:default` (`bench/RESULTS.md`). Later, not in this step: start the CLI before the first call, deeper provider telemetry | no process per call; output streams; cancel stops the work; failures say what to do; a long task feels as good as the vendor CLI |
| 3 | **ARK Desktop public**: signed and notarized release, aiark.dev live, a real download flow, onboarding that asks almost nothing | a non-expert passes the product test above on a clean machine |
| 4 | **Prove the advantage** on hosted models and real workflows: the hosted-model runs and P3 (ARK against the naive loop). Metrics: task success, tokens, cost, latency, retries, context growth. Time-boxed: enough evidence to state the thesis, not an open milestone | headline numbers in `bench/RESULTS.md`, reproducible, usable on aiark.dev |
| 5 | **Persistent, headless ARK**: runs without a window. Several sessions, persistence, real auth, saved approval and policy rules, background execution, scheduled tasks. A runtime primitive, not Desktop infrastructure. On `main`, unreleased: a runtime clients connect to with saved access tokens (ADR 0016, [`docs/REMOTE.md`](docs/REMOTE.md)); several live chats, orchestrators that outlive chats, and schedules (ADR 0017; a scheduled run ran live with no window). Saved approval rules are C3's grants per project. The terminal is a client too: `app --connect URL` (ADR 0024). A schedule's runs post their results into the chat that asked (ADR 0025). Left: sign-in and the signed-in browser on a remote runtime, notifications when a background question waits (the client's) | Desktop and the CLI connect to a running ARK as clients; a scheduled task runs with no window open |
| 6 | **ARK Cloud**: managed ARK on the same runtime, not a second one. Cloud adds hosted execution, auth, persistence, remote capabilities (MCP, connectors), scheduling, observability, billing. Plan and phases: `../ark-cloud/docs/CLOUD.md`. A cloud project is the app's binary in one machine per workspace; ARK models are an OpenAI-compatible gateway in ark-cloud; money stays out of the runtime. Started: ARK Accounts (ADR 0026), the optional `ark-account` client and the account service in `ark-cloud`; ARK Desktop signs in from Settings › Account. What ARK itself needs: named OpenAI-compatible endpoints (A1, done, ADR 0027), an out-of-credit error front-ends act on (A2, done, ADR 0027), a file-backed `Secrets` for servers (A3, done, ADR 0028), the window opening a runtime elsewhere and coming back (A6, done, ADR 0029), Run code on Linux (A4, done with bubblewrap, ADR 0032; needs a run on a Linux machine) | Cloud runs the step 5 runtime unchanged; ARK still self-hosts with no cloud dependency |

Throughout: one runtime, many clients. Desktop, CLI, Cloud and domain products use the same
ARK capabilities; prefer a reusable primitive in the right layer over a feature-specific
implementation.

API trim for v0.8.0 (breaking), done: removed `Interaction::ark_mut`, `Harness::budget_tokens`,
`budget::WorkingSet::round_count`, `Registry::from_adapters`, `PendingApproval::is_closed`,
`Ark::ask_simple`. Kept, each the missing half of a pair: `App::ark_builder` (a full
`ArkBuilder` built at open time), `Store::get_artifact` and `Store::get_session` (read by id
after `put_*`), `ActionDescriptor::with_output_schema` and `SurfaceRef::with_title` (builder
twins of `with_input_schema` and `new`).

### Capabilities

What "gives it the capabilities it needs" means in the product test: agents and teams use the
web, a browser, mail, calendars and services, and later the desktop, with no setup that
names MCP, connectors or scopes. One rule for all of it: **a capability is a source of
actions behind the existing seams** (`Adapter`, `Registry`, `RiskTier` and the
`ApprovalGate`, `AgentSpec::tools`, the acquisition seam). There is no second tool or
permission system. Service details stay in data at the edge.

| # | Step | Builds on | Done means |
|---|---|---|---|
| C1 | **Web**: `web.search`, `web.fetch` (ADR 0010) | `Adapter`, `Provider` | done on `main`: every model searches; an agent reads pages; the `web` workload |
| C2 | **Connections** (ADR 0011): ARK owns the account. `ark-connect`: a catalog (data) of providers and capabilities, OAuth with PKCE, tokens in the OS keychain, revoke on disconnect, neutral tool names. One capability list in `ark-interact` (`/connect`, Settings › Capabilities; Tool servers stays the advanced view). Done on `main`: the foundation, Mail (send) and Calendar on Google, tested against a fake provider in WebKit. Left: ARK's Google client and Google's review (`docs/GOOGLE.md`), a live run with a real account. Later, on the same accounts: remote MCP servers (`url`) and third-party sign-in | `ToolSource`, `Adapter`, `ApprovalGate` | Mail and Calendar connect from the list in both front-ends with a real Google account; reading runs on its own, sending asks |
| C3 | **Task-first access** (ADR 0013): an agent that lacks a capability asks for it through the acquisition seam, backed by the connections; the person sees "ARK needs this to finish: Connect Google…" with Connect or Not now, signs in, and the turn continues with the new tools. Done on `main`, tested in a terminal and in WebKit against a fake provider. Grants kept per project: done ("Always in this project", `/grants`, Settings › Permissions) | `AcquisitionProvider`, `ApprovalGate` | "research these companies and email me the report" works from an empty setup with one Connect click and one send approval |
| C4 | **Browser and shared resources** (ADR 0015): native, over the DevTools protocol of the installed Chrome, Edge or Brave (the owner's choice over Playwright MCP). Each agent its own fresh context; the signed-in browser one agent at a time, the others wait. Per-agent tools (web, browser) share one seam, `PerAgent`. Done on `main`, tested against the real Chrome and dogfooded on qwen3:4b; `agents.create` names the capabilities a new agent gets | `Adapter`, `PerAgent`, `ApprovalGate` | two agents browse at once without collisions; a third waits for the signed-in one |
| C5 | **Desktop / computer use** (ADR 0018): `ark-computer`. An app's window as text and numbered controls through macOS accessibility, so any model can use it; one agent holds the screen, the others wait; every action but listing apps asks; actions wait for the person to pause; "‹who› is using your computer · Stop". On `main`, unreleased, tested against a fake screen and read-only against the real one; listing apps dogfooded on qwen3:4b. Left: screenshots for models that read images, other platforms, grants per app, a live acting run on a real app | C4 | one agent operates an app while others work; nothing else touches the screen |
| C6 | **Files** (ADR 0019): `ark-files`. Spreadsheets (`.xlsx`, `.csv`) and documents (`.docx`) as shapes the agent reads by cell and block and edits in place, keeping the rest of the file; the window opens the same file as a grid or a page the person edits; one `Edit` list for both; a version guard. On `main`, unreleased, tested in WebKit and with files from openpyxl and TextEdit; dogfooded in the writing agent on qwen3:4b (reads, adds a sheet) and qwen3.5:27b (the whole loop). Left: ARK Desktop on it, a live workload. PDFs (read as text) and images open as themselves (C7) | `Adapter`, `present`, `Focus` | a person asks for a change to a spreadsheet, edits the result in ARK, and the agent continues from their version |
| C7 | **Outputs and running code** (ADR 0020, ADR 0021): `ark-exec`'s `code.run` runs a program (Python first, through `uv`) in the project folder inside a sandbox: no network, writes only there. Every file it writes is an artifact with a path; `ark-files` decides its kind once (Markdown, PDF and image added); the window opens each kind in its view; the tool's description says what ARK shows, from the formats. On `main`, tested against the real sandbox and in WebKit. Contained on macOS, Linux (bubblewrap) and Windows (AppContainer), ADR 0032. Left: ARK Desktop ships `uv`, a run on a Windows and a Linux machine, network as its own permission, packages per project | C6, `ApprovalGate`, `Sniffer` | "analyze this spreadsheet, make a chart and a short report": the spreadsheet, the chart, the table and the report open in ARK without leaving the conversation |
| C8 | **Data work** (ADR 0033): the analyst's work (several workbooks and CSV files, joins, cleaning, totals, charts, a new workbook) with the generic tools, no mode. Python has DuckDB, calamine (`.xls`), xlsxwriter and pyarrow; Files reads every sheet's start and at most 100,000 cells per sheet; the `data-analysis` skill comes with Run code; `code.run` says its `purpose`. Databases are read-only MCP plugins ([`docs/DATA.md`](docs/DATA.md)). The `workbooks` workload: Sonnet and Codex pass every data workload live; Haiku and qwen3.5:27b keep orders exported twice (`bench/RESULTS.md`). Files dropped on the chat go into the project (ADR 0035), and a file a call changed or removed keeps its version before, with Restore and Undo on its card (ADR 0034). Left: memory limits per run, Windows saying Excel holds a workbook open. Not planned: Power BI or other BI connectors until a real user needs one | C6, C7, skills (ADR 0009) | "combine these files, chart the months, give me a clean workbook": right numbers in a new workbook, sources unchanged, an answer a non-programmer reads |

Order: C1 to C3 make a single agent useful; C4 makes teams safe with resources that cannot be
shared; C5 last, as the riskiest and least proven. Overlap with the steps above: step 5's saved
approval rules are C3's grants per project; step 6's remote capabilities are C2's
connections, run where ARK runs.

Placement (the owner, 2026-10-03): the capability foundation is built after v0.8.0, beside
step 2, and before step 3. The first public ARK Desktop ships a few capabilities done well
(Web, Files, Mail, Calendar), not a large set of integrations. It proves one loop: tell ARK
what you want, ARK finds what it needs, you connect or approve it simply, the agent or team
does the work. Google's review of ARK's sign-in may gate step 3 (`docs/GOOGLE.md`).

| Kept out on purpose | Until |
|---|---|
| Remote MCP servers (HTTP/SSE `url` entries) | a consumer needs one; stdio servers only today |
| Installing skills from ARK | never in the runtime: a person runs an installer (`npx skills add`) or copies a folder |
| Preloading a skill's instructions when a task matches it | a benchmark shows it pays (the `skill` workload is the start) |

### Deferred: measured runtime

Left open when the milestone closed at v0.7.0. Step 4 of Next takes the Haiku check and
P3; the rest is not scheduled. Pick one up only with a before/after in `bench/RESULTS.md`.

| Item | What is known |
|---|---|
| Haiku check of the P2 defaults | preloading, the floor, the ready list, small arrays whole and planner off are measured live on qwen3:4b only. P1's Haiku runs (`bench/p1/`) are the baseline: 42/42 at $0.0703 per success |
| `parallel` overhead | 1.42x the naive loop per success on qwen3:4b: the model calls one item per reply, where the naive loop gets all three in one |
| `team` overhead | 1.37x: the lead and each worker pay a final answer; a lead can delegate in two replies instead of one |
| Output tokens | ARK answers use 2 to 4 times naive's output tokens (the instructions ask for facts, inference and a next step); output is billed at 5x input |
| `tool-lookup` | 1.16x: one extra call before the conversion |
| P2 items not started | step checkpoints, a model routing policy, tool-result compression, a cost model for delegating, recovery rules per failure class |
| P3 prove | one script and one report, ARK against the naive loop on the same tasks and models |

### Milestone: a measured runtime (0.7, closed)

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
| P2 improve | One change at a time, each measured. Done: action preloading (default 2); tool choice on small models (preload floor, ready-to-call list, small arrays whole: qwen3:4b 35/42 to 41/42, ahead of the naive loop's 36/42; Haiku not yet re-measured). Done: the planner off by default (its steps caused most of the extra calls on small tasks; qwen3:4b 42/42 at $0.0181 per success, ADR 0008). The rest is deferred (above) | before/after per change |
| P3 prove | Deferred (above) | `bench/RESULTS.md` headline, reproducible |

Folded in from the earlier list:

- **Orchestration workload in `ark-bench`**: P0. ARK Desktop is the second orchestration
  consumer (agents and tasks through the protocol), so no other app is needed for that.
- **Live cache measurement** on Anthropic and OpenAI: P1, when an API key is at hand.
- **Small-model tool choice** (`tool-lookup` and `approval` fail live on qwen3:4b): P2,
  measured against the P1 baseline.

Outside the milestone:

1. **Native window test**: drive the Tauri IPC path in CI, or document why it cannot be.
2. **API trim at 0.7**: did not ship with 0.7. Moved to v0.8.0 (Next, step 1).
3. **ark-cli in its own repo**: not before 0.8. First orchestration control in
   `ark-interact`, then one or two releases without a breaking `ark-interact` change.

## Worth investigating

| Idea | Why |
|---|---|
| A second hosted model in the live benchmark | all live numbers come from one 4B model |
| Working state placement vs time to first token | the 137 to 259 ms rise above |
| Drop the `list_actions` / `record_note` handlers | no longer offered; kept so an older prompt still works |

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
