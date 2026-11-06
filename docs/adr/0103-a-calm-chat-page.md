# 0103: A calm chat page

Status: Accepted. Date: 2026-11-06. Revises the page parts of ADR 0094 and ADR 0097; their records are unchanged.

## Context

The owner wants the window to show chats and little else. The fixed rail, the composer and an
empty chat each carried controls and text a person did not need to start a conversation:
Work and Automations destinations, a schedule strip above the composer, mode and model
chips in every composer, and a hero with suggestions in an empty chat.

## Decision

| Part | Now |
|---|---|
| Rail | Chat, the app's views, what the app pinned (`ark.rail`), the account. Work and Automations are removed from the page |
| Work and automations | The runtime keeps them. Agents create and run them when asked; there is no page for them. `work.js`, `schedules.js` and `automations.js` are deleted; the board view stays |
| Composer | The model chip shows only while no model is chosen. The mode chip shows only for a mode the person chose (Plan or Manual). Shift+Tab still cycles it. A chat nobody chose a mode for (revision 0, Manual) is set to Auto once with the `execution_mode` call before its first message |
| Who the chat is with | A chip in the title bar (face and name) opens the details in the side pane: face, name, description, model. An app view supplies it with `identity()` on its controller; a plain chat is with ARK |
| Empty chat | Nothing, then a typing mark for one second, then one line from a fixed list (`greeting.js`). The line is not sent to the model and not stored |

## Auto as the default

The owner chose Auto for every chat. Auto runs only actions marked `auto_eligible` (project-bound
mutations) that the guard allows. It never runs `person_only` actions and never skips an
`ask_each_time` action. Plan and Manual stay reachable with Shift+Tab, and a chat in either shows
its chip, so a person always sees a mode that is not Auto. The runtime and SDK default stays
Manual; only this page changes the mode, through the host's existing call, so the host and page
agree before the first message. If that call fails the chat stays in Manual with its chip showing.

## Consequences

- Breaking for consumers: `openView("work")`, `openView("schedules")`, `openWork` and
  `registerWork`/`registerSchedules` are gone.
- The details pane needs the app to name the agent and the model. Desktop does for agent chats.
- An agent cannot yet change the execution mode on request. Raising its own authority needs a
  person-only approval, so it is a separate change.
- `welcome()` in `Thread` and the welcome `suggestions` of the page are removed; the app's
  `welcome` setting no longer draws anything.
