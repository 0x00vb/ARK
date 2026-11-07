# 0104: The chat changes its own mode on request

Status: Accepted. Date: 2026-11-06. Amends ADR 0064 (the composer control) and finishes the open item of ADR 0103.

## Context

ADR 0103 left the Plan and Manual chip and Shift+Tab on the page and said an agent could not
change the mode: raising its own authority needs a person-only approval. The window now drops
the chip and Shift+Tab. The person must still be able to say "work in plan mode", "ask me
before changes" or "go auto", and a chat nobody has set is already Auto (ADR 0107).

## Decision

| Part | Now |
|---|---|
| Page | No mode chip, no Shift+Tab. The terminal has no mode control today. It gets the tool through the same code path |
| Tool | `mode.set(mode: plan, manual, auto)`, offered to the person's own conversation by `Interaction`, in every turn, beside the app's tools. Not offered to agents, automations or runs |
| Who may call | Only through the approval gate. The action is `Mutate`, `person_only` and never `auto_eligible`. Every change asks, "always" and "for this session" do not cover it, and a guarded or automation run refuses it |
| When it applies | After the turn ends. The turn froze its mode (ADR 0064) and keeps it. The tool result says so, and tells the model to tell the person |
| How | `Interaction::run_turn` keeps the revision the chat had at turn start. The tool records the requested mode in a cell made for that turn. When the turn ends, with an answer, an error or a cancel, `Session::set_execution_mode` applies it with that revision. Revision goes up by one |
| Another client | If the stored mode changed during the turn, the revision no longer matches. The apply is refused and the other client's choice stands. Nothing is half applied: the cell is memory only and the apply is one stored write |
| Plan | Plan hides `Mutate` actions. A new descriptor flag `stays_in_plan` keeps this one offered and runnable in Plan. It requires `person_only`. It changes no project state, only the next turn's authority |
| Same mode | Asking for the mode the chat has changes nothing and says so |

## Why every change asks

The gate decides by an action's risk tier, not by its input, so one action cannot ask only
when authority grows. Two actions (narrow without asking, widen with asking) would put a second
tool definition into every request of every chat. Asking each time costs a click when the
person narrows, and it means a prompt injection in a file or page cannot switch the chat to Auto
or flip it to Plan. A person sees the request in the normal approval card.

## How a person sees the mode

There is no chip. The person sees a mode in two ways: the model says it after `mode.set`
("From your next message this chat asks before changes"), and the approval card names the
change. Auto is every chat's default (ADR 0107) and needs no signal. The host's summary still carries
`execution_mode` and `execution_mode_revision` for any client that draws it.

## Consequences

- The tool adds one definition to each chat request, about 80 tokens (estimate from its
  schema, not a measured run). `ark-bench` does not use `Interaction`, so
  `bench/baseline.json` does not change.
- The page learns the new mode from the `summary` of the `turn_finished` event of the current
  chat (`summary.execution_mode`, `summary.execution_mode_revision`), and from `summary` for a
  chat that ran in the background. No separate event is sent.
- A crash before the apply leaves the old mode. The person asks again. The model said the change comes next message, which is true only when the turn ends.

## Check

- The model calls `mode.set`: the mode is the same during the turn and changed after it, with
  the revision up by one, and the write that followed in the same turn still asked.
- A denial leaves mode and revision unchanged. "Always" does not stop the next ask.
- In Plan the catalog has `mode.set` and has no state-changing action, and the mode can be
  changed to Manual.
- A mode another client stored during the turn is not overwritten.
- Asking for the current mode changes nothing.
