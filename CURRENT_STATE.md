# Current state

What is happening in ARK right now. Read this at the start of a session. Update it at the end
of a session that changed what is in progress, blocked or next. Keep it under a page: history
goes in git and `CHANGELOG.md`, direction in `ROADMAP.md`, decisions in `docs/adr/`.

Last updated: 2026-10-05 (v0.11.0; work runtime W1 to W6, ADR 0036 to 0039)

## Release

| | |
|---|---|
| Latest tag | v0.11.0 (2026-10-05), on `origin` and `github` |
| `main` | v0.11.0: W1 to W3, W5, W6 (ADR 0036 to 0038: questions kept on tasks and answered from any client, a restart continues work without repeating a change, each agent's step in the graph, a handed-off result reaches its chat once, a SIGTERM lets work reach a safe point and continue in the next process), a fix (a delegated task's outputs no longer include versions before), and `SurfaceKind::Terminal` and `DiffViewer` out of `ark-core`. v0.10.0 brought the browser, a runtime other machines connect to, work without the window, the Computer, Files, Run code, outputs by kind, the board, ARK Accounts, data work (ADR 0015 to 0035). `ARK_UI_PAGE_TESTS=1 scripts/ci.sh` passed on the release commit |
| Next tag | none planned. `[Unreleased]` in `CHANGELOG.md` collects what comes next |
| Step 2 | Done in v0.9.0 (ADR 0012, ADR 0014). Deferred: start the CLI before the first call, deeper provider telemetry |
| Windows | Branch `ci/windows` (pushed to `github`, not merged, 56 commits behind `main`): `ark-os` (ADR 0022) holds every OS difference. Lint clean for Windows; never run on Windows. Code on `main` written after it does not use `ark-os`, and two defects are on `main` today: on Windows a second process on one store re-runs tasks the first still runs (`process_alive` is `false`, ADR 0037 recovery), and `open_in_browser` cuts links at `&`. Analysis and proposed plan (2026-10-05): `ark-desktop/docs/WINDOWS.md`. Proposed for ARK: merge `ci/windows` before the machine run, one runtime per store (a lock), Windows stop events, a Windows cross-lint in `ci.sh`, Office fixes in `ark-files`. Waits for the owner's go |
| Accounts | In v0.10.0: `ark-account` (ADR 0026, 0031), an optional ARK Account for ARK Cloud. Tested against a fake service and dogfooded in ARK Desktop against the real account service (`ark-cloud/account`, local) |

## Open follow-ups (not started)

| What | Owner |
|---|---|
| Rename "Allow for this session" in approvals | the owner decides the words |
| Agent harnesses as workers (ADR 0039, accepted; ROADMAP › Work runtime, H1 to H5): Claude Code, Codex, OMP or any ACP agent runs a task under ARK's work model. Not built; ACP first; how each agent behaves over ACP (writes, permissions, outputs, resume) is tested before H4 counts as solved | a concrete external-worker use case |
| Board: whether the chat uses the board unasked for project-sized work, and hosted models (the `board` workload passes 3/3 live on qwen3:4b when asked); board tools for code-defined team leads | before turning anything on or off by default (ADR 0023) |
| ARK Desktop moves its keys to ARK's store (`ark_connect::ApiKeys`, the `api_key_*` methods) and drops `setup::keychain` and `load_keys` | ARK Desktop |
| A paid-model run of scheduling: qwen3:4b claims schedules it never made | needs the owner's OK |
| ARK Desktop uses `ark_files::Files` instead of its own `files.*` adapter (same action names), and turns Files on | ARK Desktop, against `../ark` until v0.10.0 |
| Files: cell styles in the grid, images in a document, undo across saves, `.xls`, more formula functions | when a real task needs one (ADR 0019 › Consequences) |
| Files on qwen3:4b: `files-edit` (`bench/RESULTS.md`) is 0/5. It finds the file, and since the announced-work check it no longer stops after reading; it then puts the totals in a new column or sheet. qwen3.5:27b 3/3 | the layout is the model's; revisit with a hosted model or a larger local one |
| ARK Desktop ships `uv` in the app (Run code is on in its `main`): without `uv` a clean Mac shows Run code as unavailable. The Data Analysis page on aiark.dev waits for that release (owner, 2026-10-05) | ARK Desktop |
| Claimed files: the harness checks an answer's saved files against the turn's (`claimed-files`, `bench/RESULTS.md`). Claimed schedules: not reproduced on qwen3:4b or qwen3.5:27b in three shapes (`claimed-schedule`), so no check for claimed actions. Left: claims in other languages than English | a transcript of a claimed schedule, then a workload that reproduces it |
| Run code on qwen3:4b: 1/5 on `analysis`; it diagnoses its own syntax error but does not fix it. Identical changes now run at most twice (35% fewer tokens). qwen3.5:27b 2/2 | a fix needs a stronger model or a measured hint; nothing planned |
| Run code later: network as its own permission, packages per project, memory and CPU limits per run | when a workflow needs one (ADR 0020, 0032 › Consequences) |
| Run code on a real Windows machine (`cargo test -p ark-exec --test run_windows`, with `ARK_TEST_PYTHON=1`) and a real Linux machine (`ARK_TEST_PYTHON=1 cargo test -p ark-exec --test run`) | the owner's Windows machine; a Linux machine or a working Docker (ADR 0032) |
| Data work on smaller models: Haiku and qwen3.5:27b keep duplicate orders on `workbooks` (0/2 each); Sonnet and Codex 2/2. A hint naming the skill in `code.run` did not help Haiku | a measured change in the skill or the task's shape; nothing planned |
| Data UX: Windows saying Excel holds a workbook open (sharing violation on a save) | the owner's Windows machine run |
| Versions before (ADR 0034): not measured on ext4 or NTFS, where the copy before a run is a real copy; the store keeps every version (no clean-up of old artifacts yet); the terminal has no Restore | a cloud runtime's numbers; when the store grows too large |

