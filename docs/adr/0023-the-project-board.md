# 0023. The project's board: work items that follow their tasks

Status: Accepted
Date: 2026-10-05

## Context
In a team of agents nothing held the project's work across chats and sessions. Each agent's
working state is private, a task is one run that ends, a brief goes one way, and the window's
agent canvas shows who is busy now, not what the project still needs. A new chat or agent
rebuilt "what is left, who has it, what needs me" from the conversation, or did not.

The owner asked for a project-wide board the person and the agents both use (2026-10-04), and
decided: the view is called Board, an item moves to Done when its task finishes (the planner
can move it back), and the work starts now. Building ARK itself, the sessions coordinate on a
local kanban the same way, and they took to it at once.

What had to be decided:
- **What an item is.** A task already has states. Making a card a task means moving it to "In
  progress" starts a paid run, and a person's own todo has no agent, budget or attempts.
- **Who keeps it current.** The planner measured off (ADR 0008): plan steps kept in the working
  state cost model calls and success on qwen3:4b. Cards an agent must update are the same risk.
- **What reaches the model.** A board injected into every request grows with the project.
- **Where it lives.** A file in the folder (like our `board.json`) or the store, where tasks,
  agents and schedules are.

## Decision
A **work item** (`ark_orchestrate::WorkItem`) is an outcome the project needs; a **task** is
one agent's run toward it. One board per project, kept by the orchestrator in the store (record
kind `orch_work`) and traced as `EventKind::Work` and `WorkRemoved`.

| Question | Decision |
|---|---|
| An item | `title`, `note`, `state` (`todo`, `doing`, `done`), `owner` (an agent, `user`, nobody), `blocked` (what it waits for, in words), `tasks` run for it, a place in its column, a log of its last 30 steps |
| Blocked | A field, not a column: a blocked item stays where it is. Blocked and owned by `user` is "Needs you"; so is an item whose task waits for the person's answer (the window reads that from the pending approvals) |
| Who keeps it current | Code. `TaskSpec::item` (the chat's `agents.delegate {item}`, the window's "Give to…", `/board give`) links a task: the item moves to In progress with that agent as owner. The last open task finishing moves it to Done with the answer in its log; a failure blocks it with the error; a cancel puts it back in To do; a retry clears the block |
| Two agents, one item | Refused: an item with an open task of one agent cannot be given to another, nor its owner changed, until that task ends |
| What the model does | Plans: `board.add` (several items in one call), `board.update`, `board.list`, offered to the person's conversation only (`Orchestrator::board_tools`, beside `chat_tools`). A worker gets no board tools; its brief carries its item (title, note, its last 3 steps) |
| What reaches the model | `board.list`'s description is the board in one line (`Board: 2 to do, 1 in progress, 4 done.`); the items come only when the model reads them, bounded (40 open, the 5 latest done) |
| Front-ends | `/board` and `Control::{AddWork, ChangeWork, RemoveWork, GiveWork}` in `ark-interact`; the window's methods `board`, `board_add`, `board_change`, `board_remove`, `board_give` build the same controls. The window shows a Board tab beside Chat in every app that runs agents (`board.js`) |

## Consequences
- The board survives chats, restarts and team changes, and a new session reads where the project
  stands from one line and one call.
- Agents spend no calls on bookkeeping: the board moves with the tasks. Whether the chat plans
  good items is model behavior; on qwen3.5:27b it added three items for a three-part guide and
  linked the first delegation to its item (dogfood, 2026-10-05). A live benchmark workload for
  planning on the board comes before any harness change for it.
- The chat's requests carry three more tool schemas when the app runs agents.
- An agent defined in code with delegates (a team lead) gets no board tools yet; its
  delegations are not linked to items. Add them when such an app needs it.
- Later views (a list, dependencies, milestones, filters) read the same items: dependencies
  would be an `after` field like `TaskSpec::after`, not a new model.
- An app's own view with the id `board` replaces ARK's.

## Check
- `crates/ark-orchestrate/tests/board.rs`: the conversation plans and hands items off, items
  follow their tasks, two agents never take one item, failure, retry and cancel, order and
  restart.
- `crates/ark-interact/tests/interaction.rs`, `board_command_shows_and_changes_the_board`.
- `crates/ark-ui/tests/page.rs` (WebKit), `the_board_is_worked_by_the_person_and_the_agents`.
