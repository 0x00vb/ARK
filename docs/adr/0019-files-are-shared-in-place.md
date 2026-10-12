# 0019. Files: one file, edited in place by the agent and the person

Status: Accepted
Date: 2026-10-04

## Context
Office work arrives as files: spreadsheets (`.xlsx`, `.csv`) and documents (`.docx`). Agents
need to read, create and change them, and the person needs to see and change the same files
inside ARK, then let the agent continue from their version. Before this, ARK read text only:
the window's Files panel showed "not text" for an Office file, and each app wrote its own file
tools (ARK Desktop's `files.*`).

Three questions decide the design:
- **Where the truth lives.** The store keeps artifacts as content-addressed snapshots, which
  cannot change. A person edits a file; Excel opens a file. Two editable copies (a store
  artifact and a file) would drift.
- **How much of a file survives an edit.** Rewriting a whole workbook from a simple model drops
  what the model does not hold (styles, column widths, charts, merged cells, macros).
- **How formats stay out of the runtime and the UI.** Each new format must not add `if xlsx`
  branches to the runtime, the interaction layer or the page.

## Decision
A crate, `ark-files`: the project folder's file is the one source of truth, and three shapes
(`Content::Text`, `Sheet`, `Document`) stand between files and everything else.

| Question | Decision |
|---|---|
| Truth | The file in the project folder. The agent's tools, the window and every agent of a team read and write that file. A write is also stored as an artifact (a snapshot, with `meta.path`), for the record; a front-end opens the file, never the snapshot |
| Formats | One `Format` per family (`Xlsx`, `Csv`, `Docx`; anything else that is text). A format reads bytes into a shape, applies `Edit`s to the bytes, and creates a file from a shape. Adding a format is one entry in `FORMATS` |
| Fidelity | Edits are in place. An Excel edit rewrites only the changed cells of the changed sheets (a cell keeps its style; a shared formula is written out when its group changes); a Word edit rewrites only the changed blocks (a block keeps its paragraph and run properties). Every other byte is copied. Excel recalculates on open (`fullCalcOnLoad`); its calculation chain is dropped |
| One mutation path | The agent (`files.edit`) and the window (`file_edit`) send the same `Edit` model: cells, sheets, document blocks, document layout, and find and replace. Agent JSON changes one document layout field with `{format, value}`; the window can batch fields as `{layout: {...}}`. Documents are Markdown blocks, numbered as the last read showed them. Layout changes the document defaults and every section margin without replacing direct paragraph or run formatting |
| Concurrent changes | Each read carries a `version` (a hash of the bytes). A document edit from the window, or an agent edit that passes `version`, is refused when the file changed since; the window then reloads. Cell edits apply to the latest file, since an address does not move. Saves go through a temporary file and a rename |
| Formulas | A small evaluator (arithmetic, references across sheets, SUM, AVERAGE, IF, SUMIF, VLOOKUP, INDEX, MATCH and others) computes values after each edit, so the agent and the window see totals that follow. A formula it does not know keeps the file's value |
| The agent | `files.list`, `read`, `search`, `write`, `edit` (`ArkBuilder::files(true)`), with their own distiller, labels and approval wording. A read is bounded (cell names, block numbers, ranges to read more). Writing asks |
| The window | A file opens in a tab of its own; the view is chosen by the kind the host reports (`sheet.js`, `document.js`, text), never by a format. A sheet can switch between cells and charts. A document keeps direct editing as the common path and puts base formatting in a small inspector. Changes save themselves. A turn that wrote files ends with a card per file (`HarnessEvent::ToolResult.files`). What the person changed rides in the focus (`edited`) |

Embedded worksheet charts are a read-only projection of XLSX and XLSM. ARK follows worksheet,
drawing and chart relationships. Internal series references use the recalculated cells; missing or
external references use the chart cache. Bar, column, line, area, pie, doughnut and scatter charts
render in the window. Other chart kinds keep their bytes and show an unavailable state. Chart
sheets remain unsupported. A malformed chart never prevents the cell grid from opening.

Document layout uses WordprocessingML's native values. Line and paragraph spacing live in the
document defaults. Character spacing uses run spacing and includes spaces because WordprocessingML
has no separate lossless word-spacing property. Margins change every `sectPr`; page size,
header, footer and gutter values stay as they were. Named styles and direct formatting keep
precedence and are reflected in each block's page-preview values.

## Consequences
- A new format on an existing shape (OpenDocument, TSV) is a `Format` and nothing else. A new
  shape (slides, PDF pages, images) is a `Format` and one view in the page.
- The editable model is deliberately small: no cell styles or merged cells in the shapes; charts
  are read-only projections and cannot be created or changed through `files.edit`; a
  table's cells are plain text; no undo in the window beyond the person's own typing. Those
  parts of a file survive edits but cannot be edited through ARK.
- `.xls` and `.doc` (the binary formats before 2007) are not read.
- `ToolResult` gained `files`, and `ArtifactSummary` gained `path`: front-ends open the file.
- Preloading actions counts a file the task names (`ventas.xlsx`) and the file open in the
  window, so a small model has the file tools in hand without matching English words.

## Check
- `crates/ark-files`: in-place edits keep untouched bytes (`an_edit_keeps_what_it_does_not_touch`
  for Excel and Word), hand-made Word files read as they look, the agent and the person on one
  file (`agent_and_person_work_on_one_file`), stale versions refused.
- `crates/ark-ui/tests/page.rs` (WebKit), `files_the_agent_writes_open_and_the_person_edits_them`.
- `crates/ark-runtime/src/catalog.rs`, `matching_actions_follow_the_tasks_words`.
