# 0025. Work reports into the chat that asked for it

Status: Accepted
Date: 2026-10-05

## Context
ADR 0017 left this undone: "a schedule's runs do not report into a chat; they show in the
agents' work." A person who asks a chat for "every weekday at 9, check the new orders and send
me a summary" expects the summary in that chat, and expects the chat to know what it said when
they answer it. A run's result was only in the task record, in the agents' view.

What had to be decided:
- **Where a result goes.** A session is a numbered list of turns. A live conversation holds its
  session in memory and numbers the next turn from there, so a turn written from outside (by the
  orchestrator, while the chat is open or running a turn) would take a number the chat is about
  to use.
- **Which chat.** Tools did not know which conversation called them.
- **What the model sees.** The carried conversation is bounded (ADR 0002). Results that arrive
  every morning must not grow it.

## Decision
A result is a **post**: a message in the chat's own stream, apart from its turns.

| Question | Decision |
|---|---|
| Storage | `ark_runtime::session::post(engine, session, Post)` appends to the store's event stream `session-posts/<id>`. Append-only and numbered by the store, so it never races a turn, and it works whether or not the chat is open. No new table |
| A post | `from` (the agent), `title` (the task, clipped), `text`, `ok`, `at`, and `after`: the chat's last stored turn when it came (a running turn's placeholder counts), so a transcript places it without comparing clocks |
| Which chat | `TaskSpec::report_to` (and `TaskRecord::report_to`): a session id. When such a task ends done or failed, the orchestrator posts its result there and emits `EventKind::Posted`. A cancelled task posts nothing |
| Schedules | The conversation's tools are built per turn; `Orchestrator::chat_tools_for(session)` gives them the session, and `agents.schedule` sets `report_to` on the schedule's task, so every run reports to the chat that asked. Its description says the result is posted in this chat |
| The model | `Session::read_posts` before each turn. The carried conversation adds "Posted in this chat since your last answer", the newest `POSTS_CARRIED` (3) clipped like answers, and a count of older ones. A finished turn marks them read (`SessionRecord::posts_read`) |
| Showing it | `TranscriptTurn::posts`: each post under the turn it came after. The window draws it as a card from the agent (a failed one says "did not finish") and adds it live from the `post` event; the terminal's recap lists it under the turn |

## Consequences
- Easier: any task can report to a chat. Delegated work that outlives the turn that asked for it
  can use the same field later.
- The chat knows a post on the next turn only; after that it is in the transcript, not the
  context, like any older exchange.
- A post into a chat of another project reaches an open window when that chat is opened; live
  events follow only the open project's orchestrator.
- A terminal waiting at the prompt does not print posts; it shows them in the recap and the
  model has them on the next turn.

## Check
- `crates/ark-runtime/src/session.rs`: `a_post_is_carried_once_by_the_next_turn_and_placed_after_the_last`.
- `crates/ark-orchestrate/tests/orchestration.rs`: `a_scheduled_run_posts_its_result_in_the_chat_that_asked`.
- `crates/ark-ui/tests/orchestration.rs`: `a_scheduled_run_posts_its_result_in_the_chat_and_the_window_shows_it`.
