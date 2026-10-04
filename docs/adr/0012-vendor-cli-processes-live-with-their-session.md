# 0012. A vendor CLI's process lives as long as its session

Status: Accepted
Date: 2026-10-04

## Context
Claude Code and Codex are ARK's main execution path (subscriptions). Up to v0.8.0 every model
call started the CLI, waited for the whole reply and parsed it at the end:

| Measured 2026-10-03/04 | Fresh process per call | Running process |
|---|---:|---:|
| Claude Code, Haiku, a short reply | 2.7 s | 0.7 s |
| Codex, a short reply (`codex exec` / `app-server` turn) | 23 s | 1.7 s |

Nothing streamed, so a person saw no text until the call ended, and a cancelled turn left
the vendor session in an unknown state that the next call resumed.

Both CLIs can stay running: Claude Code with `--input-format stream-json --output-format
stream-json` (a user message per line in, events out), Codex with `codex app-server`
(JSON-RPC: `thread/start`, `turn/start` with an output schema, `turn/interrupt`). Both
stream the structured reply in pieces.

## Decision
A managed vendor session keeps one running CLI process (Claude Code) or one app server holding
its thread (Codex). A call takes the process, writes the new messages, reads events until the
reply is complete, and gives the process back. The reply's `text` field streams to the
person while it is written. A call that is cancelled or fails drops its process (which kills
it), and the session reopens from ARK's own request on the next call (ADR 0004).

Stateless calls (a planner, a search) still start the CLI once. When `codex app-server`
cannot be started, Codex sessions fall back to `codex exec` and `exec resume`.

## Consequences
- Time to first token on Claude Haiku fell 59% and p95 turn time 52% (3 runs of three
  workloads); a 4-call Codex task fell from 98 s to 39 s. `bench/RESULTS.md`.
- One CLI process per open vendor session. The account's `max_sessions` and the idle sweep
  bound how many run; each is stopped when its session is released.
- A process cut off mid-reply is never written to again, so a cancelled turn cannot leave an
  answer to an old question in the vendor's history.
- `codex app-server` is marked experimental by Codex; a protocol change shows up as the
  `exec` fallback and a warning in the log, not as a failed turn.
- A Codex thread still starts the MCP servers of the person's own Codex setup (about 4 s and
  15k tokens on the machine measured); `app-server` has no `--ignore-user-config`.

## Check
`crates/ark-model/src/cli.rs` tests (fake CLIs speaking each protocol: one process per
session, streaming, a process that stops, the app server's own requests declined),
`crates/ark-model/src/session.rs` (`a_cancelled_call_drops_the_vendor_session_and_the_next_one_opens`).
