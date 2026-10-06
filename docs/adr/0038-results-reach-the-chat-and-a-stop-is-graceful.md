# 0038. A result reaches the chat that asked, once, and a stop lets work reach a safe point

Status: Accepted
Date: 2026-10-05

## Context
ADR 0037 made work survive a restart. Two gaps were left for work nobody watches (ARK Cloud
Phase 2), W5 and W6 in `ROADMAP.md` › Work runtime:

| Gap | Where |
|---|---|
| The chat's `agents.delegate` did not set `report_to`. A result came back only as the answer of the chat's own call, so it was lost when that turn ended first (a restart, `wait: false`, the person stopping the turn) | `ark-orchestrate/src/tools.rs` |
| `report_back` posted at the end of a task with no record that it had, so a stop between the end and the post lost the result, and nothing could tell a result posted from one not posted | `Inner::report_back` |
| A served runtime stopped only on ctrl+c. A deploy sends SIGTERM, which killed the process in the middle of whatever ran: a change under way was cut short, and ADR 0037 then fails its task for the person to check | `ark_ui::ArkUi::serve` |

## Decision
The task record carries the delivery; the engine carries the stop. No new work type.

| Question | Decision |
|---|---|
| Who gets a handed-off result | Whoever asked. A task an agent delegates goes back to its parent (the parent's brief lists its children and their results when it continues, ADR 0037). A task the chat hands off reports to that chat: `agents.delegate` sets `report_to` to the conversation, as `agents.schedule` does (ADR 0025) |
| Exactly once | `TaskRecord::reported`: the result reached its chat. A turn of that chat waiting for the task in this process takes the result as its call's answer and marks it (`ChatWait`), and nothing is posted. Otherwise the task posts it when it ends and marks it. A turn that stops waiting without the result (cancelled) leaves it to be posted. Each post names its task (`Post::work`) |
| After a restart | A finished task that reports to a chat and is not marked is delivered when the orchestrator loads: posted, unless its chat already has a post naming it (the process stopped between the post and the mark). A record saved before the field existed counts as reported (the old code posted when the task ended) |
| What stops a run | `Engine::stop_soon()`, shared by the engine and the engines scoped from it. Every turn and task watches it (`Harness::stopping`): no new model or tool call starts, a model call or a batch of reads under way is dropped, and a batch with a state-changing call runs to its end, so a change is never cut in half. `Engine::runs_ended()` resolves when no run is in progress. A cancel still drops everything at once |
| What a stop does to work | A task whose run stopped is not failed: it is saved as queued, "continues after a restart" (or waiting for the person, when it asked), with no process, and the next process continues it as ADR 0037 does. The changes it made are in its done-before list, so none is made twice. A chat's turn ends as cancelled. A task that starts while the process stops waits for the next one |
| Who stops | `ArkUi::serve` on ctrl+c or SIGTERM: `Host::stop(grace)`, then exit; a second signal exits at once. The native window when the app quits. The terminal when the person leaves. `grace` is `ARK_STOP_GRACE_SECS` (default 25): a platform that kills the process after a timeout should allow a little more |

## Consequences
- A result reaches the chat whether the turn waited, did not wait, was stopped, or the process
  restarted in between, and is in the conversation once: as an answer or as a post.
- A deploy leaves no work failed. A change still running when the grace period ends is cut
  short and fails its task after the restart, as before (ADR 0037).
- The chat's own turn is not continued after a restart: the person sees it stopped and the
  result arriving as a post.
- A result the chat's turn took as its answer is marked when the call returns. If the process
  stops before the turn is stored, the result is in the task, the call's stored output and the
  board, but not posted.
- New fields: `TaskRecord::reported`, `Post::work`. Code that builds a `TaskRecord` adds
  `reported`.

## Check
- `crates/ark-orchestrate/tests/orchestration.rs`: `a_handoff_the_chat_waits_for_is_its_answer_and_is_not_posted_too`,
  `a_handoff_the_chat_does_not_wait_for_is_posted_in_it_once`,
  `a_result_that_never_reached_its_chat_is_posted_after_the_restart_once`,
  `a_stopping_process_lets_a_change_under_way_finish_and_the_task_continues_after`,
  `a_stopping_process_drops_a_model_call_and_the_task_continues_after`.
- `examples/writing-agent/tests/restart.rs`: a real `writer --serve` process, a fake
  OpenAI-compatible model, the chat hands a task off and waits, the agent saves, then the
  process gets SIGTERM (exits cleanly) or SIGKILL. The next process continues the task, does
  not save again, and the chat gets the result once.
