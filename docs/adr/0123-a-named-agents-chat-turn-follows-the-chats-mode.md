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
| The person | A turn sent by any authenticated client is the person's: `submit_task` (`Control::Submit`, `ark-interact/src/orchestration.rs`) and the host's chat turn (`host.rs`) set `person`, and clients reach them only with the person's tokens (the window, the terminal, the phone, paired computers). A plain `chat_send` from them already runs in the chat's Auto, so their agent turn follows the chat's mode too. An agent never reaches those calls: `agents.delegate`, `agents.start`, `agents.schedule` and `work.later` create tasks that are not `person`. Covered by `chat_mode.rs::only_the_persons_own_turn_follows_the_chat_the_rest_asks` (delegated and started work into an agent in an Auto chat asks). Every other path (delegation, `agents.start`, a schedule, an automation, Work, a board item, a group turn and an `@Name` hand-on, which is a group turn) asks as before, in any chat, in any mode |
| Auto | Every action that is neither `ask_each_time` nor `person_only` runs without a card. Those two still ask (0107). So `computer.look`, `computer.open`, the active window and the like still ask in Auto |
| Manual | As before: every change asks |
| Plan | As in the plain chat: state-changing tools are not offered, acquisition is refused, the task says "Plan only". It applies on top of `allow_mutate` because the person chose it |
| `work.later` | The work reads the asking chat's mode when it starts, not a copy made at the hand-off. It follows the mode only when `TaskSpec.later` is set (only `work.later` sets it; a stored or sent task never carries it, `#[serde(skip)]`), the turn it names is this agent's own `own_chat_turn`, and that turn's chat is its `report_to`. A forged `reply`, `work` or `report_to` gets none of it and asks. Work from a group turn asks |
| `allow_mutate` | Keeps its meaning: it runs actions without a card. It no longer overrides `ask_each_time` or `person_only` when there is someone to ask. An agent with `allow_mutate` now has the approval gate, so those two ask. With no one to ask (no gate) it acts as before. This holds for every host of the harness, the plain chat and the SDK included: a host with `allow_mutate` and a gate asks for those two, a host without a gate acts alone |
| Prompt | One line for every mode, keyed only by whether there is a gate: "Some changes ask for approval first: call the action, it runs or the person decides. You are not read-only." "You make changes without asking." only for an agent with `allow_mutate` and no gate, so an agent that acts alone but has a gate also says some changes ask. The authority line does not change with the mode |
| Unattended runs | A scheduled run, delegated work, `agents.start` work and group work of an agent with `allow_mutate` now stop at an `ask_each_time` or `person_only` action, as agents without it always did. Nobody answering is not an error: `OnBehalf::decide` (`orchestrator.rs:5477`) calls `Inner::ask` (`:3403`), which stores the `Question` on the task and parks it (`park`, `:5252`): state `Waiting`, `waiting_on` "your answer" (`WAITING_PERSON`). The wait is on the host's approver (`ApprovalGate::check`, `ark-runtime/src/approval.rs:173`) and has no timeout. The question is in `Orchestrator::questions()` and the attention list, survives a restart (ADR 0036), and the task ends only by an answer, a Stop or `cancel` (the harness's `cancellable` wait). A schedule skips its next runs while a run is open, unless `overlap` is on (`schedule.rs:12`, ADR 0053). Automation steps are not changed: they decide through the run's guard (`harness.rs:1308`, the `guarded` arm), which never read `allow_mutate`. Tests: `chat_mode.rs` (`a_scheduled_run_of_an_agent_that_acts_alone_asks_for_what_asks_every_time`, `an_unanswered_question_keeps_the_run_waiting_until_it_is_stopped`) |
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
