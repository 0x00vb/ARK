# 0076. An app's conversation uses the chat's own view

Status: Accepted
Date: 2026-10-17

## Context

ARK Desktop shows one agent's work as a conversation in the side pane: its tasks, what it did,
its results and a composer to ask it more. Desktop drew that pane itself (`dk-chat-*` classes,
its own markdown styles and composer). It drifted from the chat. Measured on 2026-10-17 with the
same answer in both (1440 px, light):

| Element | Chat | Agent pane |
|---|---|---|
| Message bubble | 15 px, padding 9/15, radius 20 | 13.5 px, padding 9/12, radius 14/14/4 |
| Answer prose | 15 px / 24.9 px | 13.5 px / 21.6 px |
| Code block | 12.5 px, padding 12/14, language label | 12 px, padding 10/12, no label |
| Quote, table | rule, muted colour, row lines | browser defaults |
| Tool work | folded work block, one row per call | a dotted line per event, call and result apart |
| Composer | rounded card, controls below | flat strip, caption above the input, accent send |

The cause is two rendering paths. A fix in one never reaches the other.

## Decision

The page gives apps the chat's own view: `ark.chatView(el, { submit, stop, call, placeholder })`. It builds
the same `Thread`, `TurnView` and `Composer` the main and split chats use, in the same markup,
so app.css styles it as a chat. The app projects its records into turns and calls `render`.

| Part | Behavior |
|---|---|
| Turns | `chat_read`'s transcript shape (`input`, `activity`, `answer`, `unfinished`, `cancelled`, `footer`), plus `key`, `note`, `working`, `status`, `after` |
| `key` | Only a turn whose content changed is drawn again, so prose does not fade in on every live event. A turn that arrives after the first draw rises in like a chat's new turn |
| `note` | A line under the person's message (`.user-note`): who asked, when it was not the person (another agent, a schedule), or that a message waits for the next task |
| `working: { label, since }` | Work still going: the work block stays open and the chat's working line counts seconds from `since`. No "esc to stop": Esc does not stop an app's run |
| `status` | A muted line for work that waits (queued, paused, waiting for an answer). `unfinished` stays the failure line; a turn has one or the other |
| `after` | The app's node under the reply (an agent's produced files, a retry link) |
| `streaming` | The text being written now, drawn as the chat's streaming prose. It grows in place: a longer `streaming` on an unchanged turn does not draw the turn again. Text that is not the drawn text continued (taken back) draws it again |
| `approvals` | Questions the run waits on (`ark:approval` messages, `ark.approvals()` for those asked before the app's code ran), drawn in place with the chat's card and answered with `approve`. The tray keeps its own card; an answer anywhere closes both (`approval_closed` removes every card with that id). A new card takes the focus only when the focus is already in the view |
| Composer | No model, mode or file controls, and no slash commands. Shift+Tab is not captured. No queue: typed text always goes to `submit`. Dictation stays. `setComposer({ placeholder, disabled })` says why it is unavailable in the placeholder. With `stop`, `setComposer({ running: true })` makes the send button the chat's Stop while nothing is typed |
| `actions` | A slot beside Send for the app's own control (Resume, Restore). While it holds one, Send is hidden |
| Tool rows | With `call(key, id)` (the turn's key, the call id), a tool row opens on its call as in the chat; the app returns `task_call`'s shape. A row the person opened stays open when its turn is drawn again. A detail that failed to load is asked for again on the next click |
| Scroll | Sticks to the end like the chat; scrolled up, the turn in view keeps its place across redraws |

A missing control is `null` in `Composer`'s elements, never a fallback to the main chat's
element by id. `.app[data-view]` hides only the main column's scroller and dock, not a chat view
in the side pane.

## Alternatives

| Option | Why not |
|---|---|
| Desktop imports `js/conversation.js` and `js/composer.js` | Internal modules with no contract; `Composer` needs ten elements and a chat-shaped API |
| Copy app.css rules into Desktop | Two copies drift again |
| A view per kind of record (`ark.taskView`) | Names an orchestration concept in the page API; one conversation view serves any app that projects runs into turns |

## Consequences

- An app's conversation changes with the chat: one place for layout, type, markdown, work
  blocks, composer, dark mode and narrow windows.
- Agent-specific behavior stays in the app: Desktop keeps the agent's head (name, state, Runs
  with), maps Enter while the agent works to "add to its current work", and decides which task
  Stop stops.
- The host gives an app what it needs to draw an orchestrated task as a chat: an agent's
  streamed text as `{kind: "orch_text", project, task, agent, text}` (live only: not numbered,
  not kept for replay, not sent to native clients), `task {task}` with the task's `feed` (its
  stored events in the `orch` shape, the last 2,000) and `task_call {task, call}` (one call's
  detail, the chat `call` shape, looked up only in that task's events).
- Streamed text is not stored or replayed: a task drawn from `task` after a reload shows its
  steps and answer, not the text it wrote between them, and a window that opens while an agent
  writes shows its text from that point.

## Check

`crates/ark-ui/tests/page.rs` `an_apps_conversation_draws_exactly_as_the_chat`: the chat's turn
drawn through `ark.chatView` has the same element sequence and the same computed styles for the
bubble, prose, code, work head, tool row, footer, composer, input and send; an unchanged turn is
not drawn again; a working turn keeps its block open with no Esc hint; the composer has no chat
controls and Shift+Tab leaves the chat's mode alone; failed, stopped and waiting turns, a refused
composer and a redrawn turn in view keeping its place. `an_apps_conversation_behaves_as_the_chat`
runs a real orchestrated agent: its text grows in the same element without drawing the work
block again, its question shows in its turn and in the tray and an answer in the view closes
both, a turn drawn from `task` keeps its work block and a tool row opens on `task_call`, and the
send button is Stop while nothing is typed and stops the task. A question answered in the tray
leaves the view too. `tests/orchestration.rs` `an_agents_task_reads_as_a_chat` checks
`orch_text`, `task`'s feed and `task_call`. Desktop's `ui/tests/agent-chat-parity.test.mjs`
fails if the agent pane draws its own messages or Desktop CSS restyles the chat's elements. It
reads source and a fixed class list, so it is a first guard; the computed-style comparison is
ARK's page test.
