# 0123. A named agent's chat turn follows the chat's mode

Status: Accepted. Date: 2026-11-16. Builds on 0107 (every chat starts in Auto), 0104 (the chat's mode), 0109 (an agent has many chats), 0113 and 0117 (group-reached work asks), 0118 (`work.later`), 0120 (schedules read the chat's mode). Amends 0118 and 0120 where they say nothing reads the mode for an orchestrated agent.

## Context

The owner's real trace of 2026-11-15 (after 20:00 UTC): the agent `online-ecommerce-ceo`, on the person's own turns in a chat left in Auto, raised approval requests for `process.start` 18 times, `files.write` 6, `files.edit` 3, `computer.open` 2, and one each for `code.run`, `computer.click`, `computer.menu`.

| Cause | Where |
|---|---|
| The orchestrator built the task's `Harness` without a mode, so it ran as Manual whatever the chat's mode was | `orchestrator.rs` (harness build), `Harness::new` defaults to Manual |
| Only the schedule tools read the chat's mode (0120), and `work.later` copied it into a record nothing read (0118) | `threads.rs` |
| An agent with `allow_mutate` had no approval gate at all, so an action that asks every time (`ask_each_time`) could not ask: the harness ran it | `orchestrator.rs`, `harness.rs` |

## Decision

| Topic | Rule |
|---|---|
| Whose mode | The person's own turn in that agent's chat (`TaskRecord::own_chat_turn`: a chat turn, `person`, not `group_turn`) takes the mode of the conversation it runs in. A chat with no record yet is new and in Auto. The mode is read when the task starts, so a queued task, a retry and a restart read the person's latest choice |
| The person | The person's window (`/task`), the terminal and the person's own paired devices send `person` turns. Their turn follows the chat's mode like any other. Every other path (delegation, `agents.start`, a schedule, an automation, Work, a board item, a group turn and an `@Name` hand-on, which is a group turn) asks as before, in any chat, in any mode |
| Auto | Every action that is neither `ask_each_time` nor `person_only` runs without a card. Those two still ask (0107). So `computer.look`, `computer.open`, the active window and the like still ask in Auto |
| Manual | As before: every change asks |
| Plan | As in the plain chat: state-changing tools are not offered, acquisition is refused, the task says "Plan only". It applies on top of `allow_mutate` because the person chose it |
| `work.later` | The work reads the asking chat's mode when it starts, not a copy made at the hand-off. It follows the mode only when `TaskSpec.later` is set (only `work.later` sets it; a stored or sent task never carries it, `#[serde(skip)]`), the turn it names is this agent's own `own_chat_turn`, and that turn's chat is its `report_to`. A forged `reply`, `work` or `report_to` gets none of it and asks. Work from a group turn asks |
| `allow_mutate` | Keeps its meaning: it runs actions without a card. It no longer overrides `ask_each_time` or `person_only` when there is someone to ask. An agent with `allow_mutate` now has the approval gate, so those two ask. With no one to ask (no gate) it acts as before. This holds for every host of the harness, the plain chat and the SDK included: a host with `allow_mutate` and a gate asks for those two, a host without a gate acts alone |
| Prompt | One line for every mode, keyed only by whether there is a gate: "Some changes ask for approval first: call the action, it runs or the person decides. You are not read-only." `allow_mutate` keeps "You make changes without asking." The authority line does not change with the mode |
| Threads | `TaskSpec.thread` must be one of the agent's own conversations (`check_thread`), so a task cannot borrow another agent's chat mode. Tested |

## Not in this decision

| Left alone | Why |
|---|---|
| A setter for the mode in an agent's chat | No client has one today: agent chats are Auto, and Manual and Plan are reached by tests only. Under 0104 `mode.set` is a chat tool. A card tracks it |
| Automations | They keep their own approval route (`runs.rs`) |
| Group chats | A member's turn keeps asking whatever the mode (0113) |

## Consequences

- A named agent in Auto stops asking before ordinary changes. A prompt injection in a page the agent reads can now change files and run code in its folder without a question, as it can in any Auto chat (0107). The limits are the same: the sandbox, the folder, and what asks every time.
- An agent with `allow_mutate` asks for `ask_each_time` and `person_only` actions where it ran them before, including `agents.schedule` in ask mode (0120 said it did not ask). This settles the point 0120 left to the owner.
- The cut-short rule (an action whose result was lost asks again) now also applies to agents with `allow_mutate` that have a gate.
- Verified with scripted models only. Not verified: the real app, a live model on the owner's agent.
