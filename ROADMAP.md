# Roadmap

Where ARK is today and what is worth doing next. Keep it short: update it when a release
ships, a limit is found or fixed, or a priority changes. Numbers live in
[`bench/RESULTS.md`](bench/RESULTS.md); per-version detail in [`CHANGELOG.md`](CHANGELOG.md).

## Now

| | |
|---|---|
| Stable release | **v0.19.0** (2026-10-10): external protocol v1.1 and project Terminal without a chat. Earlier releases are in `CHANGELOG.md`. |
| `main` | v0.19.0 release tree. Generated schema and Swift models, durable native mutations, strict bearer routes and project Terminal passed the full release gate. |
| Consumers | ARK Desktop (`ark-desktop` repo; v0.6.4 still pins ARK v0.18.3), `examples/minimal-agent`, `examples/writing-agent`, `examples/agent-team` |
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
| Tauri IPC path of the native window has no automated test (the page is tested in WebKit; an app's bundle can be checked through the browser transport) | `ark-ui` |
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
| The Computer is macOS only. Apps that expose little to accessibility (Spotify, canvases, games) need a model that reads images (`computer.see`). A message app needs enough accessibility evidence to prove its recipient, composer and outgoing direction; otherwise a send stays unconfirmed. Every action asks each time, so nothing on the screen runs unattended | `ark-computer`, ADR 0018, 0042, 0056 |
| `/context` token numbers are estimates; provider numbers are in `/trace` | `ark-interact` |
| On `workbooks`, `claude:haiku` (0/2) and qwen3.5:27b (0/2) keep orders exported twice even when told each order counts once, and Haiku does not read the `data-analysis` skill there; Sonnet and Codex pass | `bench/RESULTS.md` |
| A run's memory and CPU are not limited: a program that reads a very large file can use most of the machine's memory | `ark-exec`, ADR 0032, 0033 |
| Run code on Linux and Windows is checked by lint and unit tests here, not yet run on a machine of each | `ark-exec`, ADR 0032 |
| ARK has never run on Windows. `ark-os` (ADR 0022) is on `ci/windows` only; on `main`, other processes look dead to the runtime, links open through `cmd`, and stops other than Ctrl+C get no grace | `ark-desktop/docs/WINDOWS.md` |
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
| 5 | **Persistent, headless ARK**: runs without a window. Several sessions, persistence, real auth, saved approval and policy rules, background execution, scheduled tasks. A runtime primitive, not Desktop infrastructure. On `main`, unreleased: a runtime clients connect to with saved access tokens (ADR 0016, [`docs/REMOTE.md`](docs/REMOTE.md)); several live chats, orchestrators that outlive chats, and schedules (ADR 0017; a scheduled run ran live with no window). Saved approval rules are C3's grants per project. The terminal is a client too: `app --connect URL` (ADR 0024). A schedule's runs post their results into the chat that asked (ADR 0025). W1 to W3 below are on `main` (ADR 0037): questions kept on tasks, a restart continues work, each agent's step in the graph. Left: sign-in and the signed-in browser on a remote runtime, notifications when a background question waits (the client's; to a phone in ADR 0043 step 3) | Desktop and the CLI connect to a running ARK as clients; a scheduled task runs with no window open |
| 6 | **ARK Cloud**: managed ARK on the same runtime, not a second one. Cloud adds hosted execution, auth, persistence, remote capabilities (MCP, connectors), scheduling, observability, billing. Plan and phases: `../ark-cloud/docs/CLOUD.md`. A cloud project is the app's binary in one machine per workspace; ARK models are an OpenAI-compatible gateway in ark-cloud; money stays out of the runtime. Started: ARK Accounts (ADR 0026), the optional `ark-account` client and the account service in `ark-cloud`; ARK Desktop signs in from Settings › Account. What ARK itself needs: named OpenAI-compatible endpoints (A1, done, ADR 0027), an out-of-credit error front-ends act on (A2, done, ADR 0027), a file-backed `Secrets` for servers (A3, done, ADR 0028), the window opening a runtime elsewhere and coming back (A6, done, ADR 0029), Run code on Linux (A4, done with bubblewrap, ADR 0032; needs a run on a Linux machine) | Cloud runs the step 5 runtime unchanged; ARK still self-hosts with no cloud dependency |

