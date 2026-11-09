# 0111. The phone lists agents and their chats

Status: Accepted. Date: 2026-11-09. Builds on 0109 and 0110.

## Context

An agent has many chats (ADR 0109). The phone showed one chat per agent, so a second chat could
not be read, started or written to from the phone. Protocol 1.10 phones must keep working.

## Decision

| Part | Decision |
|---|---|
| Agents | `GET /client/v1/agents` returns `{ "agents": [AgentRow] }`: every agent that has not ended, newest first. Field names are the runtime's own (`project_name`, `updated_ms`), not camelCase |
| Chats of an agent | `GET /client/v1/agents/{project}/{name}/conversations` returns `{ "conversations": [AgentConversation] }`, the default chat included (`default: true`). Unknown agent: 404 `agent_missing`. Both segments are percent-decoded and checked |
| Credential | Both routes answer 401 `signed_out` without a valid client credential |
| Capability | `agents.threads`, offered with `agents.conversations`. Protocol minor 10 becomes 11 |
| Start | Mutation `agent.conversation.start`, input `{ "project", "agent" }`. `target` is unused and stays empty. The receipt's `resourceId` is the new conversation id. A retry with the same counter returns the stored receipt. An agent on an agent program is refused |
| Send | `chat.send` with `target` set to a conversation id other than the default one runs the task in that conversation. When a task already runs there, the text steers it (effect `steered`) and no task is created. A program agent is refused |
| Chat list | Non-default conversations also appear in `GET /client/v1/chats`, one row each, with the conversation id as `id` and the agent as `agent`. Phones older than 1.11 show them as ordinary chats. This is accepted: the chat opens and accepts sends |
| One bad agent | An agent whose conversations cannot be read is logged and left out of `/client/v1/chats`. The others are listed |
| Schema | `AgentRow`, `AgentsList`, `AgentConversation`, `AgentConversations` and `AgentConversationStartInput` are in `protocol.schema.json`, with fixtures `agents.json` and `agent_conversations.json` |

## Alternatives

| Option | Result |
|---|---|
| Show conversations only through the new routes | Phones before 1.11 would not see work done in them |
| A new conversation id in `target` of the start mutation | There is no id before the start; the receipt carries it |

## Consequences

- Conversations with no turns are not listed until they have one, except the default chat.
- `/client/v1/chats` reads each agent's tasks once per agent. Fewer reads need a change in `ark-orchestrate`.
