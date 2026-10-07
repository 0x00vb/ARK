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
| What ARK charges for | Only what costs ARK something: Cloud compute, ARK's models, team and organization features later. Attention, workers and My machine are free everywhere. A person brings their own way in to My machine (Tailscale, a proxy). A relay run by ARK comes with Cloud's services, if ever |
| What ARK takes from One and herdr | The jobs: know what needs me, answer it fast, reach it away from the computer, keep work alive. Not their screens: no floating toolbar, no voice router, no terminal panes. Each job goes into what ARK already has: the attention list, approvals, notifications, the runtime |

Order of work. Each step ends with a check, and the next starts only when the earlier one holds:

| # | Step | Done means |
|---|---|---|
| 0 | **ACP spike**: one ARK task on Claude Code (`claude-agent-acp`) and on Codex (`codex-acp`) | the worker asks permission; the request becomes a kept question; ARK answers it; the worker continues; its result and written file come back as the task's answer and output. Plus what happens when the process restarts while a question is open. Recorded per agent, as ADR 0039 › Consequences asks |
| 1 | **Attention in one runtime** | the same list in the terminal and the window (`host.rs`); questions, failures and handed-off results from different projects; one key answers or opens an item, and clearing it is kept across a restart |
| 2 | **One ACP worker** (ADR 0039 H1 to H3), shaped by step 0 | a worker's permission request shows in the attention list like any other question; measured against ARK's own agent on two workloads |
| 3 | **Reach**: a notification when an item appears, to a phone through the runtime's page (Web Push), and My machine as a location (install with a service and limits, pairing through `token link`) | a VPS paired in one command; a question there reaches the phone and is answered from it; the task continues |
| 4 | **Attention across runtimes**, when a person has more than one | This computer and My machine in one list, each item answered on its own runtime |

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
  across runtimes, notifications). It does not delay Cloud, which keeps its own plan
  (`../ark-cloud/docs/CLOUD.md`).
- Ruled out: watching or controlling agents ARK did not start; terminal multiplexing; a paid
  tier for attention; ARK's own model calls on a person's vendor subscription through ARK's own
  sign-in (ADR 0012: ARK never reads vendor credentials); a phone app before the page with
  notifications falls short.
- If step 0 shows Claude Code or Codex cannot keep their writes or permissions inside ARK's
  boundary over ACP, step 2 uses a direct bridge only for that gap (ADR 0039), or waits.

## Check
- `crates/ark-ui/tests/neutral.rs`: the work model and the attention list name no vendor,
  worker protocol or workload.
- Step 1: `crates/ark-ui/tests/host.rs` compares the attention list in both front-ends.
- Step 0 and step 2: a test where a worker asks, the process restarts, another client answers,
  and the task finishes with its outputs.
- Review: a new item kind needs a record that already exists, not a new store.
