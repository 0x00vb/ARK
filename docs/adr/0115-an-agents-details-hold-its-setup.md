# 0115. An agent's details hold its setup; an app may list only its agents

Status: Accepted. Date: 2026-11-09. Amends 0110 (the sidebar lists ARK first), 0112.

## Context

The owner (2026-11-09): change an agent's folder and its name ("don't call it title, it's
name"); where it runs and its model belong in the agent's details, "so we can have a clean
composer"; "there shouldn't be a default agent ARK, it's the ones users create".

## Decision

| Part | Decision |
|---|---|
| Details pane | Rows: Name, Model, Computer, Folder. The face picks the pet when the app gives `identity.pet`. `identity.computer: { name, open(anchor) }` is a new row. The Title row is gone: Name renames what `identity.rename` names (the agent, or the chat when there is no agent) |
| Folder per agent | `Engine::workspace_dir` takes the nearest folder up the project chain, so an agent's own scope project may hold its own folder. Window call `agent_folder {project, agent, path or null}`; `agents {all}` rows carry `folder` and `own_folder`. Other agents of the project keep its folder; the agent's chats stay where they are |
| Composer | An app's conversation without `model` has no model chip; Desktop's agent chats pass none |
| No built-in assistant | `ark.newAgent({ open, assistant: false })`: the sidebar lists only the app's agents and groups. At start the latest one opens; with none, the window shows one New agent button (`open({ location, anchor })`). New chat with no agent in view makes an agent. The assistant's old chats stay reachable from Search |
| Groups in the sidebar | When an app registers a view with `groupConversation: true`, the sidebar lists `groups` (ADR 0113) as tiles with two members' faces; a tile opens that view with `{ id }`. Turns of `ark.chatView` take `author: { name, avatar(size) }` |
| Phone | `agent.create` takes optional `title` (120) and `look` (64). A deleted agent and its chats are absent from `/client/v1/agents`, `/chats` and `/home` (test) |

## Alternatives

| Option | Result |
|---|---|
| Move an agent into another project to change its folder | Its chats, tasks and outputs are project records; moving them is a store migration. A folder of its own needs one line in `workspace_dir` |
| Remove the assistant from Core | Other apps (writing-agent, the terminal) are built on it. An app option keeps them unchanged |

## Consequences

- An app that opts out of the assistant gets no assistant tile, `/new` with no agent makes an agent.
- An agent with its own folder works there for every chat; its earlier chats keep their files where they were written.