Throughout: ARK is where agents work together: the control plane and runtime for agent work, with ARK's own agent first and other agents as workers (ADR 0043); coding is one workload (ADR 0036). One runtime, many clients. Desktop, CLI, Cloud and domain products use the same
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
| C5 | **Desktop / computer use** (ADR 0018): `ark-computer`. An app's window as text and numbered controls through macOS accessibility, so any model can use it; one agent holds the screen, the others wait; every action but listing apps asks; actions wait for the person to pause; "‹who› is using your computer · Stop". Released in v0.10.0. ADR 0042 (2026-10-06): rows and items, only what is in sight, menus, scrolling, the pointer, typing as keys, pictures of one window to models that read images (Claude, OpenAI, Codex); tried by hand on WhatsApp, Finder, Spotify. ADR 0056 (2026-10-09): draft, send and confirmation are separate; a durable safety record blocks ambiguous retries; activity and targets are visible outside ARK. Left: grants per app, a native controlled self-chat run, other platforms | C4 | one agent operates an app while others work; nothing else touches the screen |
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

Transact (proposed, 2026-10-07, not approved): an action that costs money is approved with its
amount (ask each time, never covered by "for this session" or "always"), counted against a
limit the person made (`Budget.max_spend`, standing limits), and recorded with the merchant's
order and a receipt. Shopping is a capability crate on top (`ark-commerce`: UCP and ACP
clients, checkouts the person pays at the merchant, or a wallet seam with a one-time
credential); ARK never holds funds or card data and names no merchant, protocol or PSP in the
runtime. Plan, risks and order (T0 to T5): `../ark-cloud/docs/COMMERCE.md`. Trigger: the
owner's go, after ARK Desktop is public.

| Kept out on purpose | Until |
|---|---|
| Remote MCP servers (HTTP/SSE `url` entries) | a consumer needs one; stdio servers only today |
| Installing skills from ARK | never in the runtime: a person runs an installer (`npx skills add`) or copies a folder |
| Preloading a skill's instructions when a task matches it | a benchmark shows it pays (the `skill` workload is the start) |

### Work runtime (ADR 0036)

ARK is a runtime people give work and outcomes to; several agents are an implementation detail
the chat decides on. Reviewed 2026-10-05 against coding-agent orchestrators (Orca: parallel
agents in git worktrees, compare and merge, SSH, a phone app; Herdr: a terminal multiplexer
that knows each agent's state, sessions that survive detach, a socket agents drive each other
through). Extended the same day to Tern and OMP (below). ARK does not copy them; this is the
floor it must not be under.

| Area | ARK today | Verdict |
|---|---|---|
| Work that persists | the runtime runs without a window (ADR 0016, 0017), schedules, keep running; a restart continues open work, keeps questions, never repeats a change (ADR 0037); a stop (SIGTERM, quit) leaves work to continue and handed-off results reach their chat once (ADR 0038); a chat's own turn keeps its progress through a crash, shows where it stopped, and continues on the person's next word without repeating a change or a hand-off (ADR 0048) | at par since W1, W2, W5, W6 (v0.11.0); the chat's turn since ADR 0048 |
| Agent state at a glance | `AgentState`, `TaskState`, `waiting_on`, a task's `question`, each agent's `step` worded once for every client (ADR 0037) | at par since W1, W3 (v0.11.0) |
| Parallel work | many agents, capacity-aware scheduling, dependencies, up to 4 delegations per reply; one task at a time per agent | at par. No "same task N ways, compare": can wait |
| Isolation on shared files | one folder per project; version guard, versions before (ADR 0034) | enough for now. A copy per task when a workload shows conflicts |
| Delegation | by brief, typed, traced, budgets (ADR 0005, 0007); the chat creates agents. Chat-created agents cannot delegate; the chat's `agents.delegate` takes no inputs or `after` | ahead in structure; gaps can wait for the hosted-model board workload |
| Local, remote, cloud | one binary and protocol everywhere (ADR 0016, 0024, 0029, 0030), Cloud machines on it | ahead |
| Observability | stored `OrchEvent`s, `graph()`, `task_trace`, usage and cost per task | ahead |
| Extension | `Adapter`, stdio MCP, skills, connections; the protocol is JSON over HTTP + SSE | gap: remote MCP servers (most business apps ship one). Vendor agents as workers: see "Agent harnesses as workers" below (ADR 0039) |
| Low friction | chat first, no forms, Connect in one click, approvals | ahead for non-programmers; the gap is shipping Desktop (step 3) |
| Non-code work | Files (sheets, documents), Run code, data work, browser, Computer, mail, calendar, board, artifacts by kind | ahead; nothing in the runtime assumes code |

