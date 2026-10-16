# Current state

## Dictation follow-through

On `main`: each chat keeps its own draft, dictation uploads stop at exactly eight minutes,
Whisper and GGML native logs are silenced, Devices skips `/session` in native windows, and
notification page setup runs only in the app's own main window (ADR 0029). Open: a native check
of detached and remote windows at the screen. Board `26025811`.

## Bounded code runs

On `main` (ADR 0072): two active code runs, eight waiting, one per project, owned cleanup on
cancel, background children stopped, bounded capture. Left: native Windows and Linux runs and
the owner's call on the 2/8 limits (board `17de65d1`).

## Advanced artifact surfaces in development

`ark-advanced-artifacts` implements ADR 0071: map/model classification, route-bound immutable
snapshots, cancellation/progress, large-file metadata probes, bounded Focus and `ark.fileViewer`.
A0 and A1 to A3 independent code QA are GO. Focused tests pass: 49 file, 36 UI unit and nine HTTP.
Real WebKit covers Source recovery, a 30 MB model and the shipped Desktop frame through ARK's
snapshot transport, including selection and Source switching. Desktop has offline map and
worker-decoded GLB/STL surfaces, with real WebKit interaction and hostile-input proofs. Evidence:
`docs/ADVANCED_ARTIFACTS_A0.md`. The complete v0.24.0 CI passes, including all 55 WebKit
scenarios, the release build and benchmark gate. Desktop's full checks pass: 22 Rust and 64
JavaScript tests. Native local rendering, renderer-kill Retry and Linux ARM64-to-Mac map/model
dogfood pass. The consumer must pin the immutable v0.24.0 tag, not this development tree.
The owner deferred five-person observation and physical
trackpad testing on 2026-10-15; do not claim observed 10/10 usability. ARK Desktop implements A4
(CAD) as its own capability. A5 (IFC) is on `main`; the owner
deferred architect/BIM observation on 2026-10-15, so no professional validation is claimed.

What is happening in ARK right now. Read this at the start of a session. Update it at the end
of a session that changed what is in progress, blocked or next. Keep it under a page: history
goes in git and `CHANGELOG.md`, direction in `ROADMAP.md`, decisions in `docs/adr/`.

Last updated: 2026-10-16 (dictation follow-through, IFC files and bounded code runs on main)

## Release

