# 0024. The terminal is a client of a runtime too

Status: Accepted
Date: 2026-10-05

## Context
ROADMAP step 5: Desktop and the CLI connect to a running ARK as clients. ADR 0016 made a
runtime an app's binary serving one protocol, and gave the window a way onto a runtime
elsewhere (`ark_ui::remote::run_window`). The terminal still opened its own `Interaction` in
its own process, so a CLI app on a laptop could not use the agents, store and models of the
same app running on a server, and could not follow work a window started there.

What had to be decided:
- **Which protocol.** The window's (`send`, `steer`, `cancel`, `approve`, `command`, events
  over server-sent events), or a second one for terminals.
- **Where the client lives.** The protocol client sat in `ark-ui`. The rule is that `ark-cli`
  and `ark-ui` build on `ark-interact` and never on each other (`AGENTS.md` › Boundaries).
- **How the shell stays one shell.** The prompt loop, turn view, approval menu and `-p` runs
  were written against `Interaction`.
- **What a one-shot run does with questions.** Locally `-p` turns the gate off for the turn
  (`TurnControl::ask`), so state-changing actions are refused, not asked. A remote turn's gate
  is the runtime's.

## Decision
The terminal speaks the window's protocol. Nothing new on the wire except one optional field.

| Question | Decision |
|---|---|
| Flags | `app --connect URL` with `--token` or `ARK_TOKEN`, the same words as ARK Desktop. Not with `--resume`, `--session`, `--project` or `--model`: the chat is the one open there, and `/resume`, `/project` and `/model` work as commands |
| The client | `Client` moves to `ark_interact::remote` and gains `events()` (the numbered event stream). `ark_ui::remote::Client` re-exports it, so nothing breaks |
| One shell | `ark-cli` drives a `Chat`: `Local` (an `Interaction` and its approval channel) or `Remote` (a `Client`). Prompt loop, turn view, approval menu and `-p` are the same code for both; a turn is a stream of updates (events, questions, questions closed elsewhere), a control (steer, stop) and its end |
| A turn there | The terminal subscribes to events first, then sends, and follows the events of the chat whose `turn_started` carries its input, until `turn_finished` |
| Joining work in progress | If the chat is running a turn when the terminal connects, the terminal shows it so far (`hello.live`) and follows it to the end. A command that hands text to the agent is started by the runtime, so the terminal follows it instead of sending it again |
| Questions | Asked in the terminal's menu; answered with `approve`. A question answered in a window disappears from the terminal (`approval_closed`) |
| Stopping | `cancel` names the turn's chat. A stop before the runtime has started the turn is kept and sent again once it has |
| `-p` | `send` takes `ask: false` (default true): the runtime turns the gate off for that turn, as `-p` does locally. Without `--yes` any question that still reaches a one-shot run is denied, local or remote |
| `/help` | Lists the terminal's keys, not the window's |
| Lost connection | The turn goes on there; the terminal says so and how to follow it again |

## Consequences
- A CLI app on a laptop uses the same app on a server: its agents, schedules, store and models,
  through the same access tokens as a window (ADR 0016). The terminal process opens no store
  and calls no model.
- A window and a terminal on the same runtime see the same chat, and the same question can be
  answered in either.
- The terminal does not show other chats' work while it waits at the prompt; the window does.
  Questions from orchestrated agents stay with their tasks (the window's tray), as before.
- `-p --connect --json` has `ok`, `answer`, `cancelled`, `steps`, `session` and `project`; the
  local run's `grounded`, `cited`, `metrics`, `findings` and `context` are not in the protocol.
- A one-shot run without `--yes` used to allow, once each, questions from the app's other
  agents. It now denies them, as `CLI.md` always said.
- Input history in a remote shell is kept only for the session unless the app sets
  `ArkCli::history_file`.

## Check
`crates/ark-cli/src/remote.rs` tests serve a real runtime (`ArkUi::serve_background` with saved
tokens) and drive it from the terminal's side: a turn whose question is answered there, a
question answered in another window, a turn already running when the terminal connects, a
command's turn followed and not sent twice, a turn nobody can answer refused without asking,
stopping before and during a turn, `/help` keys, a refused token and a bad address.
`crates/ark-ui/tests/serve.rs` still drives `ark_ui::remote::Client`.
