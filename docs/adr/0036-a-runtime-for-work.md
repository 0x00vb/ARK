# 0036. A runtime for work: coding is one workload, and what a client shows about work is runtime state

Status: Accepted
Date: 2026-10-05

## Context
The owner's direction (2026-10-05): ARK is a runtime people give work and outcomes to. ARK
coordinates agents, tools, context, resources and artifacts to get it done. Coding is one
workload, not the architecture. ARK does not copy coding-agent orchestrators (Orca: parallel
coding agents in git worktrees; Herdr: a terminal multiplexer that knows each agent's state),
but it must not be behind them on the basics: work that persists, agent state at a glance,
parallel work, delegation, local and remote runs, observability, extension, low friction.

A review of ARK and ARK Desktop against that direction (2026-10-05) found:

| Finding | Where |
|---|---|
| No git, worktree, branch, PR or terminal concept in a runtime type or flow. Left: `SurfaceKind::Terminal` and `DiffViewer` in `ark-core`, the `present` tool offering "the terminal" to every host, a security word in `router.rs` | `ark-core/src/surface.rs`, `ark-runtime/src/catalog.rs` |
| The work model already exists: project, work item (an outcome), task (one agent's run toward it), agent, capabilities, artifacts, approvals, result | ADR 0007, 0017, 0023, 0034 |
| A question waiting for the person lives only in the serving process: a `oneshot` in `ark_ui::Host`. A restart loses it, and the orchestrator does not know its task waits for the person | `ark-ui/src/host.rs`, `ark-interact/src/approval.rs` |
| A restart fails every open task, queued and suspended ones too, and none reports to its chat | `Orchestrator::load` |
| What each agent is doing now, the team tree and "Needs you" are worked out in ARK Desktop's page or the window's Host, so the terminal and other clients cannot show them | ark-desktop `ui/lib.js`, `ui/agents.js` |
| A delegated task's outputs included the versions before its calls kept (ADR 0034). Fixed with this ADR | `Orchestrator::collect_outputs` |

## Decision
No new work model and no new types. Developer concepts stay out of the runtime. Anything a
client shows about work is a record or field in the runtime's store.

| Question | Decision |
|---|---|
| The work model | Project (a store namespace and a folder) → work item (the outcome, ADR 0023) → tasks (runs toward it) → agents → capabilities (`Adapter`, `PerAgent`, connections, leases) → artifacts (task outputs, versions before) → approvals (`ApprovalGate`, grants) → result (item Done, the task's answer and outputs). No `Objective`, `Workspace` or `Outcome` type beside these |
| Developer concepts | Repos, worktrees, branches, PRs, terminals and diffs are capabilities or an app's surfaces, built on the primitives above, never runtime types. The test stays ADR 0001's, with a second case: would it make sense for an agent that edits novels, and for one that reconciles spreadsheets? |
| Several agents | An implementation detail by default: the chat decides whether to delegate (ADR 0007). The board, the agents, traces and the graph are the advanced view of the same records, not a second way in |
| Runtime state, not client state | What work waits for, whether it was interrupted, and what each agent is doing are stored by the runtime and carried by `graph()` and the protocol. A client draws them; it does not derive them. A client may keep view state (layout, places, filters) |
| Isolation between agents on one folder | The version guard (`files.edit` `version`) and versions before (ADR 0034) are the mechanism today. A copy of the folder per task, the generic form of a worktree, comes only when a workload shows agents overwriting each other |

## Consequences
- Three corrections follow, in `ROADMAP.md` › Work runtime: W1 a question for the person is a
  stored record linked to its task; W2 a restart resumes work instead of failing it; W3 each
  agent's current activity is in the graph. ARK Desktop then drops what its page derives (W4).
- Cloud (ROADMAP step 6) depends on W1 and W2: its value is work that runs with every computer
  off, and a machine restart must not lose a question or fail the work.
- A coding product on ARK (worktrees, branches, PRs, compare and merge) is a consumer, like
  SecRes, not ARK work.
- `SurfaceKind::Terminal` and `DiffViewer` move out of `ark-core` at the next breaking release.
  Done for v0.11.0; `neutral.rs` (`the_work_model_names_no_coding_or_worker_concept`) keeps them
  out.

## Check
- `crates/ark-ui/tests/neutral.rs` for names; review against the table above for new types.
- `crates/ark-orchestrate/tests/orchestration.rs`,
  `a_files_version_before_is_not_a_task_output`.
- W1 to W3 each land with a test that a second client (the terminal) sees the same state as the
  window.
