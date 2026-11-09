# 0110. The sidebar lists agents; chats are found from the agent

Status: Accepted. Date: 2026-11-09. Supersedes 0077. Amends 0103 (rail pins).

## Context

The owner asked for an agent-first window: agents in the sidebar, projects as optional groupings
with no label, many chats per agent with a compact recent-chat menu, a prominent New chat and
`/history`, and agents that recover earlier discussions without carrying whole histories. Their
store held 6 projects, about 260 chats and 8 agents. The owner chose that every existing chat
becomes a chat with ARK, the built-in assistant.

## Decision

| Part | Decision |
|---|---|
| Sidebar | ARK first, then pinned agents, then the most recent, as tiles then rows. No project tree, no chat rows. A folder name shows only when two agents share a name |
| Click | Opens the agent's latest chat in its own folder; the window switches project without a step |
| New chat | ⌘N, the button and `/new` start a chat with the current agent. An agent on an agent program keeps its one chat |
| Recent chats | The title chip's title opens the last 8 chats, New chat and All chats |
| `/history` | Shared command. The window opens the search overlay scoped to the agent; the terminal shows the report |
| Folders | The details pane's Folder row (change, rename, copy path, app project actions) and the palette (Open folder, Upload a folder, Show a hidden one again). Creating a named project with no folder is cut |
| Recall | `conversations.search` and `conversations.read`, offered only when the agent's scope holds another chat. Word match over titles, gists and stored turns; at most 5 hits and 2,000 characters; one chat read at most 4,000 characters. Nothing enters the context unless the model asks |
| Extension points | `ark.rail` and `ark.projectSection` are removed and throw a named error. `ark.sidebar`, `ark.newChat`, `ark.action({where:"project"})` (now in the Folder menu) and `agentConversation` views (params gain `project` and `conversation`) stay |

## Alternatives

| Option | Result |
|---|---|
| Each project becomes an agent | Turns folders into identities. Not chosen by the owner |
| Chat rows under each agent | The owner wants them out of the sidebar for now |
| Embeddings for recall | A new dependency and a model call; word match over bounded stored text first |

## Consequences

- Breaking for apps: `ark.rail`, `ark.projectSection`, the sidebar Boards section (the board panel and palette remain).
- Hidden projects stay hidden: their agents and chats are left out until shown again.
- Recall adds tool definitions only for agents with more than one chat; the simulated benchmark is unchanged.
