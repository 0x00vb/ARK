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
| Phone | Placed agents are not in `client/v1` yet |
| Chat history of a placed agent in the sidebar | History search reads this runtime's chats only |

## Consequences

An agent has one home at a time. Its work and chats after the move live on the target; the
chats before it stay here. The record here holds a copy of the agent's last known state, not a
second source of truth.

## Amendment (2026-11-09): folder, model and Stop on the other computer

Found from the owner's first move to a real second Mac: Stop looked dead, the Model row stayed on
"Checking…", and the Folder row offered this computer's folders.

| Item | Decision |
|---|---|
| Folder | A placed agent's folder is a folder on its computer. `agent_folder` routes there (resource op `agent_folder`); that computer checks the path. Rows carry its `folder` and `own_folder`. The page asks for a typed path: Core has no remote folder browser |
| Model | `agent_move` keeps the agent's model when the target offers it or does not answer within 3 s; otherwise the agent gets the target's default |
| Stop | `cancel_task` asks the task to stop and answers within 3 s, `{stopping: true}` while a step that cannot be cut short still runs. A link call times out at 8 s, so waiting for the task to end made Stop fail |
| Old target | A target without the `agent_folder` op shows the project folder; a change there fails with that computer's error |

Authority of the `agent_folder` resource op: it takes only a project reference to a chat this
runtime owns (a terminal or file reference is refused; another runtime's location finds no
chat). It reaches only the agents of that chat's project, by name. It reads the agent's folder,
sets it to an existing absolute folder on that computer, or gives the project's back; it never
changes the project's own folder, files or other agents. Over a link it arrives as
`chat_resource` for a held chat on a computer the person linked. Tests:
`the_agent_folder_operation_stays_within_its_chats_project`, `an_agents_folder_is_a_full_path_or_null`.