Fix before Cloud depends on it. Each is small and in an existing seam; none adds a work type.
W1 to W3, W5 and W6 shipped in v0.11.0 (ADR 0037, 0038); W4 is done on ARK
Desktop's `main`, which pins v0.11.0. With these, the
runtime has no known blocker for Cloud running work nobody watches; what is left is checking it
on a Cloud machine (ark-cloud, Phase 2) and Cloud's own notifications.

| # | Step | Done means |
|---|---|---|
| W1 | **A question for the person is a stored record.** The pending request (what, which task or chat, choices) is saved by the runtime, not held as a `oneshot` in `ark_ui::Host`; the task's `waiting_on` says it waits for the person; `graph()` and the board read it; any client answers it; it survives a restart (W2 resumes the task when it is answered) | a scheduled run asks, the process restarts, the terminal (`--connect`) answers, the task finishes |
| W2 | **A restart resumes work.** Pending and queued tasks stay queued; suspended ones stay suspended; running and waiting ones resume from the agent's working state, as `resume` does, and a state-changing call that was in flight is not repeated without asking. A task that cannot resume posts to its chat (ADR 0025) | kill the process during a team task; on start the work continues, with no duplicate state-changing call |
| W3 | **What each agent is doing is in the graph.** Its current step as `ark-interact` words it, and what it waits for (a task, capacity, the person), so `/agents` and every client show the same | the terminal's `/agents` and the window show the same line for each agent |
| W4 | **ARK Desktop reads, not derives** (ark-desktop): the team tree from `graph.edges`, "doing now" from W3, one path for `@agent`, `setup::keychain` replaced by `ark_connect`'s | nothing in `ui/` computes runtime state (ark-desktop `docs/ARCHITECTURE.md`, The boundary, audited) |

Found while building W1 to W3; needed before Cloud runs work nobody watches (Phase 2). Both done
(ADR 0038):

| # | Step | Done means |
|---|---|---|
| W5 (done) | **Work the chat hands off reports back to it.** `agents.delegate` sets `report_to` to the chat (as `agents.schedule` does), so a result reaches the conversation even when the turn that waited for it is gone (a restart) or did not wait (`wait: false`); a waited-for result is not posted twice | the chat delegates, the process restarts, the result appears in that chat |
| W6 (done) | **A stop is graceful.** On SIGTERM (what a deploy or a machine move sends) the runtime takes no new work, lets a running change finish within a grace period, saves running tasks to continue, and exits; the next start continues them with nothing cut short | a deploy during a change: the change runs once and the task finishes, with no "cut short" failure |

#### Reviewed against Tern and OMP (2026-10-05)

Both come from Stencil Labs. OMP (Oh My Pi, MIT, a fork of Pi, about 34k stars, several releases
a day) is a coding-agent harness. It has 31 tools, among them LSP, a DAP debugger, AST edits and
hash-anchored edits. It also has 60+ providers with per-role models, subagents with filesystem
clones, a compaction cascade, remote MCP with OAuth, TypeScript extensions, an NDJSON RPC with
host tools and SDKs in four languages, and a native ACP server. Its default approval mode runs
everything and it has no OS sandbox. Tern (closed source, closed beta, 0.4.x) is a native
terminal and workspace. A daemon owns the panes, and the desktop, iOS, a web tab and remote
hosts over QUIC mirror them. Its in-band UI protocol (TSP: typed nodes, incremental ops, events
back, ANSI fallback) lets any program draw native views. Luau plugins add lenses over command
output, blocks and an agents API. Sources and detail: ADR 0039 › Context.