| | |
|---|---|
| Latest tag | v0.25.0 (2026-10-16), voice dictation, separate chat drafts, IFC models and bounded code runs. |
| `main` | v0.25.0 release tree. |
| v0.14.0 | ADR 0048 (chat turns survive a crash and continue), ADR 0049 (typed Codex tool arguments, retry after shown text), ADR 0050 (an agent never loses its task), and `ark-os` from `ci/windows` (ADR 0022, merged; Windows not yet run on a machine). `ARK_UI_PAGE_TESTS=1 scripts/ci.sh` passed on the release commit |
| v0.15.0 | ADR 0051 (a turn ends on its answer, the project's `AGENTS.md` in each turn, code coloured in the editor), ADR 0052 (a chat runs on the computer the person picks, over SSH; new crate `ark-reach`; rustls), ADR 0053 (scheduled work; renumbered from the branch's 0051). `ARK_UI_PAGE_TESTS=1 scripts/ci.sh` passed on the release commit. No live Claude run before the tag (the owner's choice, 2026-10-08; ROADMAP › Deferred) |
| v0.16.0 | ADR 0052 resource-location amendment: every resource is bound to its runtime, project replica and opening chat; missing routes fail closed; remote Browser rendering, terminal replay and reconnect; exact transfer plans, epochs and durable receipts. `ARK_UI_PAGE_TESTS=1 scripts/ci.sh` passed on the release commit. A real Linux x86_64 runtime covered Files, `.xlsx`, `.docx`, Terminal, processes, artifacts and Browser with no matching local workspace |
| Next tag | not set. |
| Step 2 | Done in v0.9.0 (ADR 0012, ADR 0014). Deferred: start the CLI before the first call, deeper provider telemetry |
| Windows | `ci/windows` merged into v0.14.0 (`ark-os`, ADR 0022): every OS difference ARK had on 2026-10-05 goes through it; code written on `main` after that (processes, notifications, durable turns) mostly does not yet, and newer shell-script tests are not marked unix-only. Never run on Windows. Next: the machine run (`scripts/windows-check.ps1`), Office fixes in `ark-files`, one runtime per store. Analysis and plan: `ark-desktop/docs/WINDOWS.md` |
| Accounts | In v0.10.0: `ark-account` (ADR 0026, 0031), an optional ARK Account for ARK Cloud. Tested against a fake service and dogfooded in ARK Desktop against the real account service (`ark-cloud/account`, local) |

## In progress

| What | State |
|---|---|
| Completed branch audit | CI's unoptimized test profile, historical completed branches, voice dictation and the reviewed efficiency and catalog-startup patches are integrated on local `main`. Exact combined page-enabled CI and focused remote-provider, dictation and approval checks pass. No push, tag or release. |
| IFC model files (A5) | On `main`: `.ifc` is a read-only model (STEP header check, 2 MiB limit before any copy), Source opens read-only, `api.confirm(label)` for viewers, and `ArkUi::wasm_worker_asset` (HTTP serve path only). ARK Desktop's consumer is on `codex/ifc-integration` and needs an ARK tag with this before it can pin. |
| Efficiency and Codex startup | Host-model tools stay in ARK's approval path; scoped vendor overrides remove unused native tools without changing user settings. Default instructions avoid redundant bookkeeping and route permitted proposals to the existing approval gate. Catalog discovery skips optional usage telemetry while retaining its six-second deadline and strict advertised model/effort checks. Separate owner gates, 99 model tests, safety checks and synthetic writing-app checks pass. The 30/30 efficiency result is configuration-confounded, not a causal savings claim. Source chain, retained failures and remaining device/release gates: `bench/integration-2026-10-15/README.md`. |
| Models and commands follow a chat's computer | Released in v0.24.0: choosing a linked computer immediately uses its subscriptions, API keys and Ollama catalog; the selected model starts the chat there; setup stays chat-bound; local-only slash commands are omitted from completion and the command palette, and typed slash words go to the remote chat as text. Focused two-runtime Rust and WebKit tests, independent QA and the full `scripts/ci.sh` gate pass. |
| Voice dictation (ADR 0070) | Integrated on `main`: explicit computer-bound uploads, independent split controls, permission and inference leases, immediate capture cleanup and stale-result cancellation. Authenticated two-runtime and WebKit tests cover remote routing and capture races. Full page-enabled CI passes: lint, workspace tests, 56 WebKit scenarios, release build and simulated benchmark gate. Independent source and screenshot QA is GO. Optional `ark-transcribe` installs the pinned multilingual Whisper base model only after a microphone action. Native microphone and multi-minute quality validation remain release follow-ups on macOS, Windows and Linux. |
| Signed page updates (ADR 0067) | Released in v0.23.4. An opted-in native consumer verifies a separately signed manifest, exact native versions, a monotonic sequence and a bounded exact file set. Apply reloads only the page. A seven-second readiness failure restores the previous page while the runtime keeps running. ARK Desktop provides the first consumer and release path. |
| Plugin marketplace (ADR 0069) | Released in v0.24.0: Plugins occupies the fixed global rail, Connections is removed from Settings and every reviewed provider integration is offered without a separate Add toggle. The Desktop bridge opens server-controlled managed connections and exposes generic, approval-gated managed actions only when an ARK Account is signed in. Connections are account-wide across that person's projects. The broker holds the provider key; Desktop does not. Composio Free is the default launch adapter. Activepieces or remote MCP remains the account-optional self-hosted path. A signed catalog refresh and second-provider swap proof remain. |
| Ambient Desktop | Released in v0.23.0. A native consumer can keep one page and runtime alive, switch it into a compact menu-bar presentation, project agent state and restore the exact chat. Computer keeps only a volatile foreground app name for this view. Window name and identifier use a separate approval each time. Accessibility content and capture remain the existing higher-permission actions. Held SSH and Cloud chats never fall back to this Mac. |
| Workspace UI polish | Released through v0.23.4. Open chat tabs and Chat, Agents and Board share one top rail; chat and side-pane tabs share the compact 30 px Terminal-tab control grammar; split panes do not repeat the selected chat title; the fixed destination rail reaches the window edge without a partial divider; WebKit side panes keep their intended width. Pixel inspection, the full ARK gate and 49 WebKit scenarios pass. |
| Plan, Manual and Auto execution modes (ADR 0064) | Released in v0.22.0. The chat persists a revisioned mode and each turn freezes it. Plan omits and rejects mutations. Auto bypasses approval only for explicitly eligible project file and contained code actions. Browser, Computer, connections, automations and unknown tools remain Manual. The composer exposes the choice and `Shift+Tab` cycles it. |
| Spatial chat layouts (ADR 0063, 0068) | Released in v0.23.4. Tabs reorder by drag and form nested horizontal or vertical layouts by dropping on a pane edge. Up to four visible chats keep their transcript, turn, queue, approvals, draft, scroll, model and mode. Dividers resize with pointer or keyboard, narrow windows show the active leaf, and valid layouts restore. A native drag beyond the window opens the chat in a full ARK window with window-scoped foreground, tab layout and Browser panel. The source changes only after native creation succeeds. |
| Native generated images (ADR 0066) | Released in v0.22.0: supported OpenAI reasoning models get an approval-gated `image.generate` action. Valid PNG output becomes a durable project file, renders as the image file card in chat and opens in the shared image view. A paid live API generation was not run. |
| Subscription agents keep their credentials on their computer (ADR 0061) | In v0.20.0: Grok 1.0 is an offered ACP worker with an isolated provider-owned login, fixed ask rules, no inherited app secrets, place-bound setup and a checked model selector. Claude Code and Codex explicit worker compatibility remains, but neither subscription is newly offered: Anthropic requires explicit permission for automated consumer access, and OpenAI's supported third-party route is app-specific Sign in with ChatGPT. ARK's full release gate passed. Left: ARK Desktop's permanent pin, native check and release. |
| Repeated failed tool calls | In v0.20.1, an identical failed read or observation runs once and gets one retry, then returns immediately. A successful change reopens the read. Focused runtime tests pass. The 22-workload simulated suite has no non-timing metric change. Live qwen3:4b passes `failure-recovery` and `web` 3/3, writer dogfood passes and implementation QA is GO. |
| Signed background updates for native consumers (ADR 0054) | In v0.17.0: opt-in `desktop-updater`, official Tauri updater in local and direct-remote window paths, an own-page-only state bridge, background download and install on macOS and Linux, no forced restart. Windows is explicitly unavailable until it has a non-disruptive installer design. Left: ARK Desktop's staged 0.5.2 to 0.5.3 proof |
| Project Terminal without a chat (ADR 0060) | Protocol support was released in v0.19.0 and stabilized in v0.19.1. The v0.20.0 correction makes the existing web Terminal pane use project authority, capture the current chat's computer and leave the chat unchanged. It removes the duplicate project view. Protocol v1.1 retains generated Terminal types and transport for future artifact clients. ARK Desktop 0.6.5 remains public on v0.19.1; a corrected Desktop release is still needed. |
| A chat runs on the computer the person picks (ADR 0052, accepted 2026-10-07): the place chip, held chats and `chat_*` calls, the project copy and Bring back, `ark-reach` (setup and a tunnel over the person's `ssh`), Settings › Computers, programs installed and signed in there, ARK Desktop's contract and builds, the writer as second app | In v0.15.0 (steps 0 to 6 and 8). The resource-location amendment is in v0.16.0: every surface carries a validated runtime, project and chat; transfer retry has exact plans, epochs and durable receipts; the remote Browser is rendered by Chromium there. Automated two-runtime tests and a real Linux x86_64 runtime cover Files, editors, spreadsheet, document, Terminal, process, artifacts and Browser, with no local replica. Left: a real-VPS run with a subscription (the owner), step 7 (OpenCode, Cursor: needs a signed-in provider and Cursor) |

## Open follow-ups (not started)

| What | Owner |
|---|---|
| Event automations (ADR 0062, proposed): schedules unchanged under one automation view; transport-authenticated external events with actor claims; deterministic matching; one durable run per event; mandatory run-scoped authority that broader grants cannot widen; Mail and WhatsApp Business proofs; one locked SQLite writer on a NAS, no broker. Plan and gates: `docs/AUTOMATIONS.md` | unassigned |
| A turn ends on its answer and follows the project's instructions (ADR 0051, in v0.15.0): found from the owner's ARK Desktop run of the highlighting task on `claude:sonnet` (10 calls, 551k input tokens, the answer shown 5 times, `rules: 0`). A restated answer beside a second check of the turn's own changes ends the turn; the project folder's `AGENTS.md` goes into each turn (1,500 tokens, sections chosen by the task); the editor colours code from muted theme tokens (BRAND.md › Product UI). Checks: `scripts/ci.sh` with page tests; `restated_answer.rs`, `rules::tests`, `a_turn_follows_the_project_folders_agents_file`, the workbench page test; QA plan and implementation gates. Left: a live run of the same task on `claude:sonnet` (the owner's OK), two instruction lines on checking and one answer measured live (+5.8% tokens in the simulated suite, held back), orchestrated agents reading `AGENTS.md`, the Claude vendor thread at 155k (a card) | the owner (live run) |
| Scheduled work (ADR 0053, in v0.15.0): dates, cron and the person's time zone; asked each time with all a run is handed; pause, resume, change, run now, stop in words, `/schedules` and the window (a line per schedule above its chat, the schedule in the side pane); one run per time across a crash; a failure that may pass tried again after 5 and 10 minutes, continuing; missed runs once or let go; overlap; auto-pause after 3 failures; 50 runs kept. Checks: `tests/schedules.rs`, a WebKit page test, a real-process run (`restart.rs`: approved, SIGKILL, ran at its minute in the next process), a live run on qwen3.5:27b (ADR 0053 › Check; the chat overwrote the person's CSVs with sample data before scheduling, asked). Cost: +7.9% tokens on the simulated `board` workload (larger schema), baseline regenerated. Left: ARK Desktop's Agents canvas (paused schedules, the schedule's zone in labels), a paid-model run of the stock example, waking the computer or starting ARK at a run's time (Desktop's login item) | the owner (paid run); ARK Desktop |
| An agent never loses its task (ADR 0050), with ADR 0049 (typed Codex tool arguments) and ADR 0048 (durable chat turns): in v0.14.0. Found from the owner's ARK Desktop store: the task left the working set after two reads, a Codex vendor session rotated without it, "cointinue" became the goal, and `cargo` was run inside Run code. Now: a pinned task frame and a ledger of dropped steps; a goal stored with each turn and continued by any follow-up (also after the step limit); a budget sized from the model's window (16k on a subscription lane, up to 48k on an API); a reply the model could not get right is sent back to it as feedback; `code.run` points a project's commands to `process.start`. Checks: `scripts/ci.sh`; `long_task.rs`, `lost_task.rs` (the owner's sequence with a SIGKILL), `login_path.rs`; live on `codex:default` (twice) and `claude:sonnet` (killed, "cointinue", highlighting implemented; ARK on Sonnet used 338k input tokens against Claude Code's 781k for the same task); live qwen3:4b against `main`: 12/14 both, +7.6% tokens (mostly ADR 0049's schemas), `bench/RESULTS.md`; 35 WebKit page tests; QA plan, design and implementation gates passed. Left: ARK Desktop's pin to v0.14.0 (its `main` has processes on since 6e52a25) and a native check | ARK Desktop |
| Durable chat turns (ADR 0048, on `main`, untagged): a chat turn keeps its progress through a crash, shows where it stopped with one Continue, and continues without repeating a change or a hand-off; a reused pid is not taken for a dead writer; 4 retries and 60 s for an unreachable model; durable blobs. `scripts/ci.sh` passes; real `kill -9` runs pass (`crash_recovery.rs`, `crash.rs`, `restart.rs`) and a page dogfood (writer, a fake model). Left: a tag (the owner's OK), ARK Desktop's pin and a native check after it, Windows process start times (`ark-os`), deduplicating Observe calls that write records (`board.add`) if a run shows it | the owner (tag); ARK Desktop (pin) |
| Project processes (ADR 0041, in v0.13.0): ARK Desktop turns them on (`.processes(true)`) with its v0.13.0 pin. The agent's browser reads the pages a project's processes serve themselves (ADR 0046, on `main`). Later: a runtime elsewhere forwards a process's port so the Browser view reaches it (ARK Cloud), Windows (a job object), a dogfood on a hosted model | ARK Desktop; ARK Cloud |
| The Browser panel as a view of its own in the window (ADR 0044, in v0.13.0): checked natively only on duckduckgo.com; the rest of ADR 0044's Check needs a person at the screen (a Next.js dev server, a sign-in, a Desktop dialog, resize, project switch, reload, This Mac \| Cloud, Dock reopen, a notification; and since "can't be reached": `localhost:3000` down then up, a slow dev server, a plain http site with ARK Desktop's Info.plist, a page without a background in light and dark). Later: ARK's keys (⌘K, ⌘N, Esc) as menu accelerators so they work while the page has focus | the owner (the native check); ARK |
| Rename "Allow for this session" in approvals | the owner decides the words |
| Typed Codex replies (ADR 0049): a live `analysis` and `workbooks` bench on `codex:default` against `bench/RESULTS.md` (2/2 each, about 230k tokens a run, before); the input-token cost of the schema in a long session. Later, if a reason comes: strict function calling for OpenAI-compatible APIs from the same converter | the owner's OK for the live bench |
| The Computer (ADR 0042, 0045, 0056, in v0.18.0): off until the person turns it on; app-neutral native control; sequenced activity and targets outside ARK; Stop shares the dispatch lock; message draft, send and proof are separate; durable draft and unconfirmed-submission records survive restart and block unsafe mutations; trusted unresolved effects replace false success claims before storage or display. Checks cover wrong foreground apps, restart, status-only message changes, keyboard cancellation, stale overlays and trailing streamed whitespace. Left: grants per app and a controlled native self-chat run in ARK Desktop | the owner: grants per app; ARK Desktop: native self-chat run |
| Attention, workers, where work runs (ADR 0043, accepted 2026-10-07, after the One and herdr review): step 0 an ACP spike (an ARK task on Claude Code and Codex: permission, kept question, answer, continue, result and file back), 1 one attention list in a runtime, 2 one ACP worker (ADR 0039 H1 to H3), 3 notifications to a phone and My machine as a location, 4 attention across runtimes. Order and checks: ROADMAP › Work runtime | Steps 0 to 4 done, in v0.13.0 (ADR 0043 › Step 0 to Step 4 results, ADR 0047). Left for the owner: real phones (iPhone Home Screen, Android), ARK Desktop's My machine at the screen. Found: after a restart an ARK agent repeats its step, and a local model may write a different change, so the person is asked again (Claude Code loads its session and is not asked again) |
| Board: whether the chat uses the board unasked for project-sized work, and hosted models (the `board` workload passes 3/3 live on qwen3:4b when asked); board tools for code-defined team leads | before turning anything on or off by default (ADR 0023) |
| ARK Desktop moves its keys to ARK's store (`ark_connect::ApiKeys`, the `api_key_*` methods) and drops `setup::keychain` and `load_keys` | ARK Desktop |
| A paid-model run of scheduling: qwen3:4b claims schedules it never made | needs the owner's OK |
| ARK Desktop uses `ark_files::Files` instead of its own `files.*` adapter (same action names), and turns Files on | ARK Desktop, against `../ark` until v0.10.0 |
| Files: cell styles in the grid, images in a document, undo across content saves, `.xls`, `.doc`, chart creation and more formula functions | when a real task needs one (ADR 0019 › Consequences) |
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
evidence (time-boxed), persistent headless ARK (shipped), ARK Cloud on the same
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
8bd8b96), within the gate. In v0.20.2, the page gate maps
`RUST_TEST_THREADS` to libtest's real setting. Its fake OAuth browser decodes complete form values
and retries transient loopback closes. The former connect-mid-task flake passes 25 of 25 focused
runs and the complete 43-scenario page gate.

## Working notes

- Several Claude sessions may edit this tree at once. Agree a file split before editing (see
  `../WORKSPACE.md`, Sessions).
- `.playwright-mcp/` is scratch output from browser dogfooding. It is untracked; delete it when
  done.