## Waiting on this repo

| Who | Waits for |
|---|---|
| ark-desktop | nothing: it pins v0.11.0 (ark-desktop 05b7aa1), so its page shows W4 (what agents do and wait for, from ARK) |
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
project there (`remote::send_folder`, ADR 0030, for Copy to Cloud); and A4 (Run code on Linux,
bubblewrap, ADR 0032), for Cloud Phase 2. A4 still needs a run on a Linux machine (Docker on
the dev Mac is broken).

W1 to W3, W5 and W6 of [`ROADMAP.md`](ROADMAP.md) › Work runtime are on `main` (ADR 0037,
0038). The runtime has no known blocker left for Cloud running work nobody watches; next is an
end-to-end check on a Cloud machine. ARK Desktop's W4 (its page reads them) ships on its pin of v0.11.0
(ark-desktop 05b7aa1).

W5 and W6 checks: `examples/writing-agent/tests/restart.rs` runs a real `writer --serve` against
a fake OpenAI-compatible model, stops it with SIGTERM (clean exit, about a second) or SIGKILL
while an agent works for a waiting chat, and starts it again: the task continues, the chapter is
not saved twice, the chat gets the result once. A change still running when the stop starts is
let finish: covered in process (`a_stopping_process_lets_a_change_under_way_finish…`), since the
writer has no slow change to stop a real process in. Dogfooded live (2026-10-05, qwen3.5:27b,
`writer --serve --tokens`): the chat handed a save to an agent and waited; after the save,
SIGTERM landed in the agent's last model call and the process exited cleanly in 0.1 s. The
restarted runtime continued the task. The model tried to save twice more and both calls were
answered from the first save (the file was not written again). The chat's turn shows cancelled
and the result arrived as one post. An earlier run where the chat's wait got the result live
posted nothing (`reported`, no second copy).

W1 to W3 dogfooded live (2026-10-05, qwen3.5:27b, `writer --serve --tokens`): an agent asked to
save a chapter, the process was killed with `kill -9`, the restarted runtime listed
the kept question in `hello` and `/agents` ("waiting (your answer)"), `writer --connect -p
"/task answer <task> once"` answered it, and the chapter was saved once. Not run on a hosted
model or a Cloud machine yet. The same through ARK Desktop's page (server build against `main`):
the canvas showed "needs you" and the waiting line before and after a `kill -9`, and "Allow once"
in the tray after the restart saved the file once.

Checks on the W1 to W3 commits: `scripts/ci.sh` passes (fmt, clippy, every test, release build,
bench gate); the 26 WebKit page tests passed on the first full run. `files-edit` reads 8 tokens
over the baseline both before and after this work: from v0.10.0's versions before (checked on
8bd8b96), within the gate. The page test `an_agent_asks_to_connect_mail_and_the_turn_goes_on`
is flaky: 1 of 5 passed before this work, 4 of 5 after (a card on the board).

## Working notes

- Several Claude sessions may edit this tree at once. Agree a file split before editing (see
  `../WORKSPACE.md`, Sessions).
- `.playwright-mcp/` is scratch output from browser dogfooding. It is untracked; delete it when
  done.
