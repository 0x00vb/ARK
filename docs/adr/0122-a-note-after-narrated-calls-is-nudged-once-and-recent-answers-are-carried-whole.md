# 0122. A note after narrated calls is asked about once, recent answers are carried whole, an agent knows its changes ask

Status: Accepted. Date: 2026-11-15. Builds on 0051 (step notes), 0108 (the check nudge), 0113 and 0117 (group-reached work asks). Amended by 0125 (the note rule covers every turn). Found in the owner's traces of 2026-11-08 to 2026-11-15 (workspace `board/evidence/trace-review-20261115/REPORT.md`). Amended by 0127 (nudges after each step, a first-reply note).

## Context

| Seen | Cause |
|---|---|
| A researcher ended its task on "Reading price lines from the competitor pages already fetched..." after 53 calls and no file; the task was `done` and the parent read it as a result | A reply with no tool call ends the turn; the announced-work nudge matches an English word list only; every outcome that is not cancelled becomes `Done` |
| An agent asked the person to paste back a table it had written two turns earlier | Each carried answer was cut to 500 characters and its lines joined |
| An agent with `allow_mutate` off said its folder was "read-only" and only advised | Nothing told it that its changes ask the person; with no gate the denial said to reason from read-only evidence |

## Decision

| Part | Decision |
|---|---|
| Step note in a brief task | A task that is not the person's chat turn (`Harness::brief_task`). When the turn made tool calls with text beside them and the final reply is one line of at most 240 characters, the model is asked once to continue (the announced-work nudge, at most one per turn). It applies to delegated, started, scheduled and automation work, including an automation step. No word list: a one-line answer after calls with no text beside them, or with no calls at all, is not nudged |
| After the nudge | Whatever the model answers is the answer, and the task ends `Done`, so a narrating agent gets one nudge and no more. An earlier version of this ADR failed a task whose second reply was again a note; that rule was dropped after a live Haiku run failed a task whose summary file was written. A second note does not fail the task: a live run failed a task whose summary file was written, and a correct one-line result such as "Renamed 3 leads." or "Wrote out/brand.md." would fail the same way. There is no `unfinished` outcome and no new `TaskState` |
| Carried answers | `CarryPolicy.newest_answer_chars` 4000 and `older_answer_chars` 2000 (serde defaults, so stored policies read). Line breaks are kept; a cut answer ends "(clipped: N of M characters)". Inputs and gists are unchanged |
| Authority line | The agent's prompt says "Your changes ask for approval first: call the action and it is approved or refused. You are not read-only." when its task has no `allow_mutate` and has an approval route (a gate, or an automation run), and "You make changes without asking." when `allow_mutate` is effective (after the group check). With no route there is no line: the refusal says no one can approve it in this run and asks the model to say what it would have changed, so the prompt must not promise an approval |
| Benchmark | `recall-answer` (simulated): an 8-row table in turn 1, another answer in turn 2, a question about row 7 in turn 3. It fails on the code before this ADR (row 7 was clipped) and passes now |

## Not done

| Cut | Why |
|---|---|
| A chat tool that turns `allow_mutate` on | `allow_mutate` also lets scheduled runs and automation steps act with no card (`harness.rs`, the no-gate path). It needs its own scope and ADR (board card `agent-allow-changes-20261115`) |
| Files named in a brief checked at the end | Would catch an honest long answer that wrote nothing; cut for size (board card `brief-files-check-20261115`) |

## Consequences

- A long session with long answers carries about 740 tokens more per request (`crates/ark-sdk/tests/long_session.rs`: 93x the largest request at turn 500, was 144x; the request stays flat from turn 50).
- Board items and schedules see no change in state: a task that ends on a note is `Done`, as before.
- An agent request with an approval route carries the authority line (about 24 tokens). The line sits after the agent's name in its own block, so it splits no prefix that agents share. It can push a short request over a provider's minimum cacheable size (1,024 tokens in the simulator and for real providers such as Anthropic's), so a short agent near that minimum can show a first-request cache write that it did not have before: the simulated `board` writer's first request went from 1,021 to 1,045 tokens before its last message and its cache writes rose by 1,069 tokens. The simulated runs have no approval route, so the baseline does not move for it.
- One extra model call when a real one-line result follows narrated calls.
