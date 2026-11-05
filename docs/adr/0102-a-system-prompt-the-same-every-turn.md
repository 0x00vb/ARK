# 0102: A system prompt that is the same every turn

Status: Accepted. Date: 2026-11-05.

## Context

On `claude:sonnet`, ARK cost about 10% more per successful coding task than Claude Code on
the same model, while sending 36% fewer input tokens
(`../board/evidence/ark-vs-claude-code-20261105/`). The first request of every new vendor
session read 579 cached tokens and wrote about 10,000. Claude Code's own first request read
12,152 cached tokens.

Claude Code marks one cache breakpoint at the end of the system prompt, so the system prompt
is cached only as a whole. Two captured ARK system prompts were identical except for:

| Part | Why it changed |
|---|---|
| `Project: <name> — <target>` and the artifact list | one per project |
| one of 30 tool entries | the 2 actions preloaded for the task (`catalog::matching_actions`) |

Either difference makes the whole prompt a cache write at twice the input price.

## Decision

| Part | Now |
|---|---|
| Project block (name, target, newest artifacts) | First message, as `== PROJECT ==`, for every provider. The system prompt is instructions, rules and the capability index only |
| Preloaded actions | Offered from the second request when the provider reports `Provider::caches_system_prompt_whole()`. They stay listed in `== READY TO CALL ==`; a direct call on the first step runs, since calls are looked up in the registry. Other providers get them at once, as before |
| `caches_system_prompt_whole` | New `Provider` and `SessionBackend` hint, default false, forwarded by wrappers. True for the Claude Code CLI only |

Also in this change: a Claude Code reply counts as "gave up" only when the CLI refused a
native tool call (`ClaudeReply::refused`). `num_turns > 2` also matched Claude Code's own
structured-output enforcement, which made ARK ask a finished answer again (5 of 15 runs).

## Consequences

- A new vendor session reads the system prompt from Claude Code's cache when any earlier
  session of the same profile wrote it within the cache lifetime.
- On Claude Code, a preloaded action has no schema in the first request, only its line in
  READY TO CALL. A model that calls it with wrong arguments gets the adapter's error and the
  schema on the next request.
- Codex and the API providers are unchanged apart from the project block's position. Their
  caches match partial prefixes, so the request's most-to-least-stable order already served
  them.
- Orchestrated agents build their own system prompt (`agent.rs`), which still names the
  project. A delegated agent on Claude Code still writes its prompt to the cache once per new
  vendor session. Moving that block is a separate change with its own measurement.
- Measured after the change, same 5 coding tasks, 3 runs, interleaved with the installed
  build and Claude Code (`../board/evidence/ark-vs-claude-code-20261105/round2/`): a later
  chat's first request read 7,940 cached tokens and wrote 2,295 (before: 579 and about
  10,000). Fresh input fell from 211k to 88k tokens, the estimated cost per success from
  $0.080 to $0.047 (Claude Code: $0.069), success 14/15 then 7/8 on the extra question runs
  (installed build 15/15). No GAVE_UP retry was sent (installed build: 6 sessions).
- Rejected: a stable-tool-count field on every request (new surface on the `Provider` API for
  one backend); turning preload off for subscription lanes (loses the READY TO CALL hint).
