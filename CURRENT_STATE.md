# Current state

What is happening in ARK right now. Read this at the start of a session. Update it at the end
of a session that changed what is in progress, blocked or next. Keep it under a page: history
goes in git and `CHANGELOG.md`, direction in `ROADMAP.md`, decisions in `docs/adr/`.

Last updated: 2026-10-05 (v0.9.0; `main` has the browser, step 5, C5, Files, Run code and the board, untagged)

## Release

| | |
|---|---|
| Latest tag | v0.9.0 (2026-10-04), on `origin` and `github` |
| `main` | Everything since v0.9.0 in one place, no other branches: the browser (C4, ADR 0015), the window hooks for ARK Desktop's UX pass (`ArkUi::theme`, `ark.modelPicker`, `ark.onSubmit`), a runtime other machines connect to with saved access tokens (ADR 0016, [`docs/REMOTE.md`](docs/REMOTE.md)), work that runs without the window: several live chats, schedules (ADR 0017), the Computer, `ark-computer` (C5, ADR 0018), and Files, `ark-files` (ADR 0019): spreadsheets and documents the agent and the person edit in place, opened in the window as a grid or a page. Run code, `ark-exec` (ADR 0020), and kinds for Markdown, PDFs and images with one view per kind (ADR 0021). The project's board (ADR 0023): work items the person and the agents share, which follow the tasks run for them; a Board tab in every app that runs agents, `/board`, and `board.*` for the chat. `scripts/ci.sh` and the WebKit page tests passed on all of it. Pushed to `origin` and `github` The terminal on a runtime elsewhere: `app --connect URL` (ADR 0024). A schedule's runs post their results into the chat that asked (ADR 0025). Named OpenAI-compatible endpoints, and a 402 as out of credit with what to do (ADR 0027). |
| Next tag | v0.10.0 (`Breaking` entries under `[Unreleased]`). Ready to cut with [`docs/RELEASING.md`](docs/RELEASING.md) steps 2 to 9 when the owner says so: `ARK_UI_PAGE_TESTS=1 scripts/ci.sh` passed on 78dc9bf (2026-10-05). Since v0.9.0: the browser, the Computer, Files edited in place, Run code, outputs opened by kind, a runtime other machines connect to, work without the window (several chats, schedules, keep running), the project's board, API keys in ARK's keychain. ARK Desktop 0.2 waits for it |
| Step 2 | Done in v0.9.0 (ADR 0012, ADR 0014). Deferred: start the CLI before the first call, deeper provider telemetry |
| Windows | Branch `ci/windows` (pushed to `github`, not merged): `ark-os` (ADR 0022) holds every OS difference, and ARK finds, starts, stops and opens programs through it; text keeps CRLF and UTF-16; Ctrl in the window; page tests in Chromium too. `scripts/ci.sh` with page tests passes on macOS; lint clean for Windows. Not run on Windows yet: the owner runs `scripts/windows-check.ps1` on a Windows machine (GitHub Actions billing stays off). Merges to `main` after that run. Plan: `ark-desktop/docs/WINDOWS.md` |
| Accounts | Branch `account` (worktree `../ark-account`, not merged): `ark-account` (ADR 0026), an optional ARK Account for ARK Cloud; `ConnectError::Refused`, `SignIn::landing`, revoke sends `client_id`; the window's `person` icon and palette `when`. Tested against a fake service and dogfooded in ARK Desktop against the real account service (`ark-cloud/account`, local). Merges to `main` when the owner says so; goes out with the next tag |

## Open follow-ups (not started)

| What | Owner |
|---|---|
| Rename "Allow for this session" in approvals | the owner decides the words |
| Board: whether the chat uses the board unasked for project-sized work, and hosted models (the `board` workload passes 3/3 live on qwen3:4b when asked); board tools for code-defined team leads | before turning anything on or off by default (ADR 0023) |
| ARK Desktop moves its keys to ARK's store (`ark_connect::ApiKeys`, the `api_key_*` methods) and drops `setup::keychain` and `load_keys` | ARK Desktop |
| A paid-model run of scheduling: qwen3:4b claims schedules it never made | needs the owner's OK |
| ARK Desktop uses `ark_files::Files` instead of its own `files.*` adapter (same action names), and turns Files on | ARK Desktop, against `../ark` until v0.10.0 |
| Files: cell styles in the grid, images in a document, undo across saves, `.xls`, more formula functions | when a real task needs one (ADR 0019 › Consequences) |
| Files on qwen3:4b: `files-edit` (`bench/RESULTS.md`) is 0/5. It finds the file, and since the announced-work check it no longer stops after reading; it then puts the totals in a new column or sheet. qwen3.5:27b 3/3 | the layout is the model's; revisit with a hosted model or a larger local one |
| ARK Desktop ships `uv` in the app (Run code is on in its `main`): without `uv` a clean Mac shows Run code as unavailable. The Data Analysis page on aiark.dev waits for that release (owner, 2026-10-05) | ARK Desktop |
| Claimed files: the harness checks an answer's saved files against the turn's (`claimed-files`, `bench/RESULTS.md`). Claimed schedules: not reproduced on qwen3:4b or qwen3.5:27b in three shapes (`claimed-schedule`), so no check for claimed actions. Left: claims in other languages than English | a transcript of a claimed schedule, then a workload that reproduces it |
| Run code on qwen3:4b: 1/5 on `analysis`; it diagnoses its own syntax error but does not fix it. Identical changes now run at most twice (35% fewer tokens). qwen3.5:27b 2/2 | a fix needs a stronger model or a measured hint; nothing planned |
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
runtime. ARK Cloud's plan is `../ark-cloud/docs/CLOUD.md` (2026-10-05); ARK's part of it is
A1 to A4 on the board. A1 (named OpenAI-compatible endpoints) and A2 (402 is out of credit) are
on `main` (ADR 0027), and A3 (`FileSecrets`: keys and tokens in an encrypted file for servers,
ADR 0028), and A6 (the window opens a runtime elsewhere and comes back, `remote::visit`, ADR
0029, for ARK Desktop's This Mac | Cloud), and a folder sent to a runtime elsewhere becomes a
project there (`remote::send_folder`, ADR 0030, for Copy to Cloud); next A4 (Run code on
Linux), for Cloud Phase 2. A4 needs a Linux machine to test on (Docker on the dev Mac is broken).

## Working notes

- Several Claude sessions may edit this tree at once. Agree a file split before editing (see
  `../WORKSPACE.md`, Sessions).
- `.playwright-mcp/` is scratch output from browser dogfooding. It is untracked; delete it when
  done.