| Area | Tern / OMP | ARK | Verdict |
|---|---|---|---|
| Durable sessions | Tern: panes outlive windows, scrollback and omp's conversation come back after a reboot. OMP: session files, a tree, resume | work outlives the process: tasks resume, questions are kept, changes are not repeated, results reach their chat (ADR 0037, 0038) | ahead. They keep sessions; ARK keeps work |
| Clients on one runtime | Tern: desktop, iOS, web, remote, typing mirrored | window, browser page, terminal, remote, Cloud; one protocol | at par. Phone only through the browser (a phone app can wait) |
| Agent state | Tern: working, idle, waiting from OSC 9;4 progress, any agent | each agent's step and what it waits for, stored (W3) | ahead |
| Programs drawing UI | Tern: TSP, 40+ node kinds, forms and events back, any program on the pty | outputs are files with a kind and one view each (ADR 0021); app views through `ark.view`; tool cards worded in `ark-interact` | different layer. Lesson below; no in-band terminal protocol |
| Protocol for others | OMP: RPC with a JSON schema, generated SDKs, ACP. Tern: CLI and NDJSON events against the daemon | JSON over HTTP and SSE; a Rust client; no published schema, no version in `hello` | gap when the first outside client comes (item below) |
| Capabilities | OMP: browser, computer, eval (persistent Python), web, MCP stdio, http and SSE with OAuth | browser, Computer (every action asks), Run code contained on three systems, Files, connections with OAuth, stdio MCP | ahead on safety and non-code work. Behind on remote MCP with sign-in |
| Context efficiency | OMP: compaction cascade, cache-aware pruning, cache warming, big outputs as `artifact://`, code read as outlines | no transcript replay, bounded context, distilled cards, raw output in the store, cost per success measured | ahead in design. Live cache reads still unmeasured (Known limitations) |
| Models | OMP: 60+ providers, roles (smol, slow, plan), fallback chains on quota, subscriptions through OAuth | Ollama, Anthropic, OpenAI, Claude and ChatGPT subscriptions through their CLIs, named OpenAI-compatible endpoints, roles | at par. ARK does not move work to another provider without asking (ADR 0014) |
| Isolation between agents | OMP: a clone per subagent (APFS, reflink, overlay, copy), merged back as a patch | version guard, versions before (ADR 0034) | enough for ARK's own agents. Becomes a floor once a worker writes with its own tools (ADR 0039) |
| Coding tools | OMP: LSP, DAP, AST and hashline edits, git, per-model edit formats | none, on purpose | not ARK's to build: comes with a worker |
| Agent harness as a worker | Tern runs any agent in a pane, omp natively. OMP speaks ACP | vendor CLIs are models only; every task runs in ARK's `Harness` | gap, structural (below) |

Agent harnesses as workers (ADR 0039, Accepted; started by ADR 0043: the ACP spike first, then H1 to H3). ARK's task boundary already fits a worker: a
brief in (ADR 0005), an answer and outputs out, `HarnessEvent`s in between. Five gaps block it.
None is a new work type:

| # | Gap | Generic fix |
|---|---|---|
| H1 (done, step 2) | `Orchestrator::attempt` always builds ARK's `Harness` | an agent's model spec names a worker (`agent:<name>`), resolved through the app's `Workers` catalog; orchestration names no vendor or protocol |
| H2 | ARK is only an MCP client; nothing outside its loop can call its capabilities | serve the agent's filtered `Registry` to the worker: ACP `fs/*` and `terminal/*`, and MCP per task, through the same gate and invocation log |
| H3 (done, step 2) | A worker's permission request has no path to the `ApprovalGate` | ACP `session/request_permission` as an `OnBehalf` request, so it becomes a stored question (W1); a grant names the file or the command |
| H4 (outputs done, step 2) | Outputs, versions before and "never repeat a change" come from ARK's invocation log; a worker's own writes are invisible | the folder as the record (ADR 0043 › A worker's results): listed and copied at the task's first start, compared when it ends. Left: a copy of the folder per task, so two tasks or the person never share one folder while a task waits |
| H5 (done, step 2) | ADR 0004 says vendor sessions are disposable; a worker's transcript is its own | canonical at the task boundary; the worker's session is a cache, loaded after a restart (`session/load`) |

First slice, done (ADR 0043 step 2, 2026-10-07): Claude Code and Codex through their ACP
adapters (`ark-acp`), H1, H3, H5 and H4's outputs, measured against ARK's own agent on two
workloads (`bench/RESULTS.md`: both succeed; ARK's agent is cheaper and faster on them and
stays the default). Writes are not routed through ARK: neither program uses ACP's `fs/*` (the
spike), so the folder is the record. Left: H2 (ARK's capabilities served to a worker: a
workload that needs one), OMP (`omp acp`), a copy of the folder per task. Direct bridges
(Claude Code `stream-json`, Codex `app-server`) only where ACP measurably falls short.

