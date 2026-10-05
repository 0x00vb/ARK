# Current state

What is happening in ARK right now. Read this at the start of a session. Update it at the end
of a session that changed what is in progress, blocked or next. Keep it under a page: history
goes in git and `CHANGELOG.md`, direction in `ROADMAP.md`, decisions in `docs/adr/`.

Last updated: 2026-10-05 (v0.9.0; `main` has the browser, step 5, C5, Files, Run code and the board, untagged)

## Release

| | |
|---|---|
| Latest tag | v0.9.0 (2026-10-04), on `origin` and `github` |
| `main` | Everything since v0.9.0 in one place, no other branches: the browser (C4, ADR 0015), the window hooks for ARK Desktop's UX pass (`ArkUi::theme`, `ark.modelPicker`, `ark.onSubmit`), a runtime other machines connect to with saved access tokens (ADR 0016, [`docs/REMOTE.md`](docs/REMOTE.md)), work that runs without the window: several live chats, schedules (ADR 0017), the Computer, `ark-computer` (C5, ADR 0018), and Files, `ark-files` (ADR 0019): spreadsheets and documents the agent and the person edit in place, opened in the window as a grid or a page. Run code, `ark-exec` (ADR 0020), and kinds for Markdown, PDFs and images with one view per kind (ADR 0021). The project's board (ADR 0023): work items the person and the agents share, which follow the tasks run for them; a Board tab in every app that runs agents, `/board`, and `board.*` for the chat. `scripts/ci.sh` and the WebKit page tests passed on all of it. Pushed to `origin` and `github` |
| Next tag | v0.10.0 (`Breaking` entries under `[Unreleased]`). Ready to cut with [`docs/RELEASING.md`](docs/RELEASING.md) steps 2 to 9 when the owner says so. ARK Desktop 0.2 waits for it |
| Step 2 | Done in v0.9.0 (ADR 0012, ADR 0014). Deferred: start the CLI before the first call, deeper provider telemetry |
| Windows | Branch `ci/windows` (pushed to `github`, not merged): `ark-os` (ADR 0022) holds every OS difference, and ARK finds, starts, stops and opens programs through it; text keeps CRLF and UTF-16; Ctrl in the window; page tests in Chromium too. `scripts/ci.sh` with page tests passes on macOS; lint clean for Windows. Not run on Windows yet: the owner runs `scripts/windows-check.ps1` on a Windows machine (GitHub Actions billing stays off). Merges to `main` after that run. Plan: `ark-desktop/docs/WINDOWS.md` |

## Open follow-ups (not started)

| What | Owner |
|---|---|
| Rename "Allow for this session" in approvals | the owner decides the words |
| Board: a live benchmark workload for planning on the board (does the chat add good items and link hand-offs, with no lost success); board tools for code-defined team leads; ARK Desktop dogfood of the Board tab | before any harness change for it (ADR 0023 › Consequences) |
| Turn the Computer off when an example serves other machines with tokens | remote work (`examples/writing-agent`) |
| Keep a runtime running after its window closes, as a generic `ark-ui` option | ARK Desktop asks for it (a menu-bar item) |
| API keys in ARK's secret store, so apps stop keeping their own | ARK Desktop moves to it |
| A paid-model run of scheduling: qwen3:4b claims schedules it never made | needs the owner's OK |
| ARK Desktop uses `ark_files::Files` instead of its own `files.*` adapter (same action names), and turns Files on | ARK Desktop, against `../ark` until v0.10.0 |
| Files: cell styles in the grid, images in a document, undo across saves, `.xls`, more formula functions | when a real task needs one (ADR 0019 › Consequences) |
| Files on qwen3:4b: it reads the file and adds a sheet, but often stops before writing values, and with a writer's working state of an earlier task it reads the wrong file. qwen3.5:27b did the whole loop (read the person's edits, rewrote the conclusions, made a report) | a live benchmark workload for files, before any harness change for it |
| ARK Desktop ships `uv` in the app (Run code is on in its `main`): without `uv` a clean Mac shows Run code as unavailable. The Data Analysis page on aiark.dev waits for that release (owner, 2026-10-05) | ARK Desktop |
| Claimed files: the harness checks an answer's saved files against the turn's (`claimed-files`, `bench/RESULTS.md`). Claimed schedules: not reproduced on qwen3:4b or qwen3.5:27b in three shapes (`claimed-schedule`), so no check for claimed actions. Left: claims in other languages than English | a transcript of a claimed schedule, then a workload that reproduces it |
| A live workload for analysis (read a workbook, run code, write a chart and a report). Dogfooded twice on qwen3.5:27b in ARK Desktop: correct numbers, a chart and a report each opened in their view, about 9 minutes, 3 approvals, one failed run each time (a pandas mistake) | before any harness change for Run code |
| Run code later: network as its own permission, packages per project, a sandbox for Linux and Windows | when a workflow needs one (ADR 0020 › Consequences) |

## Waiting on this repo

| Who | Waits for |
|---|---|
| ark-desktop | v0.10.0. Its `main` has the whole 0.2 (Mail, Calendar, Browser, the UX pass, remote) and builds against `../ark` through a gitignored `[patch]` until the tag exists |
| SecRes | nothing; pins v0.8.0. v0.9.0 and v0.10.0 are optional upgrades (`Breaking` in `CHANGELOG.md`) |
| Mail and Calendar for anyone | ARK's Google OAuth client and Google's review: owner steps in [`docs/GOOGLE.md`](docs/GOOGLE.md). Until then they show as unavailable, or work with `ARK_GOOGLE_CLIENT_ID` for test users |

## Next

Prove ARK as a product, in this order ([`ROADMAP.md`](ROADMAP.md) › Next): v0.8.0 (done),
Claude Code and Codex first-class (done, v0.9.0), ARK Desktop public, hosted-model
evidence (time-boxed), persistent headless ARK (on `main`, untagged), ARK Cloud on the same
runtime.

## Working notes

- Several Claude sessions may edit this tree at once. Agree a file split before editing (see
  `../WORKSPACE.md`, Sessions).
- `.playwright-mcp/` is scratch output from browser dogfooding. It is untracked; delete it when
  done.
