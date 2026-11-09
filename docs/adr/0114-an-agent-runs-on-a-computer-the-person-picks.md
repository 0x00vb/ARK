# 0114. An agent runs on a computer the person picks

Status: Accepted. Date: 2026-11-09.

## Context

A chat can run on another computer (ADR 0052). The owner wants the same for agents: pick which
computer or server an agent runs on, from the agent's details pane.

## Decision

| Part | Decision |
|---|---|
| Call | Window call `agent_move {agent, project, place}`. `place` is a link name, or null for this computer. `project` is this runtime's top folder |
| Move | Spawn the agent's spec on the target (`SpawnAgent` on a chat held there), keep a `placed_agent` record here, then end it here (as `agent_end`; its old chats stay readable). Moving back spawns the remote spec here, ends it there and drops the record |
| Retry | Safe at any step: an agent already on the target is not spawned again, a record for the same place only finishes ending it here, an agent already back is not spawned again |
| Failure | A target that does not answer fails closed (`<name> is not answering`) before anything changes. A project too large to send without asking stops the move with a message |
| Anchor | A chat held on the target (ADR 0052) names the project replica there. It is reused, else made. Operations on the agent cross as the existing resource operations through it, so no new route exists |
| Listing | `agents {all}` adds one row per placed agent, with `place`, `session` (the anchor) and `location {runtime, project, session}`. A reachable computer's own record of the agent refreshes it. One that does not answer leaves the row as remembered. The phone's `/client/v1/agents` lists only this runtime's agents |
| Moving between two computers | Back to this computer first, then to the target |
| Delete | `agent_end` on a placed agent ends it there and forgets the record |

## Not decided

| Item | Why |
|---|---|
| The agent's own folder | Not sent; the agent works in its project's copy there |
| Phone | Placed agents are not in `client/v1` yet |
| Chat history of a placed agent in the sidebar | History search reads this runtime's chats only |

## Consequences

An agent has one home at a time. Its work and chats after the move live on the target; the
chats before it stay here. The record here holds a copy of the agent's last known state, not a
second source of truth.
