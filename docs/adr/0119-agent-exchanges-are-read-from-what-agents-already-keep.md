# 0119. Agent exchanges are read from what agents already keep

Status: Accepted. Date: 2026-11-13.

## Context

The owner (2026-11-12): agents talk among themselves; the person sees one quiet line in their chat
("2 messages with CMO") and opens the exchange on a click. Plan P2 of the agent team.

## Decision

| Part | Decision |
|---|---|
| No new store | An exchange is derived. Its messages are: a note (`agents.message`), a brief (`agents.start`, `agents.delegate`: the task's objective, its `Origin::Agent`) and the reply (the task's result once it is final, or a failed reply when it failed) |
| One stored field | The note's record (`orch_message`) keeps its `text` (clipped to 1000, as the live event already was). Notes saved before this have no text and are not shown. The graph's `MessageEdge` is unchanged |
| Where | `ark_orchestrate::said` reads the store alone (as `agent_threads` does). `ark_interact::exchanges` groups it: `lines` (one per turn of one chat that reached other agents) and `talk` (the messages between two agents). Both front-ends call the same read; the window calls are `exchange_lines` and `exchange_talk` |
| A chat's turn | The asker's task. A line belongs to the chat (conversation lane) of that task. Work a colleague started inside it is that colleague's line |
| Window | `ark.chatView` takes `exchanges: { project, agent, session? }` and draws a line under each turn whose key is `task:<id>`, and opens the view over its own surface. The ARK window's plain chat has no task id on its turns and draws no lines |
| Secrets | Note text is masked with the store's scrubber (`ark_store::scrub_text`, now public) before it is kept, and note, brief and reply text is masked again when shown, so keys and URL passwords never reach the window. A line names every agent asked and marks the ones whose reply did not arrive |
| One view, many chats | `exchanges` may also be a function returning that descriptor or null, read at each use; `view.refreshExchanges()` after the app switches agent or chat; null makes no call |
| Consent and writes | Nothing writes. Agents never post into the person's chat for another agent |

## Consequences

- One more field in a stored record, optional on read.
- A read costs one pass over the project's task and note records. The window asks at open and at most once a second while events come.
- Open: the task id on `TranscriptTurn` (the plain chat), a phone surface (P5), Desktop passing `exchanges` and setting `--pet-ink` per pet.
