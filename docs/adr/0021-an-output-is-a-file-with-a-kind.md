# 0021. An output is a file with a kind; a kind has one view

Status: Accepted
Date: 2026-10-04

## Context
The owner wants agents to produce real outputs (tables, charts, reports, PDFs) that open inside
ARK, from any tool, not a chart UI for Python (2026-10-04): "tool, artifact, kind, one viewer
registry, ARK surface". Rules: the kind is decided once; no parallel viewer or artifact path;
what the agent is told ARK can show comes from the same source, not the base prompt; and
visualization is not hard-wired to images.

What existed after ADR 0019: `ark-files` with three shapes and a `kind` per format, the file as
the truth with snapshots carrying `meta.path`, file tabs choosing a view by kind (`VIEWS` in
`file.js`), the host sending the kinds (`hello.file_kinds`). Gaps found: the core sniffer saw
only text, JSON or binary (a PNG was stored as binary); a snapshot of a file opened in the raw
artifact view, not as the file; nothing could send a page an image's or a PDF's bytes; the
`present` tool offered views the window cannot show (`diff`, `structured`, `split`); card words
and the Markdown check were kept in the page beside Rust.

## Decision
`ark_files::Kind` is the key from a file to its view, and `ark-files` decides it once.

| Question | Decision |
|---|---|
| What a kind is | What the person sees: `text`, `markdown`, `sheet`, `document`, `pdf`, `image`. Each `Format` has one. A kind is not a shape: Markdown reads as text, a PDF as a document (its text, a heading per page), an image as `Content::Media` (type and size; models read no images today) |
| Who decides | `ark_files::kind_of(path)` for a file; `ark_files::Kinds`, a `Sniffer` the SDK always registers, for a stored artifact named by a path, whichever tool wrote it. A tool that makes files (`code.run`) returns bytes with a path and no kind |
| Where a view lives | One entry per kind in `VIEWS` (`file.js`): text, Markdown as a page (with the project's images it links to; Edit for the text), sheet, document, PDF (the browser's own viewer), image. Words for cards come from the host (`hello.kind_nouns`) |
| Snapshots | An artifact with a `path` opens as that file, as it is now (`openArtifact`), unless an app's `artifactViewer` matches it. App viewers stay lenses on stored artifacts (ARK Desktop's diagrams) |
| Bytes | `file_media {path}`: a PDF or an image as base64 through `call`, so the window and a browser on a remote runtime get it the same way. Other kinds are drawn from what they read as (`file_read`) |
| What the agent is told | `ark_files::shown()` lists kinds and extensions from `FORMATS`; `code.run`'s description carries it. `present` lists only the views the window has: `file` opens any kind |

## Consequences
- A new format of an existing kind (`.ods`, `.tiff`) is one `Format`. A new kind (slides, an
  interactive chart spec) is one `Format` and one `VIEWS` entry; tools, artifacts, cards,
  approvals and descriptions follow without change. A chart is an image today because that is
  what programs write, not because the model says so.
- PDFs and images are read only. Their edits are refused with words that say so.
- The PDF view is WebKit's or Chrome's own viewer. Headless WebKit does not draw it, so the page
  tests check that it loads, and the native window was checked by hand.
- `file_media` sends up to `MAX_BYTES` (25 MB) as base64 in one reply. Larger media would need
  a streamed route.
- No public `ark.kindView` yet: no app adds a kind today.

## Check
- `crates/ark-files/src/lib.rs`, `a_files_kind_is_decided_once_for_paths_and_stored_copies`;
  `format/pdf.rs` and `format/image.rs` tests (read, damaged files, edits refused).
- `crates/ark-ui/tests/page.rs` (WebKit), `a_run_writes_a_chart_and_a_report_that_open_beside_the_conversation`.
- `crates/ark-interact/src/labels.rs`, `a_run_asks_with_its_program_and_says_what_it_wrote`.
