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

## Step 2 results (2026-10-07)

An agent whose model is `agent:claude-code` or `agent:codex` does its tasks in that program,
when the app turns it on (`ArkBuilder::workers`). Claude Code came first: its requests name the
file and it reports cost. Codex is the second entry of the same catalog, data in `ark-acp`.

| Part | Where | What |
|---|---|---|
| The trait | `ark_runtime::worker` | `Worker::run(WorkerJob) -> WorkerRun`: brief, folder, gate, cancel, kept state; reports `HarnessEvent`s as the harness does. `Workers` is the app's catalog |
| The program | `ark-acp` | a clean environment (`env_clear` and a list: home, path, user, locale); its own process group, stopped on every end; Claude Code without the person's or the project's settings (`settingSources: []`, the adapter in a private empty home for the run, the program with the person's real home for its login) and in `default` mode (`session/set_mode`); Codex with `approval_policy="untrusted"` and a read-only sandbox; ARK sets the model per job; only connector versions ARK was checked with run (claude-agent-acp 0.23, codex-acp 0.16), since keeping the program inside ARK's answers depends on the connector |
| Questions | `ark-acp` -> the task's `OnBehalf` gate | a change is `program.change {paths}` with the grant `program.change:<paths>`, shown with the lines it removes and adds; a command is `program.run {command, folder}` with `program.run:<command>@.`, the exact command (a leading `cd` into the project folder is dropped), shown whole; anything else, or a path outside the folder or through a link, asks every time. ARK answers with the program's "once" options only, and refuses when the program offers no "allow once" |
| Results | `ark_runtime::folder`, `orchestrator/elsewhere.rs` | the folder listed and copied at the task's first start (kept under the store until it ends); when the task ends, however it ends, new and changed files are outputs and changed and removed ones keep versions before, as one `program.files` call, and the copies are removed. One program at a time in a project folder |
| Restarts | `orch_worker` record, `TaskRecord::decisions` | what a gone run left running is stopped (its process group, when its leader is gone or is still the program); the program loads its own session; the run that continues one the process stopped in takes its refusals and its leave to change a file again, never leave to run a command (it may have run already); a pause or a retry asks again |
| Measures | `Usage::cost_micro_usd`, `Usage::asked`, a `ModelCall` with the program's tokens | `bench/RESULTS.md` › ARK's agent against Claude Code |

What changed from the plan, and why:

| Plan | Shipped | Why |
|---|---|---|
| The trait in `ark-model` | in `ark-runtime`, beside `Harness` | it needs the gate, the cancel token and `HarnessEvent`; `ark-model` depends on no ARK crate (plan review) |
| Claude Code without its settings via `settingSources: []` | also the adapter in an empty home | the adapter reads the person's `~/.claude/settings.json` itself; this machine's `defaultMode: auto` made every session fail, and a project's `bypassPermissions` would skip questions. Setting `CLAUDE_CONFIG_DIR` instead loses the person's login |
| A model from the person's settings | ARK sets one; Codex defaults to `gpt-5.5` | codex-acp refused the model in this machine's Codex settings |
| `take_answer` for exactly-once | plus `TaskRecord::decisions`, carried only over a stopped process | `take_answer` only covers a question still open at the restart; an answer given before the process died (the write done, the task not ended) was asked again (plan review). Carrying every answer to every later run let a command run twice (implementation review) |
| A Settings list of programs | none | a program that is not installed is not offered; nothing to choose there yet |

Evidence: `crates/ark-orchestrate/tests/worker.rs` against a scripted program (the test binary
run again): asked, kept, answered from another client, the file an output, tokens and cost
recorded, no ARK secret in the program's environment, settings left out, mode set; a shell write
with no path in its request found in the folder; a restart after the answer and the write loads
the session, asks nobody, keeps the file once. Removing the decisions check or `env_clear`
fails them. Live, `tests/worker_live.rs` with a project whose own settings allow everything and
skip questions: Claude Code (claude-agent-acp 0.23.1) asked ARK before writing, and after a
restart following the write it saw the work done and asked nobody; Codex (codex-acp 0.16.0,
gpt-5.5) the same. Dogfood in `writer --serve`: a question in the tray from another project, the
chapter changed, "Always in this project" for `wc -w *.md` covered it in later tasks, two Denies
left the folder unchanged and the program said it would not retry.

Measured (3 runs each, both on Sonnet, two small tasks): both succeed every time; ARK's agent is
2.5 to 4 times cheaper and 1.6 to 3.4 times faster on them. ARK's agent stays the default.

Limits:
- The program is not contained. It reads anything it can without asking, and Claude Code runs
  commands it classifies as read-only (`ls -la` was seen) without a question, as reads do not
  ask for ARK's agents. Codex runs in its read-only sandbox. ARK's authority holds while the
  program keeps the mode ARK set; that is why only checked connector versions run.
- A process a program starts detached (`setsid`) leaves its group and is not stopped with it.
- The person's own edits, or ARK's agents' changes, in the folder while a program's task waits
  count as that task's (a copy of the folder per task is H4's rest). Hidden folders (`.github`)
  and package folders are not looked at; copies stop at 512 MB.
