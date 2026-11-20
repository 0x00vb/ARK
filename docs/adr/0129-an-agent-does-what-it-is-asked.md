# 0129. An agent does what it is asked

Status: Accepted. Date: 2026-11-20. Amends 0108 (what counts as a look for the check nudge) and
0049 (Codex as a model).

## Context

The owner's chat with a named agent of a business (`codex:gpt-6-luna@medium`, Auto), 2026-11-12
to 2026-11-20: asked to adapt an onboarding script and set up a cafe's account, it answered that
"the project is read-only", that it had no tool to run the script, and that it lacked the cafe's
authorization. It had `files.write`, `files.edit` and `process.start`. Its last answer was a reply
to the check nudge: "I made no file change", after it had read the cafe's menu.

A live test on the same model reproduced it (`board/evidence/agent-does-it-20261120/`): a named
agent in an Auto chat of ARK Desktop `--serve`, four requests written as the owner writes them,
each checked on the folder afterwards. Desktop 0.19.0 failed the onboarding request 3 of 3 with
"solo lectura" and no write.

| Cause | Evidence |
|---|---|
| Codex gives the model tools of its own: `exec`, `request_user_input`, `clock.sleep`, `collaboration.*`. ARK turned off only `shell_tool` and `apps` | the model called `tools.files.write` through `exec`, it failed, and the model said it could not write; three calls hung 600 s |
| Codex runs read-only and tells the model so | the model repeated "read-only" as the project's state |
| The system rule said "Actions that change state need explicit human approval" | in Auto nothing asks; the model reasoned it could not act |
| `code.run` said to use `process.start` "when it is offered"; `process.start` was not loaded | the model ran Node in the contained runner, failed, and said commands could not run |
| The check nudge followed a navigation click or any command, and the held answer was replaced by the reply to the nudge | the final answer lost the credentials it had made |

## Decision

| Part | Decision |
|---|---|
| Codex tools | Probed on codex-cli 0.166.0. `CODEX_FEATURES_OFF` (`ark-model`): `shell_tool`, `apps`, `multi_agent`, `collaboration_modes`, `sleep_tool`, `unified_exec`, `tool_suggest` off on the `exec` and `app-server` paths. Codex ignores an unknown name with a warning. `code_mode_host` stays: turning it off makes every call report an error item |
| Codex prompt | Its read-only sandbox is only for itself; the host tools write and run; never say it is read-only; never call `exec` or another function of its own |
| System rule | Call the actions the task needs, including ones that change things; the host asks when approval is needed; never ask in text or say it is read-only. A refusal the model owes (unlawful, harmful) is not forbidden here; the app's persona says when to decline |
| Check nudge | `browser.click`, `browser.type` and `process.start` are `Volatile`: their answer is the page or the command's output, so they count as a look. They stay `Mutate`, so approvals are unchanged. The nudge says the person has not seen the held reply and asks for the whole answer |
| Running commands | `code.run` says to load `process.start` when the session has a terminal; `find_capability` for an installed program names `process.start` |
| Apps | Desktop preloads `process.start` and its persona does all that is asked, without plans, offers, refusals or lectures (Desktop `src/voice.rs`) |

## Measured

Same model, same requests, regraded by one checker (`regrade.py`):

| Arm | Onboard | Leads | Tests | Posts | All |
|---|---:|---:|---:|---:|---:|
| Desktop 0.19.0 | 0/3 | 3/3 | 2/3 | 3/3 | 8/12 |
| This change | 3/3 | 3/3 | 3/3 | 3/3 | 12/12 |

Intermediate candidates (`runs-regraded.jsonl`) show each cause: without the Codex feature list,
2 of 6 onboarding runs hung at 600 s; without the nudge wording, 3 of 3 lost the credentials.
One run of an earlier candidate was refused by OpenAI's usage filter; it did not recur.

## Consequences

- An agent changes things more readily. Where approvals ask (Manual, delegated, scheduled,
  group work), they still ask: no risk tier or approval rule changed.
- A command no longer brings a check round after it, even one that changed a file without
  showing it (`sed -i`), and a command run after an edit counts as checking that edit, even
  `echo`. Chosen: a command's exit code and output are what the agent saw, and the nudge after
  every command cost a reply and once replaced the answer. ADR 0108's rule stays as crude for
  other looks (any read after a change counts).
- Approval does not read `Reuse`: a volatile `Mutate` call still asks where approvals ask
  (`crates/ark-computer/tests/asks.rs` covers volatile Computer actions).
- Agents on Codex read the person's global Codex instructions (`~/.codex/AGENTS.md`); Codex
  loads them despite `--ignore-user-config`. Workflow steps written there now run through
  `process.start`.
- The measurement is a development set: n=3 per cell, the candidates were tuned on these
  requests except the content one, which no change was made for.
