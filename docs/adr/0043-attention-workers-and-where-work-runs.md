# 0043. ARK is the control plane and runtime for agent work: one attention list, workers by protocol, three places to run

Status: Accepted (the owner, 2026-10-07)
Date: 2026-10-07

## Context
A review of One (getone.one) and herdr (herdr.dev) on 2026-10-07. Numbers and sources:
ROADMAP › Work runtime › Reviewed against One and herdr.

| Finding | Detail |
|---|---|
| What users value | Seeing which agent needs them, answering it fast, and work that survives a disconnect, from any machine and the phone. herdr: 42.7k stars since March 2026, about 16k Homebrew installs a month, about 600 issues a month |
| How they know state | herdr guesses it with a regex over each terminal screen. One reads other tools' session files, scrapes the screen, and types into terminals with macOS Automation. The most common complaints are wrong states and lag |
| What the vendors absorb | Claude Code has `claude agents` (local, Claude only, lost on shutdown) and Remote Control (phone, Claude only). Codex has its own app. Supervising one vendor's agents is becoming a vendor feature |
| What stays open | Work across vendors and machines, kept as records, that survives restarts and is self-hostable |
| Money | Nobody has shown that people pay for supervision alone. herdr has no price. One has no public traction. vibe-kanban's company reportedly closed |
| What ARK has | Questions kept on tasks and answered from any client, each agent's step, restarts that continue work (ADR 0037, 0038). One runtime and protocol on a laptop, a server or Cloud (ADR 0016, 0029, 0040). Workers accepted, not built (ADR 0039) |

## Decision
ARK is where agents work together: internally, the control plane and runtime for agent work
across agents, projects, machines and later teams. ARK's own agent stays the default and the
most capable. Other agents join as workers inside ARK's work model, never as sessions ARK
watches from outside.

| Question | Decision |
|---|---|
| External agents | Workers through a protocol (ADR 0039, ACP first). ARK starts them, and their state, permission requests and results arrive as protocol messages. ARK never watches work it did not start: no screen scraping, no reading another tool's session files, no typing into terminals |
| Attention | One generic list of what waits for a person, for any kind of work: a question or approval, a failure, a result handed off that nobody has opened. It is a view over records ARK already keeps (`TaskRecord::question`, failed tasks, results that report to a chat, ADR 0025), worded once in `ark-interact`. It is not a second store and has no item type per workload. The only new stored thing is a mark per item: cleared, by whom, when |
| Attention later | An item says who it is for. Today that is always the runtime's owner and the field stays empty. With several people in a runtime, an item can name a person or a group, and clearing it is recorded per person. Across runtimes, a client merges each runtime's list; it never copies items into another store |
| Where work runs | A project's location: **This computer**, **My machine** (a runtime the person runs on their own server or computer, reached with a token, ADR 0016), **ARK Cloud**. One work model, one protocol, the same window. The only difference the person sees is the location, plus a sign-in for Cloud |
| Authority over work | ARK decides what any work may do, also when a worker acts with its own tools. A grant names the concrete thing: a command, a path, a capability, within a task or a project. Never a whole category: "always allow edits" or "always allow commands" are not offered. The person still sees one short question about the concrete action; "always" applies to that action only. This is a constraint on step 2 and later, not a new permission system now |
| A worker's results | ARK adapts a worker to the work model and does not take its own tools away. The boundary is the folder ARK gives the task: ARK records the folder before the worker runs and compares it after. New and changed files are the task's outputs; the recorded state is the version before (ADR 0034). Paths a worker names in its messages are hints, the folder is the record. ARK file tools stay offered, never required |
| Which agent does the work | Earned by measurement, not by preference. ARK's own agent stays the default. A worker is compared with it on the same workloads: success rate, cost, time, how often a person is asked, and later quality. Routing and defaults follow those numbers |
| Where the compute comes from | **Defer ARK-funded compute, not the distributed architecture.** ARK Cloud compute (machines ARK pays for) waits until demand and unit economics justify it. My machine builds and tests the same distributed runtime (a runtime elsewhere, `token link`, notifications, attention across runtimes) on compute the person pays for, so turning Cloud on later is a location and a bill, not new architecture |
| What ARK charges for | Only what costs ARK something: Cloud compute, ARK's models, team and organization features later. Attention, workers and My machine are free everywhere. A person brings their own way in to My machine (Tailscale, a proxy). A relay run by ARK comes with Cloud's services, if ever |
| What ARK takes from One and herdr | The jobs: know what needs me, answer it fast, reach it away from the computer, keep work alive. Not their screens: no floating toolbar, no voice router, no terminal panes. Each job goes into what ARK already has: the attention list, approvals, notifications, the runtime |

