# 0118. An agent answers later in the same chat

Status: Accepted. Date: 2026-11-13. Builds on 0025 (work reports into the chat that asked), 0109 (an agent has many chats that run at once), 0107 (every chat starts in Auto).

## Context

The owner (2026-11-12): an agent that says "let me look into it" and answers later, in the same chat, as its next message, with the person free to keep talking. ADR 0025 posts a task's result into a chat as a card with a title. ADR 0109 lets one agent run several chats at once.

## Decision

| Part | Decision |
|---|---|
| Tool | `work.later {brief}` (Observe tier). Offered on a chat turn only (`TaskRecord::is_chat_turn`): origin External, `person` (the person's own turn, phone included) or `group_turn` (set only by the group driver), and not a run, schedule, Work conversation or submission, not a board item's task (Give work), not itself work handed off by this tool, not an agent-program agent, and `AgentSpec.tools` allows it (default `*`). An SDK `submit` of a plain task is not offered it. Not offered on delegated, started, scheduled or automation tasks |
| What it does | Starts a new conversation of the same agent, marked `SessionRecord.background`, which the agent's chat list, Recent chats, a project's sessions and `/history` all leave out and a task in it: `objective` = the brief, `report_to` = the chat the turn is in, `work` = the turn, `reply` = true. One per turn (a second call changes nothing), at most 3 open per agent. The turn goes on and ends with the model's short reply |
| Data | `TaskSpec.reply` and `TaskRecord.reply` (bool, left out when false). `Post.kind` (`result` default, `message`), left out when `result`. A store from before reads as it did |
| Delivery | `report_back` posts `Post { kind: message }` with `post_once` when the task is done or failed, once. A cancelled task posts nothing (the tile's Stop). The next turn of the chat is told, once, `[you, later] <the message>` among the posts since its last answer. Posting fails if the chat was deleted: the result stays in the task record and nothing is shown |
| Approvals | The task copies the turn's origin and runs as the same agent with the same gate, so `allow_mutate && !sent_through_group` is what it was in the turn. It never takes `Origin::Agent{self}`: that would make `group_only_in(self, self)` true for a member and ask where the turn did not. The new chat takes the asking chat's execution mode (ADR 0107). Since 0123 the work reads the asking chat's mode when it starts (only for a person's own turn), so it asks and runs as that turn does. The lane's approval question shows where every agent task's does: the attention list and the tile's mark |
| Notification | Existing: `attention` lists a finished task that reports to a chat; Desktop and the phone read that list. For a `reply` task the line is the message (or the reason), not "Done: <brief>" |
| Page | A `message` post draws as an agent turn: `div.agent > div.prose.answer`, live inside `article.turn.message`. A failed one is a `notice`. No card, title or caption. A result post keeps its card |
| Group chat | A group turn runs in the member's thread, so the message lands there. The window host copies a `message` post whose chat is a member's group thread into the group as that member's next message |
| Stop | Cancelling the task ends it and posts nothing. A new message in the chat never cancels it. A Stop on the turn before the hand-off is made means no work is started; a Stop on the turn after it does not reach the work: the person stops that from the agent's tile (cancel by task id) |
| Secrets | The answer is scrubbed (`ark_store::scrub_text`, the scrubber the store applies to turns) before it is posted, and the attention line is scrubbed too. An empty answer is posted as a failed message, "It finished without an answer.", and the attention line says the same |

## Consequences

- `TaskSpec` and `TaskRecord` gain `reply`; `Post` gains `kind` (a struct literal adds `kind: PostKind::Result`, or uses `Post::new`). One more tool in the schema of person and group turns.
- A group turn that acts alone (`allow_mutate`, ADR 0113 open issue) hands work that also acts alone, after the turn has ended. That is the turn's own authority, not wider.
- `reply`, `group_turn` and `report_to` are task fields any client that can submit a task may set, as `report_to` already was.
- Not tested: posting into a chat that was deleted (the store cannot delete one); the code logs and leaves the result in the task record.
- The phone (client protocol, ark-ios) draws a message post as a result card until P5.
- The page has no reduced-motion rule for turns; a message arriving live rises like any turn.
- Not verified: a live model calling the tool at the right moment and not for a quick question.
