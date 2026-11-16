# 0126. Colleagues started from a chat continue that chat when they finish

Status: Accepted. Date: 2026-11-16. Amends 0087 ("completion does not start the sender again") for the person's own chat with a named agent. Builds on 0118 and 0123.

## Context

The owner's traces of 2026-11-16: a named agent started three translators with `agents.start` and ended its turn with "When the agents report back, I'll combine their results". Nothing ran again until the person typed "continue". An earlier turn waited 2 hours. In the same session, a translator the agent created could not see the agent's files: an agent with no folder of its own walks up to the project's folder (0115), not to its creator's.

## Decision

| Topic | Rule |
|---|---|
| Who | Colleagues started with `agents.start` from the person's own chat turn (`own_chat_turn`) or from a continuation of one. Group, delegated, started, scheduled, `work.later` and automation turns keep 0087 |
| Results | Each colleague's result goes into the starting agent's inbox for that chat when it ends: "finished", "could not finish" or "was stopped", with the result as the message's context (data, not instructions), clipped at 8000 characters and scrubbed. `Mail.result` marks it |
| When | Once a batch has ended: every colleague one turn started is done, failed or stopped. No timer: one still working or waiting for the person holds the batch, and the person can stop it from its tile |
| Busy chat | Results never enter a running turn. A queued turn of the chat reads them in its brief. When a turn of the chat ends, the check runs again. One open task per chat, so one continuation at most |
| Continuation | A task of the same agent in the same chat, origin External, not `person`, `TaskRecord.continues` = the person's latest turn in that chat. It reads the waiting results in its brief and is asked to continue the person's last request with them |
| Bounds | At most 3 continuations after the person's latest message in that chat; a new message allows 3 more. A continuation counts in the request lineage of the person's turn (`conversation_root`), so the 8 starts per request cover the whole chain. If the person stopped every colleague in the batch, nothing continues |
| Mode | The chat's mode, by the check `work.later` already passes (0123): the turn it continues is this agent's own chat turn in the same chat. Otherwise Manual. An Auto turn already reads web pages directly; results stay quoted data. A scripted test showed a result can still drive actions in Auto (`chat_mode.rs`, the scripted model now answers a continuation): the same exposure as a page the agent reads |
| Tool text | `agents.start` from such a turn says the result comes back to this chat |
| Display | Nobody wrote the continuation. The phone gets only the agent's message (`client.rs`). `TaskNode.continues` lets an app draw no person message; `chatView` draws no person line for a turn with no input, images or note |
| Folder | `agents.create` from an agent with a folder of its own gives the new agent that folder when it has none. Agents the person creates are unchanged |

## Consequences

- Breaking: `Mail.result`, `TaskRecord.continues`, `TaskNode.continues` are new public fields. Stored records read as before.
- A result that waits past the cap stays in the inbox for the person's next message.
- After a restart, results already in the inbox wait until a colleague or a turn of that chat ends.
- Desktop must set `input: null` for a task with `continues` (its `ui/agent-conversation.js`), else it shows the continuation's prompt as the person's message.
- Not verified: a live model, the real window.

## Check

`crates/ark-orchestrate/tests/helper_wake.rs` (5 tests: one batch with a failed colleague, a held batch, stopped colleagues, the cap and its reset, the creator's folder). `crates/ark-ui/tests/web/post-message.test.mjs` (a turn with no input has no person line, WebKit).
