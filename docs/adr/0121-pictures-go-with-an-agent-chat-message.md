# 0121. Pictures go with an agent chat message

Status: Accepted
Date: 2026-11-15

Amends [0106](0106-images-go-with-the-message.md) and [0109](0109-an-agent-has-many-chats-that-run-at-once.md).

## Context

ADR 0106 sent pictures with `chat_send`, the assistant chat. A named agent's chat (0109) sends a
task (`submit_task`, or `work_admit` for a new chat) that carried text only, and the page of
Desktop's agent chat did not attach images. The owner dropped a screenshot in an agent chat and
the composer showed only `Screenshot...` text.

A task is queued and may start later, also after a restart. 0106 read the files when the message
was sent. A queued task cannot hold bytes, so a read at send does not fit it.

## Decision

| Part | Decision |
|---|---|
| Call | `submit_task` and `work_admit` take `task.images`: project paths, at most 8, 3.5 MB each, 20 MB together, PNG, JPEG, GIF or WebP, as in 0106. `message_agent` takes none: steering stays text only. The idempotent form of `submit_task` accepts `images` and compares them on a retry |
| Who | Pictures belong to a message the person sends from their own chat: the task is `person`, not `group_turn`, not `reply`, no `report_to`, no `work`, origin External. Delegation, `agents.start`, schedules, automations, group turns and results posted into a chat refuse them. One gate: `Inner::admit_images`, run by every path that makes a task or a work submission |
| Reader | One reader for `chat_send` and tasks: `ark_files::images(root, paths)`. Size before read, inside the project folder only. Its messages are those of 0106 |
| Admission | The orchestrator reads each picture when it admits the message and records `path`, `sha256` and `size` on the task (`TaskRecord.images`) and on the work submission (General target). A refused picture admits nothing |
| Start | When the turn starts, the orchestrator reads each file again and compares sha256 and size. A file that is gone or changed fails the task (General: the `work_execute` call) with "The picture <path> changed or was removed after it was sent." The model is shown the bytes that were checked, so the picture drawn in the chat is the file the model saw |
| Folder | The project's folder, the one the upload put the file in (`Engine::workspace_dir` of the orchestrator's project), also for an agent with a folder of its own |
| Model | `Harness::pictures` with the task message, `Session::begin_turn_with` stores the paths with the turn. A model that reads no images is told in words (0106). A new chat (General target) starts its turn with `TurnControl` attachments, as `chat_send` does |
| Programs | An agent on a program is sent no picture. Its brief lists `ATTACHED IMAGES` with the paths and the program reads them from the folder |
| Graph | `TaskNode.images` lists the paths, so the agent chat draws them on the turn after a reload |
| Records | `TaskSpec.images`, `TaskRecord.images`, `Submission.images` and `TaskNode.images` are `#[serde(default)]` and are not written when empty. Stored records load. `TaskSpec` has a new public field: a minor version, with a `Breaking` entry |
| Feature flag | `hello` answers `"images": true`. A runtime without it ignores `images` without saying so, so a client attaches pictures only when the runtime that holds the chat's project says it takes them. A chat asks `ark.at(location).taskImages()` with its own location: true only when that location is the page's own runtime and the page's hello has the flag, false for a project on another computer (the page does not read that runtime's hello). `ark.taskImages()` alone answers for the page's own runtime and is not a test for a chat |
| Page | `chatView` passes `{ images, named }` to `submit` and puts a message it cannot take back into the composer with its pictures. It exposes `addImages`, `stashImages` and `restoreImages`. A dropped image that stays over 3.5 MB after the page made it smaller is not attached: a message names it, and it goes into the project as a file |

## Consequences

- Each request of a task re-sends the task message with its picture (the 0106 layout, a pinned
  message). `PICTURE_TOKENS` counts it per request. With a provider that caches the pinned
  message, the picture is read from the cache after the first request. Measured with the mock:
  every request of a task carries its pictures once.
- The file stays in the project. A later turn of the chat does not see the picture again.
- A picture removed or edited while its task waits fails that task. The person sends it again.
- A project on another computer keeps the "Added to the project" line until the page can read
  that runtime's hello.
- The phone (`ark-ios`) does not send pictures to a task. A task with pictures shows in its list
  as text.
- A page reload loses a draft's pictures; its text stays.
