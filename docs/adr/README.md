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
| [0015](0015-a-native-browser-shared-safely.md) | A native browser, shared safely between agents | Accepted, amended by 0046 |
| [0016](0016-a-runtime-serves-its-clients.md) | A runtime serves its clients; a window is one of them | Accepted |
| [0017](0017-work-runs-without-the-window.md) | Work runs without the window: live conversations and schedules | Accepted |
| [0018](0018-the-computer-one-screen-every-action-asked.md) | The Computer: one screen, one agent, every action asked | Accepted, amended by 0042 |
| [0019](0019-files-are-shared-in-place.md) | Files: one file, edited in place by the agent and the person | Accepted |
| [0020](0020-running-code-contained-in-the-project.md) | Running code, contained in the project folder | Accepted |
| [0021](0021-an-output-is-a-file-with-a-kind.md) | An output is a file with a kind; a kind has one view | Accepted |
| [0022](0022-one-place-for-what-differs-between-systems.md) | One place for what differs between operating systems (`ark-os`) | Accepted |
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
| [0038](0038-results-reach-the-chat-and-a-stop-is-graceful.md) | A result the chat handed off reaches it once (as its answer or a post, also after a restart); a stop (SIGTERM, quit) lets a change under way finish and leaves work to continue in the next process | Accepted, amended by 0048 |
| [0039](0039-agent-harnesses-are-workers.md) | Agent harnesses are workers: ARK owns the task boundary (brief, capabilities, approvals, outputs, result), the worker owns its loop; ARK stays the authority for task state, approvals, grants, artifacts, budgets and recovery; no coding-harness features in ARK; ACP first, its behaviour tested before H4 | Accepted |
| [0040](0040-a-browser-comes-in-through-a-one-time-link.md) | A browser comes in through a one-time link (`#code=`, once, minutes) and keeps a session that ends on its own in an HttpOnly, SameSite=Strict cookie; changes must come from the page; revoking ends live streams; an entry URL brings a browser without a session back; a Content-Security-Policy on every answer | Accepted |
| [0041](0041-project-processes-asked-seen-and-stopped.md) | Agents start and stop long-running project processes: uncontained, asked per command and folder ("Always in this project" allowed), seen and stopped in a Processes panel and `/processes`, ready decided by code, a missing program asks to install through `acquire_capability`, groups stopped on exit and swept on start | Accepted, amended by 0046 |
| [0042](0042-the-computer-sees-pictures-menus-and-scrolls.md) | The Computer lists rows, items and only what is in sight, types as keys, uses menus, scrolls, and clicks with the pointer; a picture of one window (never the screen) goes to models that read images, the newest only; approvals unchanged | Accepted |
| [0043](0043-attention-workers-and-where-work-runs.md) | ARK is the control plane and runtime for agent work: one attention list over records ARK keeps (questions, failures, results handed off), other agents only as workers through a protocol (never watched or scraped), three places to run (This computer, My machine, ARK Cloud), nothing charged for attention; order: ACP spike first | Accepted |
| [0044](0044-the-browser-panel-is-a-view-of-its-own-in-the-window.md) | The Browser panel is a native webview over the pane in the window (any page shows, no IPC, never the app's own page, steps aside for ARK's menus and dialogs); a plain browser keeps the iframe and says when a page refuses frames (`frame_check`, public addresses only) | Accepted |
| [0045](0045-a-capability-the-person-turns-on-mid-task.md) | A capability an app offers can be off until the person turns it on; an agent asks for it mid-task (Allow, Not now) and its tools join the turn. The Computer first: one offer line instead of its schemas, no grant covers a request, a person always answers, Turn off in Settings | Accepted |
| [0046](0046-the-agents-browser-opens-its-projects-own-pages.md) | The agent's browser opens public pages and the pages a running process of its project serves itself (its own listening socket), in the fresh browser only; every action's landing address is checked; amends 0015 and 0041 | Accepted |
| [0047](0047-what-waits-reaches-a-phone.md) | What waits reaches a person's phone: Web Push sent by the runtime itself, encrypted to the device, once per item after a minute and only while it waits; a device signed in with a typed code for 90 days, its notifications ending with its session | Accepted |
| [0048](0048-a-chat-turn-keeps-its-progress-and-continues-where-it-stopped.md) | A chat turn keeps its progress: calls name the turn, the trace is saved as it runs, the window shows where it stopped with one Continue; the next turn is told what was done, does not make a change twice, asks again about one cut short, takes up a hand-off still under way; a stop by the app is interrupted, not stopped; a writer is its pid and its start; patient retries; durable blobs; amends 0038 | Accepted |
| [0049](0049-tool-arguments-are-objects-and-shown-text-does-not-block-a-retry.md) | Codex's reply schema is built from the tools of the call: each tool a closed variant, arguments the tool's schema in strict form, read back exactly (never repaired); a model retries a transient failure even after text was shown, which is withdrawn (`TextWithdrawn`) | Accepted |
| [0050](0050-an-agent-never-loses-its-task.md) | An agent never loses its task: a pinned task frame (the goal, what the person said, what a stopped turn did), a ledger of dropped steps, a goal stored with each turn that "continue" carries on, a request budget sized from the model's window, a reply the model could not get right sent back to it as feedback, a project's commands in the person's shell | Accepted |
| [0051](0051-a-turn-ends-on-its-answer-and-follows-the-projects-instructions.md) | A turn ends on its answer and follows the project's instructions: a restated answer beside another check of the turn's own changes ends the turn; the project folder's `AGENTS.md` goes into each turn as the project's rules, within a budget, with subfolders that have their own named | Accepted |
| [0052](0052-a-chat-runs-on-the-computer-the-person-picks.md) | A chat has a place: This Mac or a computer reached over SSH, picked when it starts; chat-addressed calls, held chats, a project copy by manifest with Bring back, setup and a tunnel over the person's own `ssh`; amends 0029 and 0043 | Accepted |
| [0053](0053-scheduled-work-is-asked-for-each-time-runs-once-per-time-and-continues.md) | Scheduled work: dates, cron and the person's time zone; asked for each time with all a run is handed; pause, resume, change, run now; one run per time across a crash; a failure that may pass tried again later, continuing; missed runs run once or are let go; a line per schedule above its chat | Accepted |
| [0054](0054-native-apps-opt-in-to-signed-background-updates.md) | Native apps opt in to signed background updates: Tauri verifies and installs, only the own page sees state, release packages are immutable and the app never forces a restart | Accepted |
| [0055](0055-subscription-models-advertise-their-effort.md) | Subscription models advertise their effort: Codex is discovered from its signed-in CLI, effort belongs to the model spec, and old specs keep provider defaults | Accepted |
| [0056](0056-a-message-is-drafted-sent-and-proved-as-separate-steps.md) | Messages are drafted, sent and proved separately: durable draft safety, race-free Stop, app-owned evidence and truthful unconfirmed results | Accepted |
| [0057](0057-an-external-client-uses-a-versioned-contract.md) | Separately released clients use one versioned ARK contract with generated schema and models, capability discovery, resumable events and ordered mutations | Accepted |
| [0058](0058-a-native-device-pairs-to-one-revocable-credential.md) | A native device pairs once to one revocable credential kept only in its device security store | Accepted |
| [0059](0059-native-phone-notifications-use-a-blind-relay.md) | Native phone notifications use an optional blind relay with no work content or runtime credential | Accepted |
| [0060](0060-a-project-terminal-does-not-need-a-chat.md) | A project Terminal opens without a chat: server-issued project authority, explicit computer binding, one-shot writes, shared PTY end semantics and staged native delivery | Accepted |
| [0061](0061-subscription-agents-keep-their-credentials.md) | Subscription agents keep their credentials on the computer that runs them; Grok is offered without ARK handling its login | Accepted |
| [0062](0062-event-automations-use-durable-events-and-run-scoped-authority.md) | Event automations: transport-authenticated provider events with actor claims, deterministic matching and one durable run per event; schedules stay compatible under one view; every event run has mandatory authority that broader grants cannot widen | Proposed |
| [0063](0063-two-chats-can-share-one-window-without-sharing-state.md) | Multi-chat windows: session-scoped conversation operations, many project tabs and at most two visible panes; each pane keeps its own transcript, draft, approvals, events and resource authority | Proposed |
| [0064](0064-execution-mode-is-per-chat-and-per-turn.md) | Plan, Manual and bounded Auto execution modes are per chat and frozen into each turn; only explicitly project-bound actions can skip approval | Proposed |
| [0065](0065-workspace-destinations-have-one-stable-rail.md) | Home and Schedules use one icon rail; Home restores its last project view and Schedules has race-safe project-wide states | Accepted |
| [0066](0066-generated-images-are-durable-project-files.md) | Generated images are validated raster outputs written as durable project files; chat previews and the side pane use that one file and viewer path | Accepted |
| [0068](0068-plugins-are-reviewed-capabilities-from-independent-providers.md) | Plugins are reviewed capabilities from independent providers; packages, servers, provider integrations, accounts, account activation and action approval stay distinct | Proposed |

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
