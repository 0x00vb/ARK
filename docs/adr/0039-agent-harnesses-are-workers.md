# 0039. Agent harnesses are workers: ARK owns the task boundary, the worker owns its loop

Status: Accepted (the owner, 2026-10-05)
Date: 2026-10-05

## Context
The owner's direction (2026-10-05): ARK owns work, orchestration, durable state, context,
resources, artifacts, approvals and outcomes. Agent harnesses (ARK's own, Claude Code, Codex,
OMP, later ones) are replaceable workers. A person should be able to give the same work to any
of them under one work model.

The competitive review (ROADMAP › Work runtime, extended 2026-10-05 to Tern and OMP, both from
Stencil Labs) found:

| Finding | Where |
|---|---|
| Every task runs in ARK's own `Harness`. `Orchestrator::attempt` builds it and calls `ask`; nothing else can run a task | `ark-orchestrate/src/orchestrator.rs`, `attempt` |
| Claude Code and Codex reach ARK only as models: their own tools are off (`claude --tools ""`, Codex read-only with `approvalPolicy: never`), and ARK's loop runs ARK's tools (ADR 0012) | `ark-model/src/cli.rs`, `codex_app.rs` |
| ARK is an MCP client only (stdio). Nothing outside ARK's loop can call ARK's capabilities, so an external agent cannot use ARK's files, Run code, browser, Computer, connections or grants | `ark-tools/src/mcp.rs` |
| W2's "never repeat a change", versions before (ADR 0034) and a task's outputs are built from ARK's invocation log (`done_before`, `collect_outputs`). A worker's own writes would not be in it | `orchestrator.rs` |
| The task boundary already fits a worker: a delegated agent gets a brief, never a conversation (ADR 0005); the result is an answer plus outputs; `HarnessEvent` (text, tool call and result, model call with usage) is not specific to ARK's loop | ADR 0005, `ark-runtime/src/harness.rs` |
| An open protocol now covers most of what a worker needs. ACP (Agent Client Protocol, JSON-RPC over stdio): the client starts and loads sessions, sends prompts, cancels, and answers `session/request_permission`. The client may serve `fs/read_text_file`, `fs/write_text_file` and `terminal/*`. The agent streams `session/update` (message chunks, `tool_call`, `tool_call_update`, `plan`, `usage_update`). About 38 agents speak ACP natively (Gemini CLI, Copilot, Cursor, Goose, OpenCode, OpenHands, Junie and others), and OMP has its own `omp acp`. Claude Code and Codex speak it through adapters (Zed's `claude-agent-acp`, `codex-acp`) | agentclientprotocol.com; OMP `src/modes/acp/acp-agent.ts` |

## Decision
An agent's harness is a choice made per agent. ARK's `Harness` stays the default. A worker
harness runs one task and stays inside ARK's work model. ARK keeps the task boundary:

| ARK owns (canonical, ADR 0004) | The worker owns (disposable) |
|---|---|
| the work item, the task, its brief, budget, state, question and result | its loop, its context management, its prompt and edit formats |
| capabilities, through one gate: approvals, grants, leases (the signed-in browser, the screen), connections and their tokens | its session transcript, a cache ARK may resume or drop |
| files written through ARK, versions before, outputs as artifacts by kind | its coding tools (LSP, debugger, AST edits), which ARK does not rebuild |
| usage and cost per task, the graph, the trace | |

| Question | Decision |
|---|---|
| Where it plugs in | Workers are resolved like models (`ModelResolver` / `AgentModels`), so `ark-orchestrate` names no vendor. A worker gets what `Harness::ask` gets today: brief, scope folder, the agent's filtered `Registry`, approvals, cancel, steering, stopping. It emits `HarnessEvent`s and returns an answer |
| The first protocol | ACP, as a client: one generic worker for every agent that speaks it. Which commands start which agent is data (like `ark-connect`'s catalog), not code. A direct bridge (Claude Code `stream-json` with tools on, Codex `app-server` with approvals on) only where ACP measurably falls short |
| ARK's capabilities for a worker | ARK serves the agent's filtered `Registry` to the worker (ACP `fs/*` and `terminal/*` where the agent uses them, and an MCP endpoint per task). Every call goes through the same gate, invocation log and artifacts as in ARK's loop |
| Approvals | A worker's `session/request_permission` is an `ApprovalGate` request on behalf of the task (as `OnBehalf` does), so it becomes a stored question (W1) any client answers. A worker never runs with "allow everything" unless the agent's `allow_mutate` says so |
| What ARK cannot see | Writes the worker makes with its own tools, outside ARK. Until a folder checkpoint per task exists, a worker that writes natively declares it, and the task says its changes cannot be restored or replayed safely. The generic fix is the deferred copy of the folder per task (a clone or reflink, as OMP's `pi-iso` does), not a coding concept |
| A restart | The worker's session resumes when the protocol allows (`session/load`), else the task restarts from its brief and the calls ARK recorded. "Never repeats a change" holds for changes that went through ARK |

Rules that hold from now on, whether or not a worker exists yet:

| Rule | Means |
|---|---|
| ARK does not rebuild a coding harness | Features a worker provides (code navigation, debuggers, AST and hash-anchored edits, edit formats per model, worktree flows) are not built in ARK |
| No worker concept in the work model | Project, work item, task, agent, capability, artifact, approval and result name no harness, vendor or worker protocol. Worker details live at the edge (`ark-model` or a worker crate, and data) |
| One task, any worker | The same task can eventually run on ARK's own agent, OMP, Claude Code, Codex or another compatible worker. The task's shape does not change with the worker |
| ARK stays the authority | Whatever the worker: task state, approvals, capabilities and grants, artifacts and results, budgets, observability and recovery are ARK's records |
| Capabilities are borrowed, credentials are not | A worker calls ARK's capabilities through ARK. It does not get the tokens, keys or accounts behind them |

## Consequences
- A person can run the same work item with ARK's agent, Claude Code, Codex, OMP or any ACP
  agent. Work, questions, approvals, outputs and results look the same in every client.
- ARK does not build coding-agent tools (LSP, DAP, AST and hash-anchored edits, worktree flows).
  Coding quality comes from the worker.
- `ark-bench` can compare workers on the same workloads by cost per success. That is the
  routing evidence no single harness has.
- ADR 0004 holds at the task boundary. Inside a task, a worker's own context is its business.
- Subscriptions: ARK still never reads a vendor's credentials. A worker signs in with its own
  login, as the CLI does today (ADR 0012).
- No code until a concrete external-worker use case to validate the seam against: a person asks
  to run work on a vendor agent with its own tools, or a coding consumer on ARK needs it. The
  first slice is one ACP worker (H1 to H3 in ROADMAP › Work runtime), measured against ARK's
  own agent on two workloads. ACP is the first path unless that testing shows a limit that
  matters.
- Nothing here assumes how an agent behaves over ACP. Before H4 counts as solved, a test with
  OMP (`omp acp`), Claude Code (`claude-agent-acp`) and Codex (`codex-acp`) records, for each:
  whether its file writes go through the client's `fs/write_text_file` or its own tools;
  whether its commands go through `terminal/*`; which actions ask `session/request_permission`
  and what a refusal does; what its tool calls report (paths, diffs, outputs) that ARK can turn
  into outputs; and whether `session/load` or `session/resume` brings a session back after the
  process restarts. The seam is designed from those results.
- The protocol ARK's clients use should become documented and versioned (ROADMAP, the protocol
  in its own crate). That is separate work, not part of this decision.

## Check
- Review: `ark-orchestrate` names no agent product (`crates/ark-ui/tests/neutral.rs`).
- When built: a task on an ACP worker (a scripted fake agent) asks permission, the process
  restarts, the terminal answers, the task finishes, and its file writes are outputs with
  versions before.
