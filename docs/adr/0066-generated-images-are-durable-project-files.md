# 0066. Generated images are durable project files

Status: Accepted
Date: 2026-10-12

## Context
`Provider::Completion` carries text and tool calls. Some model APIs can also return generated
images. ARK already has one image path: a project file is classified by `ark-files`, appears on
the turn and opens in the shared image view (ADR 0021). Putting base64 or provider URLs in a
transcript would create a second, temporary path. URLs can expire or track the viewer, and base64
would make turns, events and logs unbounded.

## Decision
An image-capable reasoning provider adds `image.generate` to that turn. It is a normal mutable
action because it spends money. Its result is a normal produced project file. ARK validates the
raster bytes, writes them atomically to a collision-safe `generated-image-N.png`, and stores only
the relative path through the existing invocation and turn-file record. The chat uses the image
file card itself as an inline preview. Activating it opens the existing image view beside the chat.

| Question | Decision |
|---|---|
| Capability | First slice: OpenAI API mainline `gpt-5` and newer models, matching OpenAI's documented Responses image-tool support. ARK offers a provider-backed `image.generate` action. Other providers and models get no action and keep current behavior. |
| Provider request | `image.generate` calls OpenAI's `/images/generations` with one prompt, `response_format: b64_json`, and `ARK_IMAGE_MODEL` or `gpt-image-2.5-flare`. Provider URLs are never accepted. |
| Format and limits | One non-animated PNG, at most 25 MB, dimensions 1 through 8192 on either edge and at most 32 megapixels. The complete PNG container must end in `IEND`. SVG and remote URLs are refused. |
| Durability | Validate the complete response before any write. Write a temporary file in the project, then persist without clobbering the first free generated name. The produced artifact and existing sniffer decide the kind once. |
| Presentation | The existing turn-file path reaches live and stored transcripts. The window turns an image file card into a keyboard-focusable thumbnail and routes it through `openFile`. The terminal prints the normal generated file path. |
| Failure | Invalid or oversized output is a non-retryable provider API error. No partial file, thumbnail or provider URL is shown. Because generation is a tool call, streamed answer text is not the result and needs no withdrawal. |
| Scope | Generation output only. Image prompting and multi-turn image editing need their own input contract later. |

## Consequences
- Generated images survive restart and remote runtimes through the existing project file and
  `file_media` paths.
- The image file card is the inline preview. There is no duplicate control, image-only transcript
  format or second viewer registry. Missing or unreadable files keep the named file card without
  a broken-image glyph.
- Providers may add native output parsing independently. Text-only providers need no changes.
- Generation cost remains part of the provider's reported usage. ARK does not invent an image
  price when an API does not report one.

## Check
- `ark-model` tests prove only supported OpenAI reasoning models advertise generation and decode
  bounded base64 output without accepting URLs.
- `ark-sdk` tests prove invalid media writes nothing and valid bytes become a collision-safe
  produced project file through the ordinary action path.
- `ark-ui` page tests cover inline loading, unreadable media fallback, keyboard focus, the
  accessible filename, and opening the existing image view. Existing responsive and theme tests
  continue to cover the shared file card, and the viewer revokes its object URL when replaced or
  closed.