- A run killed mid-way loses its usage (a program reports tokens when a prompt ends); Codex
  reports no tokens. If the process stops between recording the work and saving the task's end,
  the work is recorded twice.
- A project whose own settings name a mode the adapter does not know (`auto`) fails that
  adapter's session, with the reason.
- A person picks a program only by typing `agent:claude-code` (`/agent new`, an agent's spec):
  nothing in the window offers the installed ones yet. Which way to offer them is the owner's
  call (a board card).

## Step 3 results (2026-10-07)

What waits reaches a person's phone, and their own server is a place in the window. Design and
details: ADR 0047.

| Part | Where | What |
|---|---|---|
| Notifications | `ark-ui` (`push`, `notify`), `ark-interact` (`attention::notice`) | Web Push from the runtime itself, encrypted to each device; once per item, a minute after it appeared, only while it waits and only to devices not looking at it; the list's words; a tap opens the item |
| Signing a device in | `ark-ui` (`Tokens::pair_code`, `POST /pair`, the sign-in page) | an 8-character code from a signed-in device, typed on the new one (a phone's Home Screen app opens no links), for 90 days |
| Settings › Devices | `web/js/devices.js` | Notifications on this device (Off \| On, with why when it cannot work), a code for another device, the devices that get notifications. Settings now fit a phone |
| My machine | ARK Desktop (branch `my-machine`) | `token pair` prints the address, a QR code and the code; Settings › My machine takes the address and the code; the switch above the projects reads This Mac \| My machine (\| Cloud) without an account; the deploy gets limits, the public address and a loopback port for `tailscale serve` |

What changed from the plan, and why: a chat's own question notifies too (it is not on the list:
plan review); the runtime decides not to send to a device in view instead of the worker hiding a
notification (iOS stops delivering to a worker that shows nothing); a phone signs in with a code
it types, for 90 days (a Home Screen app has its own cookies and no address bar); My machine
replaces `--connect`'s saved address instead of sitting beside it; the push key stays in the
store (ADR 0047); a device's session lasts 90 days, not renewed (the gate's revision said 30
days, renewed while used: a renewal would rewrite the token file on every request, and a phone
that is not opened for three months pairs again in a minute); a paired device is named under the
one that paired it and never outlives it (implementation review: a paired session must not
escape revocation). Dropped: the install script (`docker compose` on the server is the one
command; a prebuilt image per tag would make it fast on a small server, and publishing one waits
for the owner).

Evidence: RFC 8291's example sealed byte for byte, a device opening its message, the VAPID
signature (`push.rs`); what is sent and to whom (`notify.rs`, also through a host); pairing,
the 90-day session, the public worker and manifest, a device tied to its session (`serve.rs`);
typed codes (`access.rs`); Desktop's `token pair` and a code signing it in at a real runtime
(ark-desktop `remote.rs`). Dogfood on this Mac (writer with saved tokens, as a server would run
it): Chromium subscribed through Google's push service; a failed task in another project arrived
as a notification with the list's words; a Claude Code agent's command question arrived on a
device not looking, the tap's page opened on its card at phone width, Allow once, the program
wrote its file. Light and dark, 1280 and 390 wide.

Not checked here: an iPhone and an Android phone with their real push services and Home
Screen, a real server reached by name or over Tailscale, and ARK Desktop's window switching to
My machine and back (the owner's checks, on the board).

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