What ARK takes from Tern, without becoming a terminal: (1) the daemon-owns-state, clients-mirror
model, which ARK already has for work; (2) a typed, versioned protocol with a schema, text
fallback and generated clients; (3) a small vocabulary of views a tool or worker can send
(table, key-value, checklist, progress, a form that answers back) that every client draws and
the terminal prints as text. Start from ACP's own `plan` and `diff` content and ARK's kinds. Do
not build a parallel UI system. Trigger: the first worker or MCP tool whose progress shows only
as text.

#### Reviewed against One and herdr (2026-10-07): attention, workers, where work runs (ADR 0043)

herdr (herdr.dev, Apache-2.0, one founder, YC F26) keeps coding agents in persistent terminal
panes across machines and guesses each one's state from its screen. One (getone.one, Mac,
closed) puts an inbox, a toolbar of agent states, a voice router and an iPhone app on top of
herdr. Checked 2026-10-07:

| Signal | Value | Weight |
|---|---|---|
| herdr stars | 42.7k since 2026-03-27; steady growth, one HN front page (July), no one-day jump | real (stargazer sample not possible) |
| herdr Homebrew installs | about 16k a month (formulae.brew.sh) | the hardest to fake |
| herdr issues | about 600 a month from about 90 authors | real use |
| herdr "1.29M installs", "$6M seed" | release downloads (every update counts again); the seed only on herdr's blog | soft |
| One | domain from 2026-08; Pro $12.99 a month, a lifetime ladder; no public reception found | no traction yet |

