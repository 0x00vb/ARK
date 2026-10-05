# 0030. A folder sent to a runtime elsewhere becomes a project there

Status: Accepted
Date: 2026-10-05

## Context
A runtime can run on a different computer (ADR 0016). The window can open it (ADR 0029). A
person has projects on their own computer. They must be able to copy one of these projects to
the runtime elsewhere. ARK Desktop uses this for "Copy to Cloud" (`ark-cloud/docs/CLOUD.md`,
D2). A self-hosted server needs the same function.

The protocol had no method that writes bytes. `file_write` writes text into the current project
only. The runtime had no method that makes a project from files.

| Option | Why not chosen |
|---|---|
| One call with an archive of the folder | The body limit is 32 MiB. A large call can fail after a long time and must start again |
| A new HTTP route for uploads | A second transport path. The page, the window and `Client` use only `call` |
| Write into a new project file by file | A copy that stops leaves a project with part of the files. ARK has no "delete project" |
| The cloud control plane writes the volume | It couples ark-cloud to the store format (ark-cloud ADR 0008 rejects this) |

## Decision
- The `Host` has three methods. They do not wait for a running turn.

  | Method | Does |
  |---|---|
  | `upload_parts {upload, parts: [{path, data, offset}]}` | writes parts (base64, max 8 MiB in one call) into `<store>/uploads/<upload>/`. No parts starts the upload |
  | `upload_project {upload, name}` | moves that folder to a new project's folder in one rename; answers `{id, name}` |
  | `upload_cancel {upload}` | removes the upload |

- The client makes the upload id (8 to 64 letters, digits or `-`).
- At `offset` 0 a file starts again. Another offset must be the end of the file. A part that is
  there already (the answer was lost) is not written two times.
- `upload_project` stores the project only after the rename. A second call with the same
  upload answers the same project (`uploads/<upload>.done`).
- All paths stay in the upload folder (`ark_files::inside`). The runtime writes only files.
- A new name that is in use gets a number (`notes 2`).
- The runtime removes uploads older than one day when it starts.
- `ark_ui::remote::Folder::read(dir)` lists the files. It does not include links or names that
  are not UTF-8 (`skipped()`). `Folder::read_at_most(dir, files, bytes)` stops early with
  `None` above a limit.
- `ark_ui::remote::send_folder(client, folder, name, go_on)` sends calls of 512 KiB at most:
  one part of a large file, or up to 200 small files. It tries a call 3 times when the
  connection fails, then makes the project. It calls `go_on` after each call. If `go_on`
  answers false, or a call fails 3 times, it cancels the upload.
- `ark.action({ where: "project" })` puts an app's item in a project's menu. The item gets
  `{ project }`. A failure of the item shows as a toast.

```mermaid
sequenceDiagram
    participant A as App (this computer)
    participant R as Runtime elsewhere
    A->>A: Folder::read_at_most(dir, files, bytes)
    A->>R: upload_parts {upload, parts: []}
    R->>R: make <store>/uploads/<upload>/
    loop until all files are sent (512 KiB per call, 3 tries each)
        A->>R: upload_parts {upload, parts}
        R->>R: write the parts
    end
    A->>R: upload_project {upload, name}
    R->>R: rename the folder to <store>/workspaces/<id>, then store the project
    R-->>A: {id, name}
    Note over A,R: On an error or a stop: upload_cancel {upload}
```

```mermaid
stateDiagram-v2
    [*] --> Started: upload_parts (no parts)
    Started --> Started: upload_parts
    Started --> Project: upload_project
    Started --> [*]: upload_cancel, or one day old at a start
    Project --> Project: upload_project again (same answer)
    Project --> [*]: answer one day old at a start
```

## Consequences
- The project shows only when all files are there. A stopped copy leaves no project.
- The copy contains the folder only. Chats, agents and traces stay on the first computer.
- Empty folders and links are not copied.
- A folder of many small files needs few calls. Each call opens a new connection (the runtime
  closes each one), so a call carries as much as it can.
- A token holder can write files on the runtime. This is not a new permission: the Terminal
  panel can do the same (`REMOTE.md`, What a token allows).
- The limits of a copy (size, number of files) are for the app to set. ARK sets only the size
  of a call.

## Check
`crates/ark-ui/tests/serve.rs` (`a_folder_sent_from_another_computer_becomes_a_project_there`):
a stopped copy leaves nothing, a full copy has the same bytes, a name in use gets a number, a
path out of the folder is refused, an empty folder is a project, `read_at_most` stops early, a
second `upload_project` gives the same answer. `src/upload.rs` tests parts sent again, a bad
part in a call, and the ids.
`tests/page.rs` (`an_apps_action_on_a_project_is_in_its_menu`): the project menu item.
