# 0113. Group chats route each message

Status: Accepted. Date: 2026-11-09.

## Context

The owner wants a chat with several agents, in one or more projects. A person's message should
reach the right agents without a full model turn per message, and an agent should get a brief,
not another agent's conversation (AGENTS.md, Boundaries).

## Decision

| Part | Decision |
|---|---|
| Records | `group` (name, members `{project, agent}`, per-member thread and cursor; scope `#groups`) and `group_msg` (scope: the group id). Ids sort by time |
| Window calls | `groups`, `group_create`, `group_update`, `group_delete`, `group_messages`, `group_send`; event `ark:group {id}`. Top level, no chat needed |
| Rules | In `ark-interact::groups`, pure and tested without a model. The driver is in `ark-ui` |
| Routing | 1. `@Name` (agent id or display name, case-insensitive): those members, no model call. 2. One live member: that one. 3. One router call on the default model (members with role, last 6 messages of 300 characters). 4. Unknown or empty answer, or a failed call: the last agent that spoke, else the first member |
| Hand on | A reply that names `@Other` brings that member in. Never the author. At most 6 agent replies per person message |
| Turns | Each member answers in its own thread for the group (`Orchestrator::start_thread`, `TaskSpec.thread`). The objective is the messages since its cursor as `Name: text` lines (20 messages, 6000 characters, newest kept); the context is the roster and the rule "reply to the group only; to bring another member in, write @Name" |
| Queue | One person message at a time per group; the next waits |
| Members | An ended or missing member is skipped and left out of `groups` rows. A group needs two or more live agents |
| Failure | A failed task stores a `failed` message with a short reason. The member's cursor stays, so its next turn still sees what it missed |

## Alternatives

| Option | Result |
|---|---|
| D1 group record and router (chosen) | 0 router calls for `@Name` or one member, else 1 short call; then one task per answering agent. Predictable, cheap |
| D2 a normal chat whose model delegates (`submit_task` per member, results post back with `report_to`) | A full harness turn on every message plus the tasks; the chat model may answer itself. Dearer and less predictable |

## Consequences

- `group_send` answers after routing, so a router call (at most 15 seconds) is inside the call. The replies come later through `ark:group`.
- Group state lives in the store and survives a restart; a turn in flight does not resume.
- Members of other projects are reached through that project's orchestrator.
- The page and the phone read the same calls; neither holds group state.
