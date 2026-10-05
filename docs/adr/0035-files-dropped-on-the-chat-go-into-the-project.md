# 0035. Files dropped on the chat go into the project

Status: Accepted
Date: 2026-10-05

## Context
For data work, the person must give ARK workbooks and exports (ADR 0033). Until now a file
reached ARK only when the person put it in the project folder in Finder or Explorer. In the
cloud there is no such folder, so a file could not reach ARK. People expect to drop a file on
a chat window.

ADR 0030 already sends files to a runtime in parts (`upload_parts`). That works for a runtime
on the same computer and for one elsewhere. Then it makes a new project of the files.

| Option | Why not chosen |
|---|---|
| Attachments on the message, sent with `send` | A second path for bytes; the agent would read them from the message, not from the folder its tools work in |
| The window writes each file with `file_write` | Text only, one call per file, no large files |
| A Tauri file drop that copies on the desktop | Only for ARK Desktop and only for a runtime on the same computer; the cloud gets nothing |

## Decision
- A fourth upload method: `upload_add {upload}`. It moves the upload's files into the open
  project's folder (`Engine::workspace_dir`) and answers `{added: [paths]}`. Each file or
  folder at the top of the upload keeps its name. If the name is taken, it gets a number
  (`sales 2.xlsx`, `exports 2`); files are never overwritten. A second call answers the same
  (`uploads/<upload>.done`), and an upload made into a project cannot be added again.
- A move from one disk to another (a project folder the person chose on another disk) copies
  the files, then removes the upload.
- After the move, the host sends `files_changed {project, paths}`. Open file views and the
  Files panel read the files again. A restore (ADR 0034) sends the same event.
- The window: dropping files or folders on the conversation or the composer adds them to the
  project. The page reads each file in parts of 2 MiB and sends small files together in one
  call, through the same `upload_parts`. A line above the composer shows the progress, with
  Stop (`upload_cancel`). When the files are in the folder, the composer gets the line
  `Added to the project: sales.xlsx, orders.csv` under what the person wrote. The cursor stays
  above that line, so the request comes first and names the chat. The person sees the line,
  can change it, and sends it with the request. So the agent learns of the files from the
  message the person sent. No new field on `send`.
- ARK sets no limit on size or number of files. An app that needs a limit sets it in its page
  or its host.

## Consequences
- The same function works with a runtime on this computer, a self-hosted server and ARK Cloud.
- A drop is not a turn: the files are in the folder before the person sends anything. A stopped
  drop leaves nothing in the project.
- If moving one top-level entry fails, the ones moved before it stay in the folder. The window
  shows the error, and the files already moved are complete files.
- ARK Desktop must stop its window from taking the drop itself (Tauri `dragDropEnabled: false`).
  Otherwise the page never gets the drop.
- The terminal has no drop. There, the person copies files into the folder that `/folder`
  shows.

## Check
- `src/upload.rs` (`files_added_to_a_project_keep_their_names_or_get_a_number`): names in use
  get a number, folders keep what is inside them, a second call answers the same, and paths
  stay inside the upload.
- Page test `drop`: two files dropped, one with a name in use and one larger than a part.
  They arrive with the same bytes, and the composer names them.
