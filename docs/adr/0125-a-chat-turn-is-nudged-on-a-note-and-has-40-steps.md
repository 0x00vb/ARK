# 0125. A chat turn is nudged on a note and has 40 steps

Status: Accepted. Date: 2026-11-16. Amends 0122 (the note rule was for brief tasks only). Found in the owner's traces of a CEO agent in Auto (workspace `board/evidence/chat-keep-going-20261116/PLAN.md`).

## Context

| Seen | Cause |
|---|---|
| Three chat turns ended on a one-line note after tool calls ("Next I'm updating the packaging script...") and the owner typed "continue" | The note rule ran only for brief tasks (`brief_task`); the word list of `announces_work` misses "Next I'm" |
| Three chat turns used all 16 steps; the forced answer ended "Shall I run the script now?" | 16 steps; the forced-answer prompt ended "the single most useful next step" |

## Decision

| Part | Decision |
|---|---|
| Note rule | Every turn, the person's own and group chat turns included: after narrated calls, a final reply of one line of at most 240 characters is sent back once with its own text (`NOTE_NUDGE`: finished, give the result; if not, continue). `ANNOUNCED_NOTE` stays for `announces_work`. One nudge of either kind per turn |
| Not a note | A reply ending in `?`; any turn where a call was refused or denied; any turn that called an action with `ActionDescriptor::ends_with_short_reply` (set on `role.set`, `groups.set`, `work.later`, `agents.schedule`, `agents.unschedule`, `mode.set`, whose text tells the model to close with one short line) |
| Steps | `DEFAULT_STEPS` 40 (harness and `Ark`). Attended work (chat turns, delegated, started and `work.later` work) uses it. Scheduled runs and automation steps use `min(configured, UNATTENDED_STEPS = 16)`. `Budget::max_steps` of a brief or spec still wins. The stall guard is unchanged (3 idle steps nudge, 5 stop) |
| Forced answer | `DEFAULT_FORCE_ANSWER` asks for what was done and what is left, forbids asking whether to continue and offering options, and allows naming one decision only the person can make that blocks the rest. `ran_out` stays true, so the next turn continues |
| `Harness::brief_task` | Kept as a `#[deprecated]` no-op |

## Cost

Worst case per attended turn: 40 steps x `MAX_BUDGET` 48,000 prompt tokens = 1.92M input tokens (was 16 x 48,000 = 768,000). Unattended work stays at 768,000. A turn that makes no new progress still stops after 5 idle steps. A real one-line result after narrated calls costs one extra model call, once per turn.

## Not proven

The forced-answer test proves the wording is sent, not that a model stops asking. Whether Opus now continues instead of stopping is not tested live. The page was not dogfooded in this change.

## Risks

Group chat turns follow the same rules as the person's own. A person's steering message is read before any nudge. The verify nudge (0108) and this one are independent, once each per turn. An automation step whose call is refused by its guard keeps its note as the result.
