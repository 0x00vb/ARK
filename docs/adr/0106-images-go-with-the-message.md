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
| `send`, `chat_send` | New `images: [project path]`, at most 8, each at most 3.5 MB and 20 MB together, PNG, JPEG, GIF or WebP. A provider counts an image as base64 (a third more): Anthropic refuses one over 5 MB and a request over 32 MB. The host checks each file's size, then reads it with `ark_files::media` (inside the project folder only, links out refused), off the async runtime. It refuses the message, starting no turn, if one is not an image or is too big. |
| Large images | The page makes an image over 3.5 MB smaller before it sends it: a PNG of at most 1568 pixels on its longer side, or a JPEG when that is still too big. The project keeps that smaller file. |
| Model | The images ride on the turn's first message (`TurnOptions::pictures`, `Harness::pictures`). A model that does not read images is told in words and sees none. They are in that turn only: the next turn's carry-over is text. |
| Record | The stored turn lists `images` (project paths). The conversation shows them above the message, also after a reload and on another window. |
| While the agent works | A message with images queues. Steering carries text only. |
| Chats | Pictures wait with the chat they were put in: another chat shown in the same composer does not carry them. A message that sends nothing (a failed send, no model chosen) puts its pictures back, and puts its words back only if the person typed them. |

## Consequences

- The file stays in the project when the person removes the picture from the composer, as any
  dropped file does.
- A later turn does not see the image again. It is in the project, and the agent can name it.
- A runtime older than this change answers `upload_add` without `as`: the page then names the file
  in the draft, as before.
- `ark_runtime::session::Turn` has a new field, `images`. Code that builds a `Turn` by hand adds it.