Order of work. Each step ends with a check, and the next starts only when the earlier one holds.
Step 1 does not wait for workers: the attention list serves ARK's own agents first, and workers'
questions join it later as records like any other:

| # | Step | Done means |
|---|---|---|
| 0 | **ACP spike**: one ARK task on Claude Code (`claude-agent-acp`) and on Codex (`codex-acp`) | the worker asks permission; the request becomes a kept question; ARK answers it; the worker continues; its result and written file come back as the task's answer and output. Plus what happens when the process restarts while a question is open. Recorded per agent, as ADR 0039 › Consequences asks |
| 1 | **Attention in one runtime** | the same list in the terminal and the window (`host.rs`); questions, failures and handed-off results from different projects; one key answers or opens an item, and clearing it is kept across a restart |
| 2 | **One ACP worker** (ADR 0039 H1 to H3), shaped by step 0. Reassessed against the step 0 results after step 1, before any code | a worker's permission request shows in the attention list like any other question; grants are per command or path; outputs come from the task's folder; measured against ARK's own agent on two workloads (success, cost, time, times asked) |
| 3 | **Reach**: a notification when an item appears, to a phone through the runtime's page (Web Push), and My machine as a location (install with a service and limits, pairing through `token link`) | a VPS paired in one command; a question there reaches the phone and is answered from it; the task continues |
| 4 | **Attention across runtimes**, when a person has more than one | This computer and My machine in one list, each item answered on its own runtime |

## Step 0 results (2026-10-07)

Branch `spike/acp-worker` (not for `main`): `AgentSpec::worker(cmd)` runs a task on an ACP agent
inside `Orchestrator::attempt`; its permission requests go through `OnBehalf`. Live tests
`crates/ark-orchestrate/tests/acp_spike.rs` and the protocol probe `spike/acp-probe.mjs`, on
claude-agent-acp 0.23.1 (Claude Code 2.1.292, Claude subscription) and codex-acp 0.16.0 (ChatGPT
plan, gpt-5.5). Task: read a CSV, write `report.md`.

| Check | Claude Code | Codex |
|---|---|---|
| The worker asks before it writes | yes: `edit`, "Write …/report.md", with the path | yes: `execute`, `printf … > report.md`, no path (another run used an `edit`) |
| The request becomes the task's kept question; task `Waiting` on "your answer"; agent `Waiting`; nothing written yet | yes (22 s) | yes (19 s) |
| Another client answers (`Orchestrator::answer`); the worker continues; task `Done` with the worker's answer; `report.md` an output; person asked once | yes (30 s) | yes (27 s) |
| Process stops with the question open: the question survives, is answered in the next process, the task runs again on a new worker session and finishes | yes; the repeated request matched the kept answer, person not asked again | yes, the same. The probe saw Codex choose a different call after a restart once: then the person is asked again |
| `session/load` after a restart | works, replays the history | works, replays the history |
| Uses the client's `fs/*` when offered | no: writes with its own tools | no: writes with its shell |
| Reads ask permission | no | no |
| Reports usage | tokens and cost per turn ($0.17, 92.7k tokens, 73k of them cached, for this task) | context used of its window |

What this changes for step 2:

| Finding | Step 2 does |
|---|---|
| Writes never go through ARK; Codex's shell writes carry no path | Outputs from the task's folder (Decision › A worker's results; the copy per task of ADR 0039 H4). ACP `fs/*` and `terminal/*` (H2) do not help these two agents, and ARK does not force them |
| An approval class per ACP kind (`worker.execute`) is too wide for "Always in this project": it would allow every command | a grant per command or path, as ARK's own processes do (ADR 0041). Treated as a constraint on ARK's authority (Decision › Authority over work), not as an adapter detail |
| The adapters lag their CLIs: codex-acp refused the model in the person's Codex settings; claude-agent-acp refused `defaultMode: auto` | ARK sets the worker's model and permission mode per task, never inherits them |
| A small task costs Claude Code about 90k tokens | measure success, cost, time and times asked against ARK's own agent before offering workers by default (Decision › Which agent does the work) |

Verdict: both behave as workers. State, the question, the answer and the result go through
ARK's records, not through code glued on the side. Step 1 can start.

## Step 1 results (2026-10-07)

`ark_interact::attention` lists what waits for the person in every project of a runtime. It
reads records that already exist; the only new record is `attention_cleared` (id, time, an
empty `by`).

| Item | From | Leaves the list when |
|---|---|---|
| A question | an open `TaskRecord::question` | it is answered. It cannot be cleared |
| Work that did not finish | a failed task a person, chat, schedule or the board started, in the last 7 days; only a schedule's latest run; when it reports to a chat, only once posted there | cleared, or opened. A retry that fails again is a new item (the id holds `finished_ms`) |
| A result | a task whose result was posted into a chat (a post with its `work`), in the last 7 days. A result a waiting turn took is not one | cleared, or its chat is opened (opening a chat, or a project on that chat, clears what was posted there; a post into the chat on screen is seen on arrival) |

| Front-end | Shows |
|---|---|
| Terminal | `/waiting`: a numbered list across projects. `/waiting 2 once` answers a question in any project; `/waiting 2` opens a result or failure; `/waiting clear`. Numbers follow the list printed last, so newer work never takes a number the person read. A dim line before the prompt when the count changes. The terminal now keeps an agent's question open when the turn that showed it ends (it used to deny it) |
| Window | Results and failures are rows in the tray, above the question cards: a mark, the title, the project and chat or agent. Three rows, then "N more". Open goes to the chat; a failure without a chat shows why under its row; x or Delete clears. Questions stay the tray's cards; an agent's question from another project names it ("In The Lighthouse"), so "Always in this project" is never ambiguous |

What changed from the plan, and why:

| Plan | Shipped | Why |
|---|---|---|
| `/attention`, "Needs you" | `/waiting`, "Waiting for you" | one plain word in both front-ends |
| A sidebar section listing questions too | rows in the tray; questions stay cards | a question twice on one screen (design review blocker) |
| Numbers per list | numbers bound to the list printed last | a number must never answer different work (design review blocker) |
| — | an agent's question card names its project when the window is elsewhere | the dogfood: both projects had an agent called editor, and "Always in this project" pointed at the wrong one |

Evidence: `crates/ark-ui/tests/attention.rs` (one list across projects in both front-ends; the
terminal answers a question in another project; cleared survives a restart, a retry is new,
opening a chat or project clears its posts), the page test
`work_that_waits_for_the_person_sits_in_the_tray`, `scripts/ci.sh` with page tests. Dogfood in
`writer --serve` on qwen3.5:27b with two projects: a handed-off result, failures (an agent on a
missing model), and an agent's save question answered from the other project after a restart
(the chapter saved once). Light and dark, 1280 and 390 wide.

Not in step 1: notifications (step 3), items per person, unread marks for a chat's own turn
finishing in the background.

## Consequences
- Developers can bring the agents they already use, and their questions and results land where
  ARK's own agents' do. This brings developers in; it is not the product's message, which stays
  the product test in ROADMAP › Next.
- ARK's state stays exact: an item exists because a record exists, never because a screen
  looked like a prompt.
- Every screen this touches (the attention list, notifications, the location picker) follows
  the workspace UI rules (`../CLAUDE.md` › Every UI or UX change): it explains itself, with no
  text that repeats a control, and nothing taken from another product's screens.
- Teams and organizations add a person to an item and to its cleared mark. They do not need a
  different list.
- My machine exercises the same path Cloud uses (a runtime elsewhere, `token link`, attention
  across runtimes, notifications) without ARK paying for compute. ARK-funded Cloud compute stays
  deferred for cost and unit-economics risk until demand justifies it
  (`../ark-cloud/docs/CLOUD.md`); when it comes, it is one more location.
- Ruled out: watching or controlling agents ARK did not start; terminal multiplexing; a paid
  tier for attention; ARK's own model calls on a person's vendor subscription through ARK's own
  sign-in (ADR 0012: ARK never reads vendor credentials); a phone app before the page with
  notifications falls short.
- If step 0 shows Claude Code or Codex cannot keep their writes or permissions inside ARK's
  boundary over ACP, step 2 uses a direct bridge only for that gap (ADR 0039), or waits.

## Check
- `crates/ark-ui/tests/neutral.rs`: the work model and the attention list name no vendor,
  worker protocol or workload.
- Step 1: `crates/ark-ui/tests/attention.rs` compares the list in both front-ends.
- Step 0 and step 2: a test where a worker asks, the process restarts, another client answers,
  and the task finishes with its outputs.
- Review: a new item kind needs a record that already exists, not a new store.