What users value: which agent needs them, jumping to it, sessions that survive a disconnect,
10+ agents at once. What they complain about: wrong states from screen rules, lag with several
remote machines, the phone, Windows, and "this will fold into Claude Code and Codex". The
vendors are taking the single-vendor part: Claude Code's `claude agents` (local, Claude only,
lost on shutdown) and Remote Control (phone), the Codex app. Nobody has shown people pay for
supervision alone (ARK's business note: `../BUSINESS`).

| Area | One / herdr | ARK | Verdict |
|---|---|---|---|
| Agent state | guessed from the screen | records: task state, kept question, each agent's step (ADR 0037) | ahead; keep it a record |
| What needs me | One's inbox, keys to answer, 2 s undo | questions per task, answered from any client; no single list | gap: one attention list (step 1) |
| Away from the computer | One's iPhone app and relay; herdr has none | the page works on a phone; no notification reaches it | gap: notifications (step 3) |
| Other agents | any agent in a pane | vendor CLIs as models only | gap: workers by protocol (ADR 0039; steps 0, 2) |
| Machines | herdr over SSH; One's connector (Node, systemd with limits, E2E, pairing by QR) | a runtime anywhere with tokens (ADR 0016, 0040); no install or pairing for a person's server | gap: My machine as a location (step 3) |

The order (ADR 0043). Each step starts when the one before holds:

| # | Step | Done means |
|---|---|---|
| 0 (done 2026-10-07) | ACP spike: an ARK task on Claude Code and on Codex through their ACP adapters. Both passed, also across a restart; findings for step 2 in ADR 0043 › Step 0 results | permission asked, kept as a question, answered by ARK, worker continues, result and written file back; a restart with a question open recorded per agent |
| 1 (done 2026-10-07) | Attention in one runtime: questions, failures and handed-off results, any workload, one list in `ark-interact` (`/waiting`, rows in the window's tray), a cleared mark per item. ADR 0043 › Step 1 results. Limit: each read of the list reads every task record (it parses in full only those that can be items); an index when a profile shows it | the same list in both front-ends; one key answers or opens; cleared survives a restart |
| 2 (done 2026-10-07) | One ACP worker, H1 to H3, shaped by step 0 and reassessed after step 1: grants per command or path (ARK stays the authority), outputs from the task's folder, ARK sets model and mode. Claude Code first, Codex as the second catalog entry. ADR 0043 › Step 2 results | its permission request is an item like any other; measured against ARK's agent on two workloads (success, cost, time, times asked) |
| 3 (done 2026-10-07, real devices waiting) | Reach: Web Push from the runtime itself (ADR 0047); My machine as a place in ARK Desktop, paired with a typed code (`token pair`), the deploy with limits. Defer ARK-funded compute, not the distributed architecture: My machine proves the remote path on the person's compute. ADR 0043 › Step 3 results | a VPS paired in one command; its question reaches the phone, is answered there, the task continues |
| 4 (done 2026-10-07, Desktop's link waiting) | Attention across runtimes: an app links a runtime (a name, an address, a token that stays in its process); its items join the list, named, and are answered, cleared or opened on it; a link that does not answer leaves its items marked. One way; each runtime notifies its own devices. ADR 0043 › Step 4 results | when a person has more than one runtime: one list, each item answered on its own runtime |

Not taken: a floating toolbar, a voice router, terminal panes, typing into terminals, ARK's own
model calls on a person's vendor subscription, a paid tier for attention.

Can wait, each until its trigger:

| Item | Trigger |
|---|---|
| A copy of the project folder per task (the generic worktree; a clone or reflink where the filesystem has one, as OMP's `pi-iso`), and running one task N ways to compare | a workload or a user shows agents overwriting each other, asks for best-of-N, or the first worker that writes with its own tools (H4) |
| A done-check on a work item (acceptance criteria the runtime verifies) | the board workload on a hosted model shows items marked done that are not |
| Teams from the chat (`agents.create` with `delegates_to`), inputs and `after` on the chat's `agents.delegate` | the same workload shows the chat needs them |
| Remote MCP servers with sign-in on ARK's connections | after Desktop is public, before ARK builds more services itself: the scalable way to business apps. OMP, Claude Code and Codex all have it: the floor for capabilities |
| The protocol in its own crate, out of `ark-ui`, with a version in `hello`, a published schema and a client outside Rust (OMP and Tern both publish theirs) | the first client outside Rust (a Cloud web app, a phone app). Attention across runtimes (ADR 0043 step 4) is a client of another runtime, but in Rust in `ark-interact`, and it reads an older or newer runtime without a version: a missing method is "too old", items are read one by one |
| A phone app | after the phone's browser with notifications (ADR 0043 step 3) falls short on a remote or Cloud runtime |
| `present` lists the views the host has instead of naming them in `ark-runtime` (today only the window offers `present`, and every window has a Terminal panel) | a host that offers `present` without a terminal or browser |

Kept out: git worktrees, branches, pull requests and terminal multiplexing as runtime concepts.
Watching or controlling agents ARK did not start (screen scraping, other tools' session files,
typing into terminals), ADR 0043.
They belong to a coding app built on ARK. Also kept out after the Tern and OMP review: LSP, debugger,
AST and hash-anchored edit tools, per-model edit formats (a worker brings them); a terminal
emulator, an in-band pty UI protocol, a plugin language in the runtime (Adapters, MCP and skills
stay the extension seams); compaction tricks and mid-stream rule injection without a benchmark
that pays; ARK as an ACP agent for editors (no user asks; ARK's client is the person, not an IDE).

### Windows (proposed, not approved)

ARK Desktop on Windows is planned in [`../ark-desktop/docs/WINDOWS.md`](../ark-desktop/docs/WINDOWS.md)
(analysis of 2026-10-05). The ARK part, waiting for the owner's go:

| # | Work | Why |
|---|---|---|
| X1 | Merge `ci/windows` (`ark-os`, ADR 0022) into `main`; move code written since onto it: stop handling, restore, `FileSecrets`, API key index, uploads, `open_in_browser` | the port is 56 commits behind; new code bypasses the boundary |
| X2 | One runtime per store: the process that opens a store holds a lock on it until it exits; a second one is refused or connects as a client (ADR 0024) | on Windows a second process re-runs tasks the first still runs; removes the pid check everywhere (an ADR if accepted) |
| X3 | Windows stop events in `ark-os` (Ctrl+Break, close, logoff, shutdown) for ADR 0038's safe point | today only Ctrl+C gives grace |
| X4 | `ark-files`: a "file in use" error with plain words, legacy code pages and CSV BOM kept, `~$` lock files hidden; the data skill writes `utf-8-sig` | Windows knowledge workers keep workbooks open in Excel; Windows-1252 text loses accents on edit today |
| X5 | `scripts/ci.sh` runs ADR 0022's `cfg` grep and a Windows cross-lint on the Mac; a self-hosted Windows runner for tests | Windows cannot regress silently while no hosted Windows CI runs |
| X6 | After the Desktop beta: the Computer's `Screen` on UI Automation | the trait fits; about 600 lines, 2 to 3 weeks with the native pass |

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
| Live checks of v0.15.0 | released without live Claude runs by the owner's choice (2026-10-08): ADR 0051 on `claude:sonnet`, a paid-model run of scheduling (ADR 0053), a real-VPS run of ADR 0052 with a subscription. Simulated bench, page tests and `ARK_TEST_SSH=1` container tests only |

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
