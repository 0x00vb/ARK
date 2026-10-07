# Current state

What is happening in ARK right now. Read this at the start of a session. Update it at the end
of a session that changed what is in progress, blocked or next. Keep it under a page: history
goes in git and `CHANGELOG.md`, direction in `ROADMAP.md`, decisions in `docs/adr/`.

Last updated: 2026-10-07 (ADR 0043: attention, workers, where work runs; v0.12.1: Upload a folder… and Add files, a touch pass, Run code on Linux fixed and tested)

## Release

| | |
|---|---|
| Latest tag | v0.12.1 (2026-10-07), on `origin` and `github` |
| `main` | v0.12.1: Upload a folder… (a project from this device's folder) and Add files (the composer's +) for browsers and phones, a touch pass of the page (ark-cloud ADR 0016 W4, W5), and Run code on Linux fixed: uv's Python was not visible in the sandbox; `ark-exec`'s tests pass on Linux (Docker, bookworm, bubblewrap) with `ARK_TEST_PYTHON=1`. v0.12.0: a browser comes in through a one-time link and keeps a session in an HttpOnly cookie, an entry for a browser without one, streams that end on revoke, a Content-Security-Policy on served pages (ADR 0040; ARK in the browser from ARK Cloud, ark-cloud ADR 0016). v0.11.0: W1 to W3, W5, W6 (ADR 0036 to 0038: questions kept on tasks and answered from any client, a restart continues work without repeating a change, each agent's step in the graph, a handed-off result reaches its chat once, a SIGTERM lets work reach a safe point and continue in the next process), a fix (a delegated task's outputs no longer include versions before), and `SurfaceKind::Terminal` and `DiffViewer` out of `ark-core`. v0.10.0 brought the browser, a runtime other machines connect to, work without the window, the Computer, Files, Run code, outputs by kind, the board, ARK Accounts, data work (ADR 0015 to 0035). `ARK_UI_PAGE_TESTS=1 scripts/ci.sh` passed on the release commit |
| Next tag | none planned. `[Unreleased]` in `CHANGELOG.md` collects what comes next |
| Step 2 | Done in v0.9.0 (ADR 0012, ADR 0014). Deferred: start the CLI before the first call, deeper provider telemetry |
| Windows | Branch `ci/windows` (pushed to `github`, not merged, 56 commits behind `main`): `ark-os` (ADR 0022) holds every OS difference. Lint clean for Windows; never run on Windows. Code on `main` written after it does not use `ark-os`, and two defects are on `main` today: on Windows a second process on one store re-runs tasks the first still runs (`process_alive` is `false`, ADR 0037 recovery), and `open_in_browser` cuts links at `&`. Analysis and proposed plan (2026-10-05): `ark-desktop/docs/WINDOWS.md`. Proposed for ARK: merge `ci/windows` before the machine run, one runtime per store (a lock), Windows stop events, a Windows cross-lint in `ci.sh`, Office fixes in `ark-files`. Waits for the owner's go |
| Accounts | In v0.10.0: `ark-account` (ADR 0026, 0031), an optional ARK Account for ARK Cloud. Tested against a fake service and dogfooded in ARK Desktop against the real account service (`ark-cloud/account`, local) |

## Open follow-ups (not started)

| What | Owner |
|---|---|
| Project processes (ADR 0041, on `main`, untagged): ARK Desktop turns them on (`.processes(true)`) after the next tag. The agent's browser reads the pages a project's processes serve themselves (ADR 0046, on `main`). Later: a runtime elsewhere forwards a process's port so the Browser view reaches it (ARK Cloud), Windows (a job object), a dogfood on a hosted model | ARK Desktop; ARK Cloud |
| The Browser panel as a view of its own in the window (ADR 0044, on `main`, untagged): checked natively only on duckduckgo.com; the rest of ADR 0044's Check needs a person at the screen (a Next.js dev server, a sign-in, a Desktop dialog, resize, project switch, reload, This Mac \| Cloud, Dock reopen, a notification; and since "can't be reached": `localhost:3000` down then up, a slow dev server, a plain http site with ARK Desktop's Info.plist, a page without a background in light and dark). Later: ARK's keys (⌘K, ⌘N, Esc) as menu accelerators so they work while the page has focus | the owner (the native check); ARK |
| Rename "Allow for this session" in approvals | the owner decides the words |
| The Computer (ADR 0042, 0045, on `main`, untagged): off until the person turns it on, asked for mid-task ("Allow ARK to use the apps on this computer"), Turn off in Settings; dogfooded in ARK Desktop on qwen3.5:27b (TextEdit). Left: grants per app (`grant_per("computer", ["app"])` instead of `ask_each_time`) so a task does not ask at each step; a run on WhatsApp and one on a Claude model, with the person approving; Desktop adds `.computer(true)` with its pin to the next tag (never on v0.12.1, where it is on in every chat); a WebKit page test of the Settings row and the card | the owner: grants per app, the WhatsApp and Claude runs; ARK Desktop: the pin |
| Attention, workers, where work runs (ADR 0043, accepted 2026-10-07, after the One and herdr review): step 0 an ACP spike (an ARK task on Claude Code and Codex: permission, kept question, answer, continue, result and file back), 1 one attention list in a runtime, 2 one ACP worker (ADR 0039 H1 to H3), 3 notifications to a phone and My machine as a location, 4 attention across runtimes. Order and checks: ROADMAP › Work runtime | step 0 done (branch `spike/acp-worker`, ADR 0043 › Step 0 results); step 1 next |
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
| ark-desktop | nothing: it moves to v0.12.0 for `token link` and `ARK_ENTRY_URL` (ARK in the browser) |
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
bubblewrap, ADR 0032), for Cloud Phase 2. A4 ran on Linux on 2026-10-07 (Docker, Debian bookworm, bubblewrap, uv 0.12.23; `/proc`
unmasked as in a VM): all 10 `ark-exec` tests pass after the fix in v0.12.1.

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
