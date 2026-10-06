# Architecture decisions

One file per decision that constrains future work. The code and tests are the truth for how
things work; an ADR records why a choice was made, so the next session does not undo it.

## Index

| # | Decision | Status |
|---|---|---|
| [0001](0001-no-domain-in-core.md) | The runtime names no domain, tool or file format | Accepted |
| [0002](0002-bounded-context.md) | Working state is the memory, not the transcript | Accepted |
| [0003](0003-deterministic-before-inference.md) | Code decides what code can decide; a model call needs a benchmark | Accepted |
| [0004](0004-ark-state-canonical-vendor-sessions-disposable.md) | ARK's store is canonical; vendor sessions are disposable | Accepted |
| [0005](0005-delegation-by-brief.md) | A delegated agent receives a brief, never a conversation | Accepted |
| [0006](0006-release-by-immutable-tag.md) | A release is an immutable git tag; consumers pin it | Accepted |
| [0007](0007-the-chat-creates-agents-and-tasks.md) | The person's chat creates agents and tasks; forms are not the way in | Accepted |
| [0008](0008-planner-off-by-default.md) | The planner is off by default | Accepted |
| [0009](0009-skills-load-on-demand.md) | Skills: names in context, instructions on demand | Accepted |
| [0010](0010-web-search-without-a-key.md) | Web search: the model's own first, keyless after, no key | Accepted |
| [0011](0011-capabilities-and-connections.md) | Capabilities people see, connections ARK owns | Accepted |
| [0012](0012-vendor-cli-processes-live-with-their-session.md) | A vendor CLI's process lives as long as its session | Accepted |
| [0013](0013-asking-for-a-capability-mid-task.md) | Asking for a capability mid-task | Accepted |
| [0014](0014-agents-follow-the-sessions-model.md) | Agents follow the session's model unless the person sets one | Accepted |
| [0015](0015-a-native-browser-shared-safely.md) | A native browser, shared safely between agents | Accepted |
| [0016](0016-a-runtime-serves-its-clients.md) | A runtime serves its clients; a window is one of them | Accepted |
| [0017](0017-work-runs-without-the-window.md) | Work runs without the window: live conversations and schedules | Accepted |
| [0018](0018-the-computer-one-screen-every-action-asked.md) | The Computer: one screen, one agent, every action asked | Accepted |
| [0019](0019-files-are-shared-in-place.md) | Files: one file, edited in place by the agent and the person | Accepted |
| [0020](0020-running-code-contained-in-the-project.md) | Running code, contained in the project folder | Accepted |
| [0021](0021-an-output-is-a-file-with-a-kind.md) | An output is a file with a kind; a kind has one view | Accepted |
| [0023](0023-the-project-board.md) | The project's board: work items that follow their tasks | Accepted |
| [0024](0024-the-terminal-is-a-client-too.md) | The terminal is a client of a runtime too | Accepted |
| [0025](0025-work-reports-into-the-chat-that-asked.md) | Work reports into the chat that asked for it | Accepted |
| [0026](0026-an-optional-ark-account.md) | An optional ARK Account, signed in through the browser | Accepted |
| [0027](0027-named-openai-compatible-endpoints.md) | Named OpenAI-compatible endpoints, and out of credit as its own error | Accepted |
| [0028](0028-secrets-in-a-file-on-servers.md) | On a server, secrets are kept in one encrypted file in the store | Accepted |
| [0029](0029-the-window-visits-a-runtime-elsewhere.md) | The window opens a runtime elsewhere in place of its own page, and comes back | Accepted |
| [0030](0030-a-folder-sent-to-a-runtime-becomes-a-project.md) | A folder sent to a runtime elsewhere becomes a project there | Accepted |
| [0031](0031-an-app-names-its-computers-sign-in.md) | An app can name the computer's sign-in to its ARK Account | Accepted |
| [0032](0032-run-code-contained-on-every-system.md) | Run code is contained on macOS, Linux (bubblewrap) and Windows (AppContainer) behind one `Sandbox` | Accepted |
| [0033](0033-data-work-with-generic-tools.md) | Data work with the generic tools: Python's data packages, Files reads large sheets in part, a skill that comes with Run code, databases as read-only plugins, no analytics mode | Accepted |
| [0034](0034-a-file-a-call-changes-keeps-its-version-before.md) | A file a call changes keeps its version before: copies before a run, `ProducedArtifact::before`, `Engine::restore_file`, Restore and Undo on the file card | Accepted |
| [0035](0035-files-dropped-on-the-chat-go-into-the-project.md) | Files dropped on the chat go into the project: `upload_add`, names in use get a number, the composer names them | Accepted |
| [0036](0036-a-runtime-for-work.md) | A runtime for work: coding is one workload, no new work model, and what a client shows about work is runtime state | Accepted |
| [0037](0037-work-survives-a-restart.md) | Work survives a restart: a question is kept on its task and any client answers it; a restart continues work, never repeats a change, and fails a task only when a change was cut short; each agent's step is in the graph | Accepted |
| [0038](0038-results-reach-the-chat-and-a-stop-is-graceful.md) | A result the chat handed off reaches it once (as its answer or a post, also after a restart); a stop (SIGTERM, quit) lets a change under way finish and leaves work to continue in the next process | Accepted |
| [0039](0039-agent-harnesses-are-workers.md) | Agent harnesses are workers: ARK owns the task boundary (brief, capabilities, approvals, outputs, result), the worker owns its loop; ARK stays the authority for task state, approvals, grants, artifacts, budgets and recovery; no coding-harness features in ARK; ACP first, its behaviour tested before H4 | Accepted |
| [0040](0040-a-browser-comes-in-through-a-one-time-link.md) | A browser comes in through a one-time link (`#code=`, once, minutes) and keeps a session that ends on its own in an HttpOnly, SameSite=Strict cookie; changes must come from the page; revoking ends live streams; an entry URL brings a browser without a session back; a Content-Security-Policy on every answer | Accepted |

0001 to 0006 were recorded on 2026-10-03 from decisions made earlier and documented in
`AGENTS.md`, `docs/ARCHITECTURE.md`, `docs/ORCHESTRATION.md` and `docs/RELEASING.md`.

## Writing one

- When: a choice between real alternatives that a later change could quietly reverse (a
  boundary, a data format, a protocol, a measured-and-rejected idea).
- Name: `NNNN-short-slug.md`, next free number. Add a row to the index.
- Never rewrite an accepted ADR's decision. Supersede it: new ADR, old one gets
  `Status: Superseded by NNNN`.
- Keep it under a page.

```markdown
# NNNN. Title

Status: Proposed | Accepted | Superseded by NNNN
Date: YYYY-MM-DD

## Context
What forced a choice. Facts, numbers, links.

## Decision
What we do, in one or two sentences.

## Consequences
What gets easier, what gets harder, what is now ruled out.

## Check
The test, script or review step that keeps it true.
```
