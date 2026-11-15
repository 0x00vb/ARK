# 0122. Unfinished delegated work fails, recent answers are carried whole, an agent knows its changes ask

Status: Accepted. Date: 2026-11-15. Builds on 0051 (step notes), 0108 (the check nudge), 0113 and 0117 (group-reached work asks). Found in the owner's traces of 2026-11-08 to 2026-11-15 (workspace `board/evidence/trace-review-20261115/REPORT.md`).

## Context

| Seen | Cause |
|---|---|
| A researcher ended its task on "Reading price lines from the competitor pages already fetched..." after 53 calls and no file; the task was `done` and the parent read it as a result | A reply with no tool call ends the turn; the announced-work nudge matches an English word list only; every outcome that is not cancelled becomes `Done` |
| An agent asked the person to paste back a table it had written two turns earlier | Each carried answer was cut to 500 characters and its lines joined |
| An agent with `allow_mutate` off said its folder was "read-only" and only advised | Nothing told it that its changes ask the person; with no gate the denial said to reason from read-only evidence |

## Decision

| Part | Decision |
|---|---|
| Step note in a brief task | A task that is not the person's chat turn (`Harness::brief_task`). When the turn made tool calls with text beside them and the final reply is one line of at most 240 characters, the model is asked once to continue (the announced-work nudge, at most one per turn). No word list: a one-line answer after calls with no text beside them, or with no calls at all, is not nudged |
| Unfinished | A second note after that nudge sets `HarnessOutcome.unfinished`. The task ends `Failed`, keeps its `result`, `error` starts "unfinished:", and it is not retried. No new `TaskState`. `ran_out` keeps its meaning: an answer forced at the step limit can be a real answer |
| Who sees it | `reports()` and the exchange view show the error, then the kept result. A scheduled run that ends unfinished counts as one failure toward the pause after 3 |
| Carried answers | `CarryPolicy.newest_answer_chars` 4000 and `older_answer_chars` 2000 (serde defaults, so stored policies read). Line breaks are kept; a cut answer ends "(clipped: N of M characters)". Inputs and gists are unchanged |
| Authority line | The agent's prompt says "Your changes ask the person first: call the action and they approve it. You are not read-only." or "You make changes without asking.", from `allow_mutate` after the group check. With no gate, a refused change says no one can approve it in this run and asks the model to say what it would have changed |

## Not done

| Cut | Why |
|---|---|
| A chat tool that turns `allow_mutate` on | `allow_mutate` also lets scheduled runs and automation steps act with no card (`harness.rs`, the no-gate path). It needs its own scope and ADR (board card `agent-allow-changes-20261115`) |
| Files named in a brief checked at the end | Would catch an honest long answer that wrote nothing; cut for size (board card `brief-files-check-20261115`) |

## Consequences

- A long session with long answers carries about 740 tokens more per request (`crates/ark-sdk/tests/long_session.rs`: 93x the largest request at turn 500, was 144x; the request stays flat from turn 50).
- Board items and schedules see `Failed` for unfinished work where they saw `Done`.
- Each agent request carries the authority line (about 30 tokens; simulated `team` and `board` moved, baseline regenerated).
- One extra model call when a real one-line result follows narrated calls.
