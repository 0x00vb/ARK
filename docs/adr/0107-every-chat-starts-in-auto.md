# 0107. Every chat starts in Auto, and Auto asks only what asks at every call

Status: Accepted
Date: 2026-11-07

Amends [0064](0064-execution-mode-is-per-chat-and-per-turn.md) and [0103](0103-a-calm-chat-page.md).

## Context

The owner's chats stopped and waited on permission questions. Two causes:

| Cause | Where |
|---|---|
| Auto ran only actions marked `auto_eligible`, which is file write and edit. Shell and code runs, processes, connected services and every other change still asked in Auto. | `harness.rs`, 0064 |
| A chat started in Manual. Only the page switched a new chat to Auto, once, through `chat_execution_mode`, and the first send waited for it. A split chat, the terminal and any other client stayed in Manual. | 0103, `app.js` |

## Decision

| Part | Now |
|---|---|
| Default | A chat nobody chose a mode for is Auto. A new record starts Auto at revision 0. `Session::load` reads a stored revision 0 as Auto, whichever mode an older version stored. A person's choice (Plan or Manual, asked for in words through `mode.set`, ADR 0104) has a revision above 0 and stays. |
| Auto | Runs every state-changing action without a question, except an action that asks at every call (`ask_each_time`, which `person_only` implies). With nobody to ask (a one-shot client, `ask: false`) it runs only `auto_eligible` actions, as before: that client's contract is that a change it cannot ask about is refused. |
| Page | No step before the first send. `adoptAuto` and the wait are gone. |
| `auto_eligible` | Stays, for runs with nobody to ask: an automation may run only what is project-bound or contained (`automations.rs`, `authority.rs`). A chat in Auto does not read it. |
| Not changed | The harness default (Manual) for orchestrated agents and automations, grants, the guard of a vendor CLI, Plan. |

## What still asks in Auto

| Action | Why it keeps asking |
|---|---|
| Image generation | It spends money. |
| Computer (look at, open, click, type in the person's own apps) | It acts on the person's own screen and accounts. |
| Acquire a capability | It installs or connects something. |
| Schedule, change or remove standing work | Text the chat read (a page, a mail) must not set up work that runs unattended (ADR 0045, `tools.rs`). |
| `mode.set` | Only the person may widen or narrow authority (ADR 0104). |

To stop asking for one of these, drop `ask_each_time` from its descriptor. That is the whole change, and it is left to the owner.

## Consequences

- A chat the person never set asks nothing but the list above. A prompt injection in a file or page can now make the chat change files and run code in the project without a question. That is what Auto meant to the owner, and the sandbox, the project folder and the list above stay the limits.
- Actions that reach outside the project and are not on the list now run without a question: sending mail or posting through a connected service, starting a process that runs as the person. Connecting the service still asks once. This is what "never ask" means; adding `ask_each_time` to a descriptor puts one back.
- A person who wants questions says so ("ask me before changes"). The mode is stored with revision 1 and every state-changing action asks again.
- Older chats opened after this change are Auto if nobody chose, so they stop asking too.
- `chat_execution_mode` stays as a call for other clients. The page does not use it.
