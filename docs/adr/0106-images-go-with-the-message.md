# 0106. Images go with the message

Status: Accepted
Date: 2026-11-07

Amends [0035](0035-files-dropped-on-the-chat-go-into-the-project.md).

## Context

ADR 0035 put every dropped file in the project folder and named it in the draft
(`Added to the project: Screenshot.png`), with no field on `send`. For an image that was wrong
twice. The composer showed a line of text and no picture. And no tool shows an image file to the
model, so the agent never saw a dropped image.

## Decision

| Part | Change |
|---|---|
| Upload | Unchanged: an image goes into the project folder through the 0035 calls. `upload_add` also answers `as: {dropped name: path}`, because `added` is sorted and a name in use gets a number. |
| Composer | A dropped, pasted or chosen PNG, JPEG, GIF or WebP is a small picture above the text, with a remove button. No line is written in the draft. Other files keep the 0035 line. A picture alone can be sent: its file name is the text. |
| `send`, `chat_send` | New `images: [project path]`, at most 8, each at most 5 MB, PNG, JPEG, GIF or WebP. The host reads them with `ark_files::media` (inside the project folder only) and refuses the message, starting no turn, if one is not an image or is too big. |
| Model | The images ride on the turn's first message (`TurnOptions::pictures`, `Harness::pictures`). A model that does not read images is told in words and sees none. They are in that turn only: the next turn's carry-over is text. |
| Record | The stored turn lists `images` (project paths). The conversation shows them above the message, also after a reload and on another window. |
| While the agent works | A message with images queues. Steering carries text only. |

## Consequences

- The file stays in the project when the person removes the picture from the composer, as any
  dropped file does.
- A later turn does not see the image again. It is in the project, and the agent can name it.
- A runtime older than this change answers `upload_add` without `as`: the page then names the file
  in the draft, as before.
- `ark_runtime::session::Turn` has a new field, `images`. Code that builds a `Turn` by hand adds it.
