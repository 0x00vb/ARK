# 0124. An agent chat's files are in the agent's folder

Status: Accepted. Date: 2026-11-16. Amends [0115](0115-an-agents-details-hold-its-setup.md) (folder per agent) and [0121](0121-pictures-go-with-an-agent-chat-message.md) (the picture folder). Builds on 0109 (an agent has many chats).

## Context

The owner (2026-11-16) opened a csv and documents from an agent chat after changing the agent's folder and got "No such file or directory (os error 2)".

An agent chat is a session in the agent's own scope project (0109). The agent's tools resolve files in `workspace_dir(scope)`, the agent's own folder (0115). The page never gave that session to a file call. Desktop passed the project chat's location, so the host took `workspace_dir(project)`, the top folder, for every link, card, thumbnail, file list and upload. `resource_project` refused an agent session ("the resource's project does not match its chat"), so the page could not name it. Files the agent wrote in its folder were read from the wrong place.

## Decision

| Part | Decision |
|---|---|
| Wire | No new field. A file location is `{ runtime, project, session }`: `project` is the project the window shows, `session` may be the id of one of its agents' chats |
| Authority | `ark_orchestrate::agent_chat_scope(engine, project, session)` reads the store only. It answers the session's project when that project is the scope of an agent of `project` (not ended), the scope's parent is `project`, and the session is a conversation (not a work item's, an automation's or an execution-managed one). Anything else keeps the refusal: another project's agent, a work item's session, an ended agent, an unknown chat, another runtime. The page sends a chat id, never a path |
| Host | `resource_project` accepts such a session for a file resource only and still answers the shown project. Any other kind (project, terminal, artifact, process, ...) named with an agent chat session is refused as before. `resource_scope` answers the agent's scope for a file resource. File, upload and `file_restore` operations use `workspace_dir(scope)`; their subscriber keys and `files_changed` use the scope, the key the agent's own writes belong to. An agent without a folder of its own walks to the project folder (0115): the same root as before |
| Missing file | A file operation on a path that is not there says "<name> is not in <folder>" (the folder's name), not the operating system's words. An agent folder that is gone already says "<dir> is not there" |
| Pictures | Admission (`submit_task`, `work_admit` to an agent, delegation checks) and the turn start read the owner's scope, not the orchestrator project. A picture that is not in the agent's folder when the turn starts fails the task with "The picture <path> is not in <agent>'s folder." A picture that is there but is another file keeps "changed or was removed after it was sent". A new chat with no agent (General) keeps the project folder |
| Flag | `hello` answers `"agent_chat_files": true`. A runtime without it refuses an agent chat location. `ark.at(location).agentChatFiles()` is true only for the page's own runtime with the flag, false for another computer (the page does not read that runtime's hello), as `taskImages()` is |
| Page | `chatView` has a `fileLocation` option, apart from `location`. Its file calls (`files`, `file_*`, `upload_*`) and `openFile` go to `base.at(fileLocation())`. Every other call, and the turn's own location, keep the window's |
| Desktop | The agent chat's `location()` adds the chat's session when the flag is true. Attaching a file in a new chat starts the chat (`agent_conversation_start`) before the upload, as a first send does, because the upload needs a chat to name the agent. The chat has no turn, so no list shows it until its first message. If the upload fails, the started chat stays and the next send uses it (one start for both) |

## Alternatives

| Option | Result |
|---|---|
| A `folder` or `agent` field on the resource reference | Widens the protocol every client reads. The session already names the agent |
| The page sends an absolute path | The page would name paths the host must then trust or check |
| Move the files when the folder changes | 0115 keeps an agent's earlier files where they were written. A move is a store migration |

## Consequences

- This widens what a chat id can open: from the project's folder to the folder of that project's agent. The agent's folder is one the person chose with `agent_folder` and the agent already works in.
- Files written before the agent's folder changed stay in the old folder (0115). A link to one now says "<name> is not in <folder>". There is no fallback to the old folder.
- A queued task with a picture admitted from the old folder fails at its start with the named message.
- A chat of an agent on another computer keeps today's location until the page can read that runtime's hello. An older runtime refuses the new location, so the page never sends it there.
- The card of a file the turn removed keeps its "Removed" state. The "<name> is not in <folder>" message shows when a file is opened (a link, a card or a tab), whether the turn wrote it or not.
- A group's member thread is an ordinary conversation of that agent (`start_thread`), so its chat names that agent's folder like any of its chats. A group has no session of its own in an agent's scope.
- The phone (`ark-ios`) sends no file call by chat location.
