# 0127. A note is sent back after each step, and a rotated vendor session keeps its prompt

Status: Accepted. Date: 2026-11-17. Amends 0108 (the check nudge), 0122 and 0125 (the note
rule). Found in the owner's traces of a CEO agent in Auto on `claude:opus@medium` (workspace
`board/evidence/finish-turn-20261117/PLAN.md`).

## Context

| Seen (42 turns, 2026-11-15 to 17) | Cause |
|---|---|
| A first reply that was only a note ("Finding every em dash and listing the product files first."), 1 step, 0 calls, task done | The note rule needed narrated calls earlier in the turn |
| A turn nudged on a note at step 24, then ended on another note at step 32 | One note nudge per turn |
| 4 turns ended on an offer ("If you want, I can ...") after "keep going until it's done" | The method asked to "end with the single most useful next step" |
| An answer delivered with `self_check` "changed 3; checked after: no" | One check nudge per turn |
| 21 of 32 vendor session rotations on Claude Code read 579 cached tokens and wrote the system prompt again (about 16k tokens each) | A rotation opened with a system prompt listing every tool loaded so far; Claude Code caches the system prompt only whole (ADR 0102) |

## Decision

| Part | Decision |
|---|---|
| Note nudge | A text reply with no tool call is sent back when it announces work it did not do (`announces_work`), or is one line of at most 240 characters not ending in `?` and either follows narrated calls or is the first reply to a task that is not trivial (`classify_trivial`) on a turn the person started (`Harness::first_note`, default on; the orchestrator sets it only for the person's own turn, since a helper's, a schedule's or an automation's one-line reply is often the whole result: 19 orchestration tests showed it). Not after a refused call or a short-closing action, as before. The reply right after a nudge is the answer. A step that runs a call allows the next nudge; at most 8 a turn |
| Check nudge | Once per change instead of once per turn: a later unchecked change is asked about again |
| Prompt | The method's last line asks for all of the work asked, no optional extras, no asking whether to continue, and to ask the person only for what they alone can do |
| Cited result | A one-line reply that cites a result (`inv:`) is an answer, not a note: a live run nudged "12 × 12 = **144** (calc.eval, inv:...)" and paid a call for the same answer |
| Claim nudge | A reply that names a file no call wrote is withdrawn when sent back, so the person sees one answer (a live run showed the answer twice). A sentence that says a file "has to" or "have to" be made ("report.md has to be created another way") is not a claim |
| Retry | The method said "Do not re-run a tool you already ran", and a live model refused to retry a call that had failed halfway. It now says not to re-run a call that worked, and that a failed or partial call is worth one more try |
| Rotation | A lane keeps the system prompt and tools of its first vendor session. A later open (rotation, idle, failure) uses that system prompt and sends the instructions and tools that differ as a `<host_update>` before the transcript (`SessionBackend::open_from`) |

## Rejected: a `finish` tool that alone ends a turn

Built and measured on the same day (patch `board/evidence/finish-turn-20261117/finish-tool-attempt.patch`).
Live `claude:sonnet`, 15 workloads x 3, against v0.51.0 (82% success, $0.0265 per success):

| Variant | Success | Cost per success | Why |
|---|---|---|---|
| Every text reply without `finish` sent back | 64% | $0.0473 | The model answers through the CLI protocol's `reply`, so every answer cost a second call; after the nudge it wrote "Finishing with the answer." beside `finish` and the answer was lost |
| Only notes sent back, `finish` confirms the earlier reply | 71% | $0.0344 | Called directly, `finish` came with a status line as its text ("Closing out the task with the result from the CRM lookup."): text beside a call is a note in the CLI protocol |

Carrying the answer in a `finish` argument would stop answers from streaming. The checks it
was meant to carry (work left, the person's part) stay in the prompt.

## Cost

| Item | Effect |
|---|---|
| A one-line first reply on a non-trivial task, and a one-line reply after more narrated work | one more model call each, at most 8 a turn |
| Prompt line | about 20 more tokens, in the cached prefix |
| Rotation | from the traces, about 345k of 2.23M written tokens (15%), about 11% of input cost at write 2x and read 0.1x |

## Measured

Live `claude:sonnet`, 15 workloads x 3 (`bench/finish-turn-2026-11-17/`). v0.51.0 is shown with
the corrected `recall-answer` check, rescored from its traces (its own report says 37 of 45):

| | v0.51.0 | This change |
|---|---|---|
| Success | 40 of 45 (89%) | 44 of 45 (98%) |
| Cost per success | $0.0245 | $0.0201 |
| Model calls per run | 3.44 | 3.22 |
| Uncached input per run | 2,585 | 2,488 |
| Median turn | 7.3 s | 6.1 s |

The one failure: `claimed-files`, the model guessed the argument names, corrected them, and
reported the partial failure instead of trying again.

Rotation, live `claude:sonnet` with `ARK_SESSION_MAX_TURNS=2`, `long-session` (12 turns) x 2:
cache writes 22,332 to 19,164 per run, cached share of input 77.8% to 80.4%, cost $0.1142 to
$0.1004. The bench's tools are few; ARK Desktop's system prompt is larger (about 16k tokens).

A long job on ARK Desktop's runtime (`claude:opus@medium`, clean dashes from 10 or 24 files
and write a Spanish copy of each): finished in one turn before and after. After: 5 and 6 model
calls (before 5 and 8); one of two final answers still offered an optional change (before: two
of two). Simulated suite: success and calls unchanged; input +3% from the longer prompt.

Not measured: Opus on the bench workloads; Codex rotation.
