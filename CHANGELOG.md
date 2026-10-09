# Changelog

What changed for people building on ARK, per release. Not a commit log.

- Sections: `Added`, `Changed`, `Fixed`, `Breaking`. A `Breaking` entry says what an app must
  change.
- New entries go under `[Unreleased]`; a release renames it (see `docs/RELEASING.md`).
- One line per change where possible. Link to a doc instead of explaining it here.

## [0.18.0] - 2026-10-09

### Breaking
- `ark_computer::Element` literals add `identifier`, `description`, `focused` and `frame`;
  `Look` literals add `window_identifier`.
- `ark_tools::ActionOutput` and `ark_core::Invocation` literals add `effect: None`. Use
  `ActionOutput::json` where possible.
- `computer.type` no longer accepts or presses `submit`. Native messages use
  `computer.draft`, `computer.send` and `computer.cancel`.

### Added
- ADR 0056: native message sends prove the exact conversation, staged draft and new outgoing
  message. A durable global safety record blocks retries and other mutations when a draft or
  submission still needs attention.
- Computer activity has sequenced phases and target frames. The native window can show where
  ARK is acting, with Stop and Check again available in the window and menu.
- Trusted external effects can be confirmed or unconfirmed independently of model-visible tool
  output. An unresolved effect replaces an optimistic final answer before it is kept or shown.

### Changed
- Computer brings the approved app to the front and revalidates the conversation inside the
  dispatch lock immediately before posting Return. Stop, release and send share that lock.
- Project processes refuse common direct desktop automation commands and point the agent to
  Computer. This is a usability guard, not a sandbox boundary.

## [0.17.0] - 2026-10-09

### Breaking
- `ark_model::Backend` has `model_options`: a literal needs `model_options: vec![]`.

### Added
- ADR 0054: native apps can opt in to Tauri's signed background updater, with state exposed
  only to the app's own page and no forced restart.
- ADR 0055: Claude Code and Codex model specs can name effort as `model@effort`. Codex models
  and their supported efforts come from the signed-in CLI instead of a static Default entry.
- Settings › Appearance has Monokai, Dracula, GitHub and Xcode palettes for the shared UI,
  editor, charts and terminal.

## [0.16.0] - 2026-10-08

### Breaking
- ADR 0052 resource locality: `ArkUi::handler` is replaced by `project_handler` and
  `window_handler`. A project handler receives the project from the resource reference. A
  window handler has no project context and never follows a remote chat.

### Added
- ADR 0052 resource locality: `ResourceLocation` and `ResourceRef` bind Terminal, Files,
  editors, spreadsheets, documents, processes, artifacts, invocations, project panels and the
  Browser to the runtime, project replica and chat that opened them. The `resource` and
  `chat_resource` protocol validates that identity and refuses unknown routes instead of
  falling back to the window's computer.
- A Browser opened for a chat on another runtime is a rendered, interactive Chromium page on
  that runtime, with resize, pointer, wheel, keyboard, navigation, popup and reconnect state.
- Project terminals have a generation and bounded cursor replay. Two chats on one project
  replica can observe the same terminal and processes through their own resource references.

### Changed
- Project transfer parts resume by byte offset. Apply and settle receipts are bound to a
  monotonic replica epoch, reconciliations serialize per replica, and completed archives
  publish atomically with the exact packed-content manifest. A Bring interrupted after local
  apply resumes only while that content is still present; later edits become an ordinary
  conflict. `Bring to this Mac` remains the only reconciliation action.

## [0.15.0] - 2026-10-08

### Breaking
- `ark_interact::Effect` has `Open(session)`: a `match` on it needs an arm for it (or `_`).
- `ark_model::Backend` has `step`: a literal needs `step: None`.
- `When` has `date`, `cron` and `tz`: struct literals add them or `..Default::default()`.
  `EventKind::ScheduleChanged` is new: an exhaustive `match` adds it. `ScheduleRecord`,
  `TaskRecord` and `TaskSpec` have new fields (see Added) for struct literals; stored records
  read with defaults.

### Added
- ADR 0051: each conversation turn reads the project folder's `AGENTS.md` as the project's
  rules (sections within 1,500 tokens, the rest named, subfolders with their own listed).
  `ArkBuilder::project_instructions(names)` names other files, or none to turn it off;
  `rules::project_instructions` (sections chosen by the task) and `RuleSelection::with_project`
  are new.
- ADR 0052: a chat runs on the computer the person picks. A runtime holds chats for another
  through calls that name the chat (`chat_new`, `chat_send`, `chat_read`, `chat_approve`,
  `chat_model`, …) and never move its own windows; the other keeps a record per chat
  (`ark_interact::held`), passes the window's calls there, and numbers that computer's events
  into its own stream, a question's id becoming `<runtime id>:<id>`. `new_session {place}`,
  `sessions` rows with `place`, `places`, `/chats` and `/resume` of a held chat
  (`Effect::Open`). `Links::chat`, `chat_events`, `add`, `remove`, `state`, `runtime_id`.
- ADR 0052: the window's place chip beside the model in an empty chat, the line above the
  composer ("3 files changed on vps · Bring to this Mac", a choice per file changed on both
  sides, "vps is not answering · Try again"), marks in the sidebar and the header, and
  Settings › Computers: a host from `~/.ssh/config`, `user@host`, or an address and a code.
- ADR 0052: `ArkUi::computers(ComputerSetup { contract, build, secrets, ssh_config })` lets the
  person add computers over SSH (setup, a tunnel kept up, a link); `ArkUi::keep_computer` moves
  a server an app kept before. `ark_ui::ark_reach` re-exports the crate for the contract.
- ADR 0052: who answers on a computer is that computer's: the model menu lists its models;
  Install and Sign in run the program's own step there in a terminal sheet (`place_setup`,
  `Cli::install_line`, `Cli::sign_in_args`, catalog `Backend::step`); a key goes there only
  when asked (`place_key`, `ApiKeys::saved_key`) and is forgotten when the computer is removed.
- `scripts/linux-build.sh PACKAGE BIN`: a program's Linux build for a server (Debian 11).
- Examples: the writer serves on a computer with its own contract (`--version`,
  `--revoke-token`, `WRITER_SERVER_BUILD`).
- ADR 0052 step 2: `ark_ui::copy`, a project's copy on another computer. `select` (a git
  folder's `ls-files`, else all but build folders), a `Manifest` per copy (a `copy_manifest`
  record), `changes` against it, `plan_send` (never a file the other side changed), `pack` (a
  `tar` stream), `apply` and `apply_first` (refuse paths and links out of the folder, never write
  through a link, drop setuid and setgid, keep a version before each replaced file), and
  `both_changed`. `Engine::keep_before` keeps a project file as a version before (ADR 0034);
  `restore_file` uses it.
- Window: code in the file editor is coloured from the theme (keywords, strings, numbers,
  literals, comments) for 12 language families. Other files keep the plain editor.
- ADR 0052 step 0: `ArkUi::serve("unix:/path/to.sock")` serves on a Unix socket, with saved
  tokens only. The socket is 0600; its folder is made 0700, or refused when others can write to
  it. A socket left by a stopped process is replaced; a live one stops the second server
  ("already serving"). The file is removed when serving stops.
- `remote::Client::new("unix:/path/to.sock", token)` reaches a runtime on a Unix socket, for
  calls and events; errors name the socket as they name an address.
- `ark_os::fs::private_dir`: a folder only its owner can change (Unix 0700, owner checked).
- ADR 0052 step 3: new crate `ark-reach` sets up a computer over the system `ssh`. It covers
  `hosts` from `~/.ssh/config` (through `Include`, resolved by `ssh -G`), a checked `Target`,
  one `Session` per setup (connect, look, install by SHA-256, start as a `systemd --user`
  service or in the background, pair), `remove`, and a `Tunnel` between Unix sockets that
  comes back after a drop. The app describes its program with a `ServeContract`. Container
  tests run with `ARK_TEST_SSH=1`.
- Scheduled work (ADR 0053, [`ORCHESTRATION.md`](docs/ORCHESTRATION.md#schedules)):
  `When::{date, cron, tz}` (one run on a date, five-field calendars, the person's IANA zone
  with daylight saving), `Orchestrator::{schedule_with, pause_schedule, resume_schedule,
  change_schedule, run_schedule_now, schedule_runs, schedule_record, schedule_state,
  chat_tools_in_zone}`, `ScheduleChange`, `ScheduleState`, `IfMissed`; schedule policies
  `if_missed`, `overlap`, `retries`; a schedule pauses itself after 3 failed runs in a row and
  keeps its newest 50 runs. `TaskSpec::retry_after` and `TaskRecord::{slot, retry_delay_ms,
  retry_at_ms}`. `agents.schedule` takes `schedule` (change, `resume`, `run_now`), `date`, `cron`,
  `tz`, `if_missed`, `overlap`, and no `to` (the schedule gets its own agent, ended by
  `Orchestrator::stop_schedule` when nothing else uses it); its description says today's date. `agents.unschedule` takes `pause`.
- `ark_interact::orchestration::{schedules, schedule_view, schedule_runs}`,
  `Control::{PauseSchedule, ResumeSchedule, RunSchedule}`, `/schedules run|pause|resume|stop`,
  `Interaction::set_zone`, `Siblings::set_zone`.
- The window: a line per schedule of the open chat above the composer, and the schedule in the
  side pane (runs, results, files, Run now, Pause or Resume, Stop). Page methods `schedules`,
  `schedule`, `schedule_run`, `schedule_pause`, `schedule_resume`; `hello` takes `tz`.

### Changed
- ADR 0051: a project folder with an `AGENTS.md` changes every conversation turn's first
  message (its rules block). Apps that do not want it call `.project_instructions(Vec::<String>::new())`.
- The rules activity line reads "Following N rules (M more not shown)" and names the file or
  set the rules come from.
- ADR 0051: a turn whose model restates its full answer beside a second check of the files it
  changed ends on that answer; the check does not run.
- HTTPS uses rustls instead of the system's OpenSSL (`reqwest` without default features),
  with the system's certificates and Mozilla's built in. A Linux build needs only glibc, and a
  server without CA certificates still reaches HTTPS sites (ADR 0052).
- `agents.schedule` asks each time (`ask_each_time`): no "always" answer covers making or
  changing standing work. Its approval shows when and, in full, what each run is handed. Its
  larger schema costs a chat that orchestrates about 200 input tokens a turn: the simulated
  `board` workload goes from 14,899 to 16,075 tokens (+7.9%, success unchanged);
  `bench/baseline.json` regenerated.
- A task tries again only after a failure that may pass: out of credit, a missing project or
  action fail at once. A wait of 10 s or more to try again is kept on the task (`retry_at_ms`)
  and shown as `waiting_on: "trying again at …"`.

### Fixed
- A task's second attempt repeated the state-changing calls its failed first attempt made; it
  now continues it (`done_before`, its own calls only).
- A crash between starting a scheduled run and saving its schedule ran that time twice; a timer
  tick could undo a stop or change made while it ran; intervals drifted by up to a tick per run.

## [0.14.0] - 2026-10-07

### Breaking
- ADR 0049: `HarnessEvent::TextWithdrawn` is a new event; an exhaustive `match` adds it.
- ADR 0050: `Settings::context_budget` is `Option<u32>` (`None`: sized from each agent's model).
  `ark_model::Endpoint` has `context_window` (struct literals add `context_window: None`, or
  use `Endpoint::new(..).context_window(n)`). `ProviderError::Refused` and
  `AgentError::UnusableReply` are new variants, and `HarnessEvent::Retry` has `repair`: an
  exhaustive `match` adds them or `..`. `Unfinished::carry` takes the request budget.
  `Turn::{goal, ran_out}`, `HarnessOutcome::ran_out`, `Invocation::narration`,
  `ContextReport::ledger` and `Unfinished::{lines, goal, error, stopped}` are new fields for
  struct literals.
- New fields for ADR 0048: `Invocation::{work, pid_start}`, `session::Turn::pid_start`,
  `TaskSpec::work`, `TaskRecord::{work, pid_start}`, `ApprovalRequest::cut_short`,
  `RetryPolicy::offline`, `RunCtx::{work, continues}`, `TranscriptTurn::{interrupted, resume}`.
  Struct literals add them (`None`, `false`, `Duration::ZERO`, `Vec::new()`). `HarnessEvent`
  has `Interrupted`, and `HarnessOutcome` has `interrupted`: an exhaustive `match` adds an
  arm, a literal adds the field.
- `TranscriptTurn::unfinished` is now the whole sentence to show ("Interrupted before it
  finished.", "Did not finish: <error>"); a front-end that added "Did not finish: " drops it.
- `ark_model::session::Delta` has a `tools` field (every tool available now, for a backend that
  shapes its reply by them). Code that builds a `Delta` with a struct literal adds
  `tools: Vec::new()` or `..Default::default()`.

### Added
- `ark-os` (ADR 0022): what differs between operating systems, behind one API. Folders
  (`dirs::home`, `app_home`, `documents`), finding programs with `PATHEXT` and installers'
  folders (`find::program`, `find::app`), starting any program in its own group with no
  console window and stopping it with everything it started (`process::command`, `Tree`),
  opening links and files without a shell (`open::open`), `fs::replace` and `write_atomic`
  that retry while another program holds the file, `fs::name_problem`, `time::local_offset`,
  `shell::interactive`. Re-exported as `ark_sdk::prelude::ark_os`.
- The page tests run in Chromium too (`ARK_UI_BROWSER=chromium`), the engine of Windows' WebView2.
- The window reads the local way: Ctrl in shortcuts and their labels on Windows and Linux
  (`web/js/keys.js`, `data-os` on the app); the room for the Mac window buttons only on
  macOS. In the desktop window, no reload or print keys and no browser menu except on text.
- The desktop window: a second launch brings the open window forward (one runtime per store),
  and a window larger than the screen is fitted to it and centered.
- `ark_os::process::login_path` (a windowed app gets the terminal's `PATH` on macOS and Linux)
  and `attach_console` (on Windows, a windowed app started from a terminal prints there).
- `ark_connect::secrets::Keychain::lasting()`: whether saved secrets outlive a restart here.
- An agent never loses its task (ADR 0050). The first message of a turn (the task, the goal it
  continues, the carry-over) is pinned in the working set and never dropped; dropped steps leave
  a ledger, one line per call (action, file or query, result, `inv:`) and the model's last note.
  `WorkingSet::pin`, `ledger_tokens`; `ContextReport::ledger` ("Earlier steps").
- A turn after an unfinished one works on that one's goal with the new message added
  (`session::Goal`, `Turn::goal`, `Session::goal_for`, `begin_turn_with_goal`,
  `Harness::goal`): "continue" never becomes the goal, and a chain of any length keeps its first
  request and every later instruction. Rules, skills, preloaded tools and the trivial class read
  the goal. Messages sent while a turn runs stay in the working state and are stored at once
  (`Session::steered_now`), and kept when the turn fails.
- A turn that reached its step limit (`HarnessOutcome::ran_out`, `Turn::ran_out`) keeps its
  answer and is continued like an unfinished one: the next turn keeps its goal and does not make
  its changes again.
- The next turn after a stopped one is told why it stopped and the model's last note, kept on
  each call (`Invocation::narration`, `Engine::narrating`); it looks back over up to 20 stopped
  turns, read from the store.
- `Provider::context_window` (and `SessionBackend::context_window`): Claude models 200k, Codex
  272k, OpenAI by family, an endpoint when it declares one (`Endpoint::context_window`). Every
  wrapper passes it on. `budget::budget_for`, `Harness::request_budget`, `budget_or_auto`.
- A chat turn keeps its progress and continues where it stopped (ADR 0048). Its calls name the
  turn (`Engine::for_work`, `Invocation::work`); its trace is saved as it runs; a stopped turn
  shows each call done, failed, cut short or not run, "Interrupted before it finished." and,
  on the chat's last turn, one Continue. The next turn, whatever the person says, is told what
  the stopped turn did (`Session::unfinished`, `Unfinished::carry`), gets its changes as
  `done_before`, asks again about a change cut short (`Harness::cut_short`), and takes up a
  hand-off still under way (`Orchestrator::take_up`, `Adapter::continued`).
- `ark_runtime::process::{own_start, start_of, writer_alive}`: a record's writer is its pid and
  its process start, so a pid reused after a reboot is not taken for it.
- `ARK_MODEL_OFFLINE_S` (default 60): a model that cannot be reached is waited for that long
  (`RetryPolicy::should_retry_for`, `ProviderError::is_unreachable`).

### Changed
- The request budget is sized from the reasoning model's window: a quarter of it, 6,000 to
  48,000, at most half. A vendor session's window is its rotation limit (64k), so a Claude or
  Codex subscription lane gets 16,000. Unknown windows keep 6,000. `ARK_CTX_BUDGET` and
  `.context_budget(n)` still set it.
- A reply the model wrote that cannot be used (arguments that do not parse, a reply that is not
  structured) is not sent again unchanged: the model is told what was wrong and sends it again,
  at most 2 times in a row and 4 in a turn (`Retry { repair: true }`, "Redoing its last step").
  After that the turn fails with `AgentError::UnusableReply`, which both front-ends word as "The
  model's reply could not be used." A refused reply schema is `ProviderError::Refused`, retried
  as before.
- The model's note with its tool calls is kept in its round for the ledger and on each call
  (`Invocation::narration`), but not sent while the round is shown: live on qwen3:4b, sending it
  made the model keep calling tools after it had its answer.
- `code.run` says it is contained and points a project's commands to `process.start`;
  `process.start` says it runs tests and builds with the person's own programs.
  `Adapter::summary` (default none) puts a few words beside an adapter's actions in the tool
  index; `process` and `code` have one.
- A turn's goal says to do what the person's newest message asks when it asks for something
  else (after a Stop or the step limit, a new request is not folded into the old task). The
  follow-ups it shows shrink with the budget.
- The working state's `GOAL:` line is clipped to 300 characters (the full task is in the first
  message). Evidence lists an object's keys sorted.
- A stop by the app (SIGTERM, quit) leaves the chat's turn interrupted, not cancelled, and
  ends it with `HarnessEvent::Interrupted` (amends ADR 0038).
- `ARK_MODEL_RETRIES` defaults to 4 (was 2).
- Codex gets a reply schema built from the tools of the call (ADR 0049): each tool is a closed
  variant whose `arguments` is the tool's own schema in OpenAI's strict form, so arguments
  arrive as an object the grammar checked, not as JSON written inside a string. What strict mode
  cannot express travels as JSON in a string for that part only; nothing is repaired. A schema
  Codex refuses switches that model to the older string form and the call is retried.
  `ark_model::cli::codex_reply_schema`, `strict_problems`.
- A model call that fails with a temporary error is retried even after some of its text was
  shown. That text is withdrawn first (a new event, `HarnessEvent::TextWithdrawn`) and the retry streams its
  own: the window takes it back, the turn record drops it, text mode ends the line. Before, any
  shown text made the failure end the turn.
- `files.edit` describes each of its edits as a closed shape, and `files.write` the cells of a
  row.

### Fixed
- Windows: Claude Code, Codex, `npx` and tools are found (`PATHEXT`, npm's `.cmd`
  launchers run their target directly, not through `cmd.exe`); a cancelled call stops what it
  started; Connect's sign-in link opens whole; schedules run at local time; a live process
  counts as alive; data folders no longer depend on `HOME`.
- Every program ARK starts (vendor CLIs, MCP servers, the browser) runs in its own process
  group, so a cancelled call or a closed browser stops its children too, on macOS as well.
- Files ARK creates must have a name every system accepts (no `:`, `?`, `CON`, ...).
- A fresh browser starts in a new profile folder each time.
- Text files keep how they are stored when the agent or the person edits them: CRLF lines stay
  CRLF, UTF-16 stays UTF-16 (and now reads as text). Skills saved with CRLF read cleanly.
- Paths ARK shows or compares no longer carry Windows' `\\?\` prefix.
- An agent on Codex no longer forgets its task part of the way through: the task left the
  working set after two file reads, and a vendor session that rotated started without it. A
  follow-up "continue" no longer becomes the goal (ADR 0050).
- After a crash and a reboot, a reused pid no longer keeps a dead turn, call or task "running";
  the next turn no longer overwrites the stopped one.
- A chat whose first turn was cut short is listed (with its title) as soon as the store opens.
- A turn stored in full just before the process stopped is no longer shown unfinished.
- `Store::put_blob` flushes the bytes and the folder before naming an object, and rewrites an
  object of the wrong size.
- An agent on Codex no longer stops with "arguments for files.edit are not JSON" on a long edit
  (an unescaped quote in JSON inside a string).
- Two Codex models running `codex exec` at once in one process no longer write and delete the
  same schema file.
- A vendor session that failed is discarded (its files removed unless kept), not left behind.

## [0.13.0] - 2026-10-07

### Breaking
- `ark_orchestrate::TaskRecord` has `decisions` and `ark_orchestrate::Usage` has
  `cost_micro_usd` and `asked` (ADR 0043 step 2): code that builds either with a struct
  literal adds the fields (`decisions: Vec::new()`, `..Default::default()` for `Usage`). The
  next tag is 0.13.0.
- `ark_connect::State` has a new variant, `Off` (the Computer while the person has not turned
  it on): an exhaustive `match` adds an arm. `ark_interact::capabilities::Parts` has a
  `computer` field; build it with `Parts::of`.

### Added
- The Computer is a capability the person turns on (ADR 0045). Off, a turn carries one line
  saying the agent can ask for it, and no computer schema. An agent that needs an app calls
  `acquire_capability`; the person sees "Allow ARK to use the apps on this computer" with Allow and Not now;
  Allow asks for macOS's permission, turns it on for good (the store's `computer.json`) and the
  tools join the running turn. Settings › Capabilities and `/connect computer`,
  `/disconnect computer` turn it on and off; off takes the screen back at once.
  `ComputerService::switched`, `is_on`, `set_on`; `ark_computer::TurnOn`,
  `names_the_computer`; `PerAgent::acquisition`; `AcquisitionProvider::asks_a_person`;
  `Ark::acquisition_for`; `State::Off` in the capability list; the page's `capability_turn`.
- The Browser panel shows any page in the window (ADR 0044): it is a webview of its own over the
  pane, so pages that refuse frames (`X-Frame-Options`, `frame-ancestors`: most sites, and dev
  servers with security headers) show instead of a white panel. It gets no IPC and never loads
  the app's own page, and steps aside while ARK's menus, palette or dialogs cover it; an app's
  own overlays carry `data-ark-overlay`. In a plain browser the panel stays a frame and says
  when a page refuses (`frame_check`). `ark-ui` now needs Tauri's `unstable` feature (`~2.12`).
- `ark_tools::web::public_client()`: a client that only connects to public addresses.
- An agent's browser opens the pages a running process of its project serves itself (a dev
  server it started), in the fresh browser; a page an action lands on is checked too (ADR 0046).
  `Processes::serving`, `BrowserService::serve_local_pages`; `web::local_host`, `private_ip`.
- The Browser view says when an address does not answer ("localhost:3000 can't be reached",
  Try again) instead of staying white, shows a loading line in the bar until the page
  finishes, and shows the pane's colour, not a white block, until the page arrives
  (`ark_browser_reach`, ADR 0044).
- An agent can do its tasks on another agent program installed here, Claude Code or Codex,
  with that program's own tools (ADR 0043 step 2): its model is `agent:claude-code` or
  `agent:codex` (`agent:claude-code:sonnet` names the model), when the app turns it on
  (`ArkBuilder::workers`). The task stays ARK's. The program starts with a clean environment,
  without the person's or the project's own settings, in the mode that asks; what it wants to
  change or run is an ARK question that names the file or the command ("Always in this
  project" covers only that), kept on the task and answered from any client. What it changed
  is found in the project folder and kept as the task's outputs, with versions before. After a
  restart it loads its own session, and an answer given in an earlier run is not asked again.
  New crate `ark-acp` (the Agent Client Protocol client and the catalog of programs);
  `ark_runtime::worker` (`Worker`, `Workers`), `ark_runtime::folder` (what work changed in a
  folder, shared with Run code), `Engine::keep_produced`, `TaskRecord::decisions`,
  `Usage::cost_micro_usd` and `Usage::asked`. Measured against ARK's own agent:
  `bench/RESULTS.md`. ARK's agent stays the default. `ark_exec::written_since` is now
  `ark_runtime::folder::written_since`, re-exported.
- What waits for you reaches your phone (ADR 0043 step 3, ADR 0047): Web Push sent by the
  runtime itself, encrypted to the device, with no ARK service in between. Settings › Devices:
  Notifications on this device, a code to sign in another device, the devices that get
  notifications. Something waiting (a question, a failure, a result nobody saw, a chat's own
  question) reaches each device once, a minute after it appeared, if it still waits and that
  device's page is not in view; a tap opens the page on it (`#item=`). A device signs in with an
  8-character code from a signed-in one (`POST /pair`, `Tokens::pair_code`,
  `Tokens::redeem_as`) for 90 days, named under the device that made the code
  (`laptop › Phone Safari`): revoking that one signs it out too, and Settings › Devices signs it
  out on its own. Its notifications end with its session. The page has a manifest (the app's name, accent and logo)
  and a service worker (`sw.js`); a browser with no session on such a runtime sees only the code
  field. Host methods `push_key`, `push_add`, `push_remove`, `push_devices`, `push_here`,
  `devices_signed_in`, `device_sign_out`; `Tokens::info`;
  `ark_ui::push`, `ark_interact::attention::{Notice, notice}`.
- What waits on another runtime of yours shows here too (ADR 0043 step 4): an app links one with
  a name, an address and a token (`ArkUi::link`, `ark_interact::links::Links`), and its
  questions, failures and results join this runtime's list, named ("My machine · novel-b"),
  each answered, cleared or opened on its own runtime (`attention_all`, `attention_answer`,
  `attention_go` (the app's own window only), and `runtime` on `attention_open` and
  `attention_clear`; `/waiting` in the window's command line, and in a terminal connected to
  it). A link is followed in the background, so a slow or sleeping one never
  holds up the list; while it does not answer, its items stay, marked, and cannot be answered;
  one that refused the token says to pair again. The token stays in the app's process. An app's
  own calls change the links with `HandlerCtx::links()` (a person pairs or forgets a server).
  `ark_ui::remote::visit_item` opens another runtime's page on an item; a page signed in with a
  token keeps `#item=`. The writer example: `--link NAME=URL` with `WRITER_LINK_TOKEN`.
- Settings fit a phone: the sections are a row above the one shown, and a row's controls go
  under its words.
- What waits for you, in every project (ADR 0043 step 1, `ark_interact::attention`): a question
  an agent's task keeps, work you started that failed, and a result posted into a chat while
  nobody was in it. One list in both front-ends: `/waiting` in the terminal (numbers answer the
  list you saw: `/waiting 2 once`), and rows in the window's tray above the questions (open goes
  to the chat; a failure with no chat shows why under its row; x or Delete clears). Opening a
  chat, or a project on it, clears what was posted there. An agent's question from another
  project names that project on its card (approvals carry `project`). Clearing is the only new record (`attention_cleared`) and
  survives a restart. Host methods `attention`, `attention_open`, `attention_clear`; event
  `attention`. `OrchestratorSet::subscribe` (every project's events) and `answer` (a kept
  question in any project); `ark_orchestrate::all_tasks`.
- Project processes (ADR 0041, `ark-proc`, `ArkBuilder::processes`): agents start, watch and
  stop long-running commands in the project (`process.start`, `list`, `stop`): a dev server, a
  watcher, a worker, with the person's own shell and tools. Not contained, so every start asks,
  showing the command and the folder, and "Always in this project" covers only that command in
  that folder. Ready is decided by code (a local address in the output that answers, or a
  `port`). The page has a Processes panel (live output, Stop) that opens on a start, and opens
  the process's page (`open`) in the Browser panel once ready. `/processes` lists and stops
  them in both front-ends. Processes stop when the runtime stops; a crash leaves a record that
  the next start sweeps. macOS and Linux.
- A program a process needs and does not find (Node.js, pnpm, Yarn, Bun, Deno, uv, Rust) is
  installed through `acquire_capability` after the person allows it ("Install Node.js", with
  the exact command); the install runs as a process they can watch.
- `ark_runtime::Sources`: several acquisition sources behind a profile's one slot (accounts to
  connect, programs to install); `AcquisitionProvider::knows` says which need goes where. An
  `acquire_capability` question's yes button is the first word of its line ("Connect",
  "Install").
- The Computer sees and does more (ADR 0042): `computer.menu` lists and chooses any app's
  menu items, with their shortcuts; `computer.scroll` scrolls a window or one part of it without
  moving the pointer; `computer.click` takes `double` (open a file, a chat) and `right` (its
  menu), and clicks with the pointer when a control takes no press. A look lists rows and list
  items (Finder's files, a table's rows) and what an app made pressable, and only what is
  scrolled into sight; names lose invisible marks, and a phone number read out digit by digit
  comes back whole. Electron apps are asked to build their accessibility tree.
- Pictures for apps that show little to accessibility (ADR 0042): `computer.see` takes a picture
  of one app's front window (never the whole screen) and `computer.point` clicks a spot in it.
  The picture goes to the model with the result: `ark_tools::ActionOutput::pictures`
  (`with_image`), `ark_model::Message::pictures`, `Provider::reads_pictures`. Sent by the
  Anthropic and OpenAI APIs, Ollama, Claude Code and Codex's app server; a provider that reads
  none gets a line instead. Pictures stay in memory, only the newest stays in context, and each
  counts `PICTURE_TOKENS` in the budget. Taking one needs macOS's Screen Recording permission.
- `ActionDescriptor::grant_per(group, fields)` (`ark_core::GrantKey`): one session or project
  answer covers the calls with the same values of some input fields. Its buttons name what they
  cover: "Allow WhatsApp for this session", "Always allow this command"
  (`ark_interact::grant_scope`).
- A question to click a spot in a picture (`computer.point`) shows the picture with a ring on the
  spot, so the person judges the spot, not the agent's words for it. A click is refused once a
  newer picture of that app was taken. A step that took a picture shows it, small, under the step
  (click to enlarge) while the runtime keeps it (the last 12). Host calls `computer_picture` and
  `picture`; `ComputerService::picture`, `Engine::picture`.

### Changed
- `ArkBuilder::computer(true)` starts with the Computer off until the person turns it on, asked
  the first time a task needs it (writer users allow it once). `computer_with` is as the service
  says: on unless it has a switch.
- No grant covers `acquire_capability`: each request asks (ADR 0045), and one for a capability
  that `asks_a_person` asks also when the agent may change things without asking.
- An answer given in a task carries into the run that continues it after the process stopped
  (`TaskRecord::decisions`): a refusal, or leave to change a file, is not asked again; leave to
  run a command is, since it may have run already. A pause, a retry or a new task asks again.
  `Usage::asked` counts the questions of every task.
- On macOS, `remote::visit` and `run_window` refuse a runtime elsewhere by name over plain
  http (use https); IP addresses, names without a dot and `.local` still open over http. The
  token is no longer sent in clear there.
- The terminal keeps an agent's question open when the turn that showed it ends, instead of
  refusing it: the task keeps waiting, `/waiting` answers it, and the next turn asks again. A
  question answered meanwhile is not asked again (`PendingApproval::is_closed`). A chat's own
  question is still refused when its turn ends. Several questions in one turn are asked one
  after another (a second one used to replace the first, which was refused), and closing the
  terminal refuses none: the tasks keep them for the next process.
- `present` says what `locator` is for each view: the address for the browser (this machine's
  too) and the command for the terminal. An agent asked to show a page on `localhost` now opens
  it in the person's Browser view instead of giving up.
- `browser.open` refusing an address on this machine says to show the page in the person's
  browser view instead.
- The approval card has no caption under its buttons: what "for this session" covers is on the
  button itself when it is narrower than the action.
- `computer.type` types as keys (the field focused, its text selected, then replaced), so apps
  that ignore a text set from outside (WhatsApp's Send button) see the change. A field that
  takes no focus still gets its text set.

### Fixed
- `find_capability {need: "computer"}` no longer answers "Computer is not connected": account
  sign-ins resolve only capabilities that need an account.
- The Browser's address bar opens `localhost:3000` (and `127.0.0.1`, `[::1]`, `*.localhost`
  addresses) over http instead of searching for it.
- The board's "Needs you" lights for a task waiting on your answer: the host's `board` response
  carries each task's `question`.

## [0.12.1] - 2026-10-07

ARK in any browser and on a phone: files in without drag and drop; Run code on Linux.

### Added
- The page: Upload a folder… (the sidebar's add menu, where there is no folder picker) makes a
  new project from a folder on this device, through the upload calls (ADR 0030), and opens it.
  A browser on a runtime elsewhere has nothing else; a browser on the runtime's computer keeps
  Choose a folder… (a typed path) too.
- The page: Add files (the composer's +) puts chosen files in the project, as a drop does (ADR
  0035). A phone has no drag and drop.

### Fixed
- Run code on Linux: Python failed to start in the sandbox when uv reached the interpreter
  through its `cpython-3.12-…` link (every uv 0.8+ install). A folder a run reads through a
  link is now mounted at both paths. First run of `ark-exec`'s tests on Linux: all pass with
  `ARK_TEST_PYTHON=1` (bubblewrap 0.8, uv 0.12.23, Debian bookworm).

## [0.12.0] - 2026-10-06

A browser comes in through a one-time link and keeps a session in a cookie (ADR 0040).

### Breaking
- A page served by `ArkUi::serve` runs only scripts it serves: an inline `<script>` or a script
  from another origin is blocked by its Content-Security-Policy. Serve it as a file
  (`ArkUi::asset` and `ArkUi::script`).
- `TokenInfo` has `expires_ms` (None for a saved token): code that builds a `TokenInfo` sets it.

### Added
- A browser comes in through a one-time link and keeps a session in a cookie (ADR 0040):
  `Tokens::link`, `Tokens::redeem`, `Tokens::end`, `TokenInfo::expires_ms`, `ArkUi::entry`, and
  `GET/POST/DELETE /session` on a served runtime. The page trades `#code=` for an HttpOnly,
  `SameSite=Strict` session cookie, goes to the entry when it has no session, and stops
  reconnecting sooner with an entry ([`docs/REMOTE.md`](docs/REMOTE.md)).

### Changed
- A served page has a Content-Security-Policy (only its own scripts, no framing),
  `Referrer-Policy: no-referrer` and `nosniff` on every answer. An inline script or one from
  elsewhere no longer runs there.
- A request carried by the session cookie that changes something must come from the page
  (`Origin`); an event stream ends when its token or session is revoked or over.
- `access.json` changes hold a lock (`access.json.lock`).

### Fixed
- The page's "Disconnected" toast in a plain browser no longer shows "nullnull".

## [0.11.0] - 2026-10-05

Work survives a restart and a stop, and what it waits for is the runtime's (ADR 0036, 0037,
0038).

### Added
- A question an orchestrated agent asks is kept on its task (`TaskRecord::question`, `Question`):
  the task is `Waiting` on `WAITING_PERSON` and gives its capacity back; events `Asked` and
  `Answered`. `Orchestrator::questions()` lists them; `Orchestrator::answer(task, decision)`
  answers from any client, also after a restart. The window's `hello` lists kept questions
  (their `id` is a string) and `approve` takes that id; the terminal has
  `/task answer <task> once|session|always|deny`. `ApprovalGate::remember` keeps what an
  answer grants.
- A restart continues open work: pending and queued tasks run, started ones continue from their
  working state, a task with an open question waits for the answer, a suspended one stays
  suspended. A task whose state-changing call was cut short fails with that call named
  (`retry` runs it again) and reports to its chat. Apps open every project with open work or
  schedules when they start (`projects_with_work`).
- `Harness::done_before(invocations)`: a run that continues an interrupted one (a restart, a
  resume) does not repeat a state-changing call an earlier run made; the brief lists them.
  `ark_runtime::engine::INTERRUPTED_CALL` names a call a stopped process left running.
- A task the chat hands off (`agents.delegate`) reports to that chat (`report_to`), so its
  result arrives when the turn did not wait for it (`wait: false`), was stopped, or the process
  restarted. `TaskRecord::reported` makes it once: the waiting turn's answer, or a post. A
  restart posts a result that had not arrived; `Post::work` names the task a post is about and
  `ark_runtime::session::posted` finds it.
- A graceful stop: `Engine::stop_soon()` and `Engine::runs_ended()`, watched by every turn and
  task (`Harness::stopping`). Runs stop at their next safe point; a batch with a state-changing
  call finishes first. An orchestrated task stopped this way is saved to continue in the next
  process, not failed. `ArkUi::serve` stops this way on SIGTERM as well as ctrl+c
  (`Host::stop(grace)`, `ARK_STOP_GRACE_SECS`, default 25), the window when the app quits, and
  the terminal when the person leaves. `RESTARTED` is exported.
- What an agent does now: `AgentNode::step` (its current call), worded once by
  `ark_interact::orchestration::doing`; `/agents` has a "doing" column and says how to answer a
  waiting task; the window's `graph` adds `doing` to each agent.

### Changed
- The board's "Needs you" reads questions from the tasks, not from approval events.
- `agents.delegate` with `wait: false` in a chat says the result will be posted there.

### Breaking
- `TaskRecord`, `TaskNode`, `AgentNode` and `orchestrate::Brief` have new fields (`question`,
  `reported`, `step`, `done`): code that builds them adds them (`None`, `false`, `&[]`).
  `session::Post` has `work` (`Post::new` sets it to `None`).
- `EventKind` has `Asked` and `Answered`: an exhaustive `match` adds them.
- `ark_interact::orchestration::graph_report(g)` is `graph_report(g, labels)`: pass the app's
  `Labels` (`ix.labels()`), or `Labels::new(vec![])`.
- `INTERRUPTED` is now the start of a failed task's error, not all of it (the call cut short
  follows).
- `SurfaceKind::Terminal` and `SurfaceKind::DiffViewer` are gone from `ark-core` (ADR 0036):
  they are a front-end's panels, not core kinds. Use `SurfaceKind::Other("terminal")` or
  `Other("diff_viewer")`; the tag a page sends (`kind_tag`) is unchanged.

### Fixed
- A delegated task's outputs no longer include the versions before that its calls kept for
  Restore (ADR 0034). Before, a task that edited `notes.md` handed its delegator two
  `notes.md` outputs, the new one and the old one.

## [0.10.0] - 2026-10-05

ARK works without a window and on other machines: several live chats, schedules that post
their results into the chat that asked, a runtime other machines and the terminal connect to,
the window visiting a runtime elsewhere, and folders sent there as projects. Agents get the
browser, the Computer, Files edited in place, Run code contained on macOS, Linux and Windows,
outputs opened by kind, data work, and the project's board. Optional ARK Accounts. A file a
call changes keeps its version before, and files dropped on the chat go into the project.

### Added
- The version before (ADR 0034): a tool that changes or removes a file of the project keeps the
  file as it was, `ProducedArtifact::before(kind, path, bytes)`. `code.run` copies the
  project's files before a run (clones on APFS, btrfs and XFS; up to 256 MiB a file, 512 MiB a
  run) and keeps the copy of each file it changed or removed; its output lists `removed` and
  `no_copy`. `files.write` and `files.edit` keep the file they save over.
  `HarnessEvent::ToolResult` and `Activity` carry `before: [FileBefore {path, artifact}]`.
  `Engine::restore_file(project, artifact)` puts a file back and keeps what it replaced
  (`Restored {path, undo}`); the window's `file_restore`. A turn's file cards have Restore and
  Undo, and a card for each file the turn removed.
- Files dropped on the chat go into the open project's folder (ADR 0035): `upload_add
  {upload}` after `upload_parts`, names in use get a number. The window takes files and folders
  dropped on the conversation, shows the progress with Stop, and puts `Added to the project:
  ...` in the composer. Event `files_changed {project, paths}` after a restore or a drop; open
  file views and the Files panel read the files again.
- ARK Accounts (ADR 0026): the new crate `ark-account`, optional. `Account::sign_in` (the
  browser, PKCE, loopback), `refresh`, `access_token`, `sign_out` (revokes this computer's
  token), `status`, and `can("cloud.agents")` for ARK Cloud features. Tokens stay in the OS
  keychain and in memory ([`EXTENSION.md`](docs/EXTENSION.md#an-ark-account)).
  `Account::set_computer_name` names this computer's sign-in on the person's account page
  (ADR 0031).
- `ark_connect::oauth::SignIn::landing(url)`: once the browser is back, send it to the
  provider's own page instead of ARK's plain one.
- Window: a `person` icon, and palette actions honor `when` (an action is offered only when
  `when({})` is true).
- A folder sent to a runtime elsewhere becomes a project there (ADR 0030):
  `ark_ui::remote::send_folder(client, &Folder::read(dir)?, name, go_on)` sends it in parts
  (`upload_parts`, `upload_project`, `upload_cancel`); the project appears only when every file
  arrived. `ark.action({ where: "project" })` adds an app's item to a project's menu.
- The window opens a runtime elsewhere and comes back (ADR 0029): `ark_ui::remote::visit(url,
  token)` from an app's handler loads that runtime's page in place of the app's own, and
  `ark.openLocal()` on that page goes back. `ark.sidebar({ id, render })` adds a row above the
  projects. The window's commands answer only the app's own page.
- Secrets in a file for servers (ADR 0028): `ark_connect::FileSecrets::in_store(root)` keeps
  account tokens and saved API keys in `<store>/secrets.enc` (AES-256-GCM, mode 0600), with the
  key from `ARK_SECRETS_KEY` or `<store>/secrets.key`; `ArkBuilder::secrets(..)` uses it instead
  of the OS keychain. `writing-agent --serve --tokens` uses it, so saved keys survive a restart
  ([`REMOTE.md`](docs/REMOTE.md#saved-keys-and-connections)).
- Named OpenAI-compatible endpoints (ADR 0027): `ark_model::endpoints::register(Endpoint::new(name,
  url))` or `ARK_ENDPOINTS=name=url`; `name:model` reaches it with its own key (`<NAME>_API_KEY`
  or the app's key store) and its models from `GET /models` in `catalog()`
  (`BackendKind::Endpoint`). Built-in provider names are refused, so `router:claude` is never
  Claude Code ([`OPERATIONS.md`](docs/OPERATIONS.md#named-endpoints)).
- Out of credit (ADR 0027): an HTTP 402 from a provider is `ProviderError::OutOfCredit`, not
  retried; the turn stops with `AgentError::OutOfCredit` and the provider's words;
  `TurnEnd::out_of_credit()` and `turn_finished.out_of_credit` give the hint both front-ends show.
- `ark-bench` workload `files-edit`: a computed column and a total row in a real spreadsheet,
  checked on the saved file.
- The Browser (ADR 0015): the new crate `ark-browser` drives the installed Chrome, Edge or Brave
  over the DevTools protocol; `browser.open`, `read`, `back`, `click`, `type`; a fresh context
  per agent, and a signed-in browser one agent uses at a time. `ArkBuilder::browser(true)` or
  `browser_with(service)` ([`EXTENSION.md`](docs/EXTENSION.md#the-browser)).
- `agents.create` takes `capabilities` (`["web", "browser"]`): the agent gets only those tool
  namespaces; its description lists them (`Orchestrator::capabilities`).
- `ark_runtime::per_agent::{PerAgent, Owner}`: tools each agent gets its own of (the web on its
  model, the browser). `Ark::per_agent`, `Ark::agent_tools`; `ark_runtime::web::Web`.
- `examples/writing-agent` turns the browser and the computer on.
- Work that runs without the window (ADR 0017):
  - **Several live chats**: `Interaction::siblings()` opens another live conversation of the same
    app. In the window, moving to another chat while a turn runs leaves it working. Its questions
    wait in the tray with the chat's name, and the sidebar marks working chats.
  - **Protocol, additive**: turn events and approvals carry `session` (approvals also `chat`);
    new `working` method and event; `steer`, `cancel` and `transcript` take an optional `session`.
  - **Lists while a turn runs**: `ark_interact::interaction::{list_projects, list_sessions,
    project_sessions, transcript}` read the store without the interaction, so `sessions`,
    `projects` and `transcript` answer while turns run.
- Schedules (ADR 0017):
  - **API**: `Orchestrator::schedule(agent, TaskSpec, When)`, `schedules`, `unschedule`,
    `tick_schedules`, `spawn_schedule_timer`.
  - **When**: every N minutes, or at a local time on given days, or once.
  - **Runs**: each run is an ordinary task (`TaskRecord::schedule`). `Graph::schedules`,
    `EventKind::{Scheduled, Unscheduled}`, `ark_orchestrate::scheduled_projects`.
  - **Asked in words and on the command line**: the chat's `agents.schedule` and
    `agents.unschedule`; `/schedules` and `/schedules stop <id>`; `/agents` lists them;
    `Control::Unschedule` ([`ORCHESTRATION.md`](docs/ORCHESTRATION.md#schedules)).
- The Computer (ADR 0018): the new crate `ark-computer`. Agents use the apps on this computer
  through macOS's accessibility interface: `computer.apps`, `look`, `open`, `click`, `type`,
  `keys`. One agent holds the screen at a time, every action but `apps` asks, actions wait for
  the person to pause, and the person can take the screen back. `ArkBuilder::computer(true)` or
  `computer_with(service)`; `Ark::computer()`. Window: "‹who› is using your computer · Stop"
  (`computer` event, `computer_stop`). A "Computer" capability in the catalog.
- `impl Clone for Ark` (a handle on the same agent); `Ark::follow_current_model`.
- `ActionDescriptor::ask_each_time()`: every call asks, Allow once or Deny; no grant covers it
  and no answer is remembered (`ApprovalRequest::ask_each_time`). The Computer's actions use it.
- Window: Settings › Appearance (Light, Dark, System), kept per window; `ArkUi::theme(Theme)` sets
  the default. Dark is `:root[data-theme="dark"]` now, not a media query
  ([`UI.md`](docs/UI.md#what-the-workspace-shows)).
- Window: `ark.modelPicker({ label, render })` lets an app draw the composer's model menu and
  ask for a model before the first message; `ark.onSubmit(fn)` sees each message first and
  can take it; `ark.setting({ advanced: true })` lists a section under Advanced.
- A runtime other machines connect to (ADR 0016): `ArkUi::access(Access::Tokens(..))` serves
  with saved access tokens (`ark_ui::access::Tokens`: named, hashed in `<store>/access.json`,
  revocable without a restart) and accepts any `Host`, for a TLS proxy in front.
  `ark_ui::remote::Client` calls a served runtime; `remote::run_window` opens a native window
  on one. `GET /health` answers without a token ([`REMOTE.md`](docs/REMOTE.md)).
- Window: `ark.isLocal()` and `<html data-runtime="local|remote">`: whether the runtime runs on
  the computer showing the page, so apps offer machine-bound actions only there.

- Files (ADR 0019): the new crate `ark-files`. The project folder's files as text, spreadsheets
  (`.xlsx`, `.xlsm`, `.csv`, `.tsv`) and documents (`.docx`) that agents and people read and
  change in place, keeping what an edit does not touch (styles, formulas, other sheets, a
  paragraph's look). Tools `files.list`, `read`, `search`, `write`, `edit` with `ArkBuilder::files(true)`;
  one `Edit` list for the agent and the window; a `version` that refuses an edit based on an old
  read; formulas computed after each edit. `Format` is the seam for another format
  ([`EXTENSION.md`](docs/EXTENSION.md#files)).
- Window: a file opens in a tab of its own: a spreadsheet as a grid (formula bar, sheets,
  keyboard, copy and paste), a document as a page (headings, lists, tables, bold and italic,
  Markdown shortcuts), text in the editor. Changes save themselves, and the tab follows changes
  the agent or another app makes. A turn that wrote files ends with a card per file. "Open in its
  app" when the runtime is local. Protocol: `file_read` answers the file's kind and shape,
  `file_edit`, `file_version`, `file_open`; `file_write` takes `base`; `hello` carries
  `file_kinds` ([`UI.md`](docs/UI.md#files)).
- `HarnessEvent::ToolResult.files` and `Activity.files`: the project files a call wrote.
  `ProducedArtifact::at(path)` marks an artifact as a snapshot of a project file
  (`Artifact.meta.path`, `ArtifactSummary.path`).
- `examples/writing-agent` turns Files on: chapters that arrive as Word files.
- Run code (ADR 0020): the new crate `ark-exec`. `code.run {code, runtime?, timeout?}` runs a
  program in the project folder inside a sandbox: it reads the system, its environment and the
  project, writes only the project (not `.git` and other tool folders), and has no network.
  Python 3.12 with pandas, numpy, matplotlib and openpyxl first, kept with `uv` under the store.
  Every file a run writes comes back as an artifact with its path. Contained on macOS
  (`sandbox-exec`), Linux (bubblewrap) and Windows (an AppContainer; the program calls
  `ark_exec::entry()` first in `main`), ADR 0032; offered nowhere else. `ArkBuilder::code(true)`
  or `code_with(Exec)`; a `Runtime` trait for other environments; a "Run code" capability
  ([`EXTENSION.md`](docs/EXTENSION.md#run-code)).
- Kinds (ADR 0021): `ark_files::Kind` gains `markdown`, `pdf` and `image`, each from a format.
  PDFs read as their text, a heading per page (read only); images read as their type and size.
  `ark_files::Kinds`, a sniffer the SDK registers, gives a stored copy of a file the same kind;
  `artifact_kind`, `shown()` and `media()` are the rest of the one decision.
- Window: images and PDFs open as themselves, Markdown as a page that shows the project's images
  it links to (Edit for the text). A stored copy of a project file opens as the file. Protocol:
  `file_media {path}` (a PDF or an image as base64); `hello` carries `kind_nouns`.
- `ark_tools::subprocess::{Spec, run_spec, timeout_note}`: a process with its folder, its own
  environment, a timeout and a bounded tail of its output.
- `examples/writing-agent` turns Run code on.
- Run code's Python has the data tools an analyst's work needs: duckdb (SQL over CSV, Parquet,
  data frames, and `.xlsx` through its `excel` extension, installed at setup since a run has no
  network), python-calamine (`pd.read_excel(engine="calamine")`: fast, and reads legacy `.xls`,
  `.xlsb` and `.ods`), xlsxwriter and pyarrow. The first setup downloads about 100 MB more.
- Files reads large spreadsheets in part: a read keeps the first `ark_files::READ_CELLS` cells
  of each sheet and counts the rest (`Sheet.unread`); the agent's listing says which rows were
  not read and that a program reads all of them. An edit still sees every row. A workbook read
  without a sheet shows the start of every sheet. A 300,000-row workbook reads in 200 MB instead
  of 1.3 GB.
- Skills that come with a tool (ADR 0033): `Runtime::skills`, `Exec::write_skills`,
  `Engine::add_skills`, `SkillScope::Builtin`, found after the project's and the store's skills.
  Run code's Python comes with `data-analysis`: how to inspect sources, check joins, keep
  originals, reconcile totals and answer in plain words ([`DATA.md`](docs/DATA.md)).
- `ark-bench` workload `workbooks`: two workbooks and a CSV (54,000 rows, a title row, total
  rows, duplicates, amounts as text, messy IDs), a summary workbook, a chart and three findings,
  checked on the saved files; the sources must not change.
- `code.run` takes an optional `purpose`, one sentence in the person's words. The approval and
  the activity row lead with it; the program follows in the approval.
- Window: a sheet longer or wider than the grid shows says so under the grid ("Showing the first
  5,000 of 300,001 rows"), and has no empty rows to type into past the part shown, which would
  have been the file's own rows.

- The project's board (ADR 0023): work items the person and the agents share, kept per project
  across chats and restarts. `Orchestrator::{board, work_item, add_work, change_work,
  remove_work}`; `WorkItem`, `WorkState`, `WorkChange`; `TaskSpec::item` links a task, and the
  item follows it (In progress with its agent as owner, Done when it finishes, blocked when it
  fails). Two agents never take one item. The chat plans with `Orchestrator::board_tools`
  (`board.list`, `board.add`, `board.update`) and links a hand-off with `agents.delegate {item}`;
  a worker's brief carries its item. `/board` (`add`, `done`, `move`, `give`, `remove`); window
  methods `board`, `board_add`, `board_change`, `board_remove`, `board_give`; a Board tab beside
  Chat in every app that runs agents ([`ORCHESTRATION.md`](docs/ORCHESTRATION.md#the-board)).
- The terminal on a runtime elsewhere (ADR 0024): `app --connect URL` with `--token` or
  `ARK_TOKEN` opens the shell (or a `-p` run) on the same app served with saved tokens. It
  follows a turn already running there, asks the runtime's questions in its menu and closes
  them when a window answers ([`CLI.md`](docs/CLI.md#on-another-machine)).
- `ark_interact::remote::Client` (moved from `ark-ui`, which re-exports it) has `events()`: the
  runtime's numbered events as a channel. `ark_interact::inspect::recap` builds the "where the
  chat left off" report from a `SessionSummary` and transcript turns.

- Native window: `ArkUi::keep_running(true)`: closing the window hides it and the app keeps
  running, with Open and Quit in the menu bar (the tray on Windows and Linux); the Dock icon
  opens it again. A question that waits while the window is not in front shows a system
  notification (`tauri-plugin-notification`, part of the `desktop` feature).

- API keys in ARK's secret store, so apps stop keeping their own: `ark_connect::ApiKeys`
  (`Connections::api_keys`) checks a key with the provider, keeps it in the OS keychain and
  lists which providers have one in `<store>/api-keys.json` (names only). `ark_model::keys`
  reads the environment first, then the saved key (`set_source`, installed by `ArkBuilder` with
  connections on). Window methods `api_keys`, `api_key_save {provider, key}`,
  `api_key_forget {provider}`; the model menu offers a key field for an API without one.

- `ArkUi::serve` with saved tokens refuses an app that lets agents use the computer: a runtime
  other machines connect to never offers this machine's screen.

- Benchmark: the `board` workload (planning on the board across a restart and a new chat);
  `Workload::chat` runs a team's turns through the person's conversation, and
  `TurnSpec::new_chat_after_restart` starts a new chat ([`bench/RESULTS.md`](bench/RESULTS.md)).
- Work reports into the chat that asked for it (ADR 0025): a scheduled run's result is posted in
  that chat (`ark_runtime::session::{post, posts, Post}`), shown under its last turn (live in
  the window, `post` event; in the terminal's recap) and carried by the chat's next turn (the
  newest 3, clipped). `TaskSpec::report_to`, `TaskRecord::report_to`,
  `Orchestrator::chat_tools_for(session)`, `EventKind::Posted`, `TranscriptTurn::posts`
  ([`ORCHESTRATION.md`](docs/ORCHESTRATION.md#schedules)).

- Benchmark: the `analysis` workload (a chart and a report from a CSV with Run code, checked on
  the saved files; simulated with a stand-in program) ([`bench/RESULTS.md`](bench/RESULTS.md)).

### Changed
- A state-changing call that already ran twice in a task with the same input is not run, or asked
  about, a third time; the model is told to change the input (`harness::MAX_IDENTICAL_CHANGES`).
  On `analysis`, qwen3:4b: same success, 35% fewer tokens.
- Runtime: an answer that ends by announcing work it has not done ("I will now call
  files.edit...") while tools are offered is sent back once, saying nothing ran. On qwen3:4b
  `files-edit` no run stops after reading any more; seven other workloads 20/21 to 21/21 at the
  same cost per success (`bench/RESULTS.md`).
- Preloading actions counts a file the task names (`ventas.xlsx`) and the file open in the
  window: actions whose summary names that extension load first, whatever the task's language.
- Window: the Files panel opens a file in its own tab instead of in the panel.
- Window: an approval that shows what the call would write hides the action's general summary.
- With `App::orchestrate`, the orchestrators are shared by all of an app's conversations.
  Each one's approval gate keeps "Always in this project" grants in its own project; before,
  one gate followed whichever project the chat had open. When the app opens, projects with
  schedules get their orchestrator.
- Window: `new_session`, `open_session`, `switch_project`, `open_project` and `add_project` no
  longer refuse while a turn runs: the running chat goes on in the background.
- Approval lines for ARK's own tools are in words ("Scheduling digest to … every day at 18:00",
  "Clicking “Send” in Mail") rather than the call's preview.
- The HTTP server takes the token from `Authorization: Bearer` as well, and drops a client
  that takes over 30 s to send its request.
- Window: Settings lists Skills and Tool servers under Advanced; shorter copy in Capabilities,
  Permissions, Skills and Tool servers. Welcome suggestions go through the same checks as a
  typed message.
- Window: the send button stays at the right of the composer when the context meter is hidden.
- Window: an approval shows its sentence, then what it would write in a separate block; the
  card carries `data-action`. A finished work block reads "Reading notes and 2 more steps"
  instead of "Used 3 tools". Capabilities that cannot be used say "Not available yet" (why, on
  hover) instead of an error. "Choose a folder…" and "New project…" in the sidebar's + menu.
- Protocol: `send` takes `ask` (default true). With `false` the turn's state-changing actions are
  refused instead of asked, as in a local `-p` run.

### Fixed
- `claude:` models: a tool call's arguments are a JSON object in Claude Code's reply schema
  (`CLAUDE_REPLY_SCHEMA`), not a JSON string. Haiku could not escape a program inside a string
  inside JSON, and every Run code call failed (`error_max_structured_output_retries`). Codex
  keeps the string (`REPLY_SCHEMA`), which OpenAI's structured outputs need. A Claude Code
  error with no text names its subtype.
- `ark_connect::oauth::revoke` sends the `client_id` (RFC 7009): providers that require it for a
  public client revoked nothing.
- A saved OpenAI or Anthropic key is checked where the model calls go: `OPENAI_BASE_URL` or
  `ANTHROPIC_BASE_URL` when set, not always the vendor's API.
- `ConnectError::Secrets` reads "secret store: ..." instead of "keychain: ...", since the store
  may be a file.
- An app whose model is an OpenAI-compatible model called `claude` no longer gives its agents
  Claude Code sessions: the orchestrator decides "subscription" from the provider, not the
  model id.
- Files: `files.list` on a file shows that file, and on a missing path says nothing has that
  name (it said "(empty folder)", and a small model concluded the file did not exist);
  `files.search` also matches file and folder names. Benchmark: `files-edit`, `bench/RESULTS.md`.
- Window: a question still waiting after a reload keeps all its answers ("Always in this
  project" was missing).
- `subprocess::run_timeout` no longer hangs until its timeout when a tool prints more than a
  pipe holds (about 64 KB): output is read while the tool runs.
- The `present` tool no longer offers views the window cannot show (`diff`, `structured`,
  `split`).
- An answer that says a file was saved when no call of the turn wrote it is no longer taken as
  the final answer: the model is told once which files were written, and saves the file or
  corrects the answer. A claim is a file name in a sentence that says it was saved, written or
  created (English only); a file counts as written when a call wrote it or it changed in the
  project folder during the turn. On the new `claimed-files` workload, qwen3:4b said a report was
  saved that its run never wrote in 5 of 5 runs, and in 0 of 5 with the check.
- `-p` without `--yes` denies questions that still reach it (an agent's, a connection's)
  instead of allowing each once.

### Breaking
- `ProducedArtifact` has a new field, `before`; build it with `text`, `bytes` or `before`
  instead of a struct literal.
- `HarnessEvent::ToolResult` and `ark_interact::Activity` have a new field, `before`: a struct
  literal of either needs `before: Vec::new()` (a pattern with `..` needs nothing).
- `SkillScope` has a new variant, `Builtin` (a skill that comes with a tool): a `match` on it
  needs an arm.
- `ConnectError` has a new variant, `Refused`: a 4xx answer from a token endpoint (an expired
  or revoked grant). A `match` on `ConnectError` needs an arm for it.
- `ProviderError::OutOfCredit` and `AgentError::OutOfCredit` are new variants: an exhaustive
  `match` adds an arm. `BackendKind::Endpoint` likewise. `OpenAi::describe` uses its name
  (`openai` unless `.named(..)`).
- `HarnessEvent::ToolResult` has a new field, `files`, and `ProducedArtifact` one, `path`: a
  struct literal needs `files: Vec::new()` / `path: None` (or `ProducedArtifact::text`/`bytes`).
  `Activity` and `ArtifactSummary` have one more field each.
- `ark_sdk::offered_tools(engine, per_agent)` takes the per-agent sources
  (`Ark::per_agent()`) instead of the web adapter. `Ark::web_adapter` and `Ark::web_tools` are
  gone: use `Ark::per_agent` and `Ark::agent_tools(&Owner { name, model })`.
- Window: an app stylesheet that set dark colours inside `@media (prefers-color-scheme: dark)`
  now loses to ARK's `:root[data-theme="dark"]`. Put them under `:root[data-theme="dark"]`, and
  give light overrides `:root, :root[data-theme]` so they win in both themes.
- `ActionDescriptor` and `ApprovalRequest` have a new public field, `ask_each_time`: a struct
  literal needs `ask_each_time: false` (or build the descriptor with `ActionDescriptor::new`).
- `ark_orchestrate::EventKind` has two new variants (`Scheduled`, `Unscheduled`): an exhaustive
  `match` needs an arm or `_`. `Graph` and `TaskNode` have a new field each (`schedules`,
  `schedule`): add it where you build them; deserializing fills a default.
- `ark_orchestrate::EventKind` has two more variants (`Work`, `WorkRemoved`), and `TaskSpec`,
  `TaskRecord` and `Brief` a new field each (`item`): a struct literal needs `item: None`, or
  build with `TaskSpec::new`. `Control` and `ControlOutcome` in `ark-interact` have new variants.
- `ark_cli::ArkArgs` has `connect` and `token`, and `ark_cli::Welcome` has `runtime`: a struct
  literal needs them (`..Default::default()` for `ArkArgs`).
- `TaskSpec`, `TaskRecord`, `SessionRecord` and `TranscriptTurn` have one more field each
  (`report_to`, `report_to`, `posts_read`, `posts`), and `EventKind` the variant `Posted`: a
  struct literal or an exhaustive match needs it.

## [0.9.0] - 2026-10-04

Claude Code and Codex are first-class: their CLI keeps running per session and the answer
streams; cancel drops the vendor session; usage limits say when they reset, and the usage the
CLIs report is available to apps. Agents follow the session's model. Capabilities that need
an account (Mail, Calendar on Google), asked for mid-task, and approvals that last in a
project.

### Added
- Claude Code and Codex sessions keep their CLI running between calls and stream the answer
  while it is written (ADR 0012): Claude Code over streaming JSON, Codex as `codex app-server`
  (`codex exec` when the app server cannot start). Live on `claude:haiku`: time to first token
  -59%, p95 turn time -52%; a 4-call Codex task 98 s to 39 s (`bench/RESULTS.md`).
- `SessionBackend::open_streaming` and `resume_streaming` (default: the whole text at the
  end); `ManagedSession` and `CliModel` stream through `Provider::complete_streaming`.
- Subscription usage: `ark_model::subscription_usage()` and `usage_of(account)` return what
  Claude Code and Codex last reported about the account's limits (`SubscriptionUsage`: plan,
  windows with whole-percent use and reset time, `limited`). Claude Code reports it with each
  reply, Codex when its app server starts and during turns. `CapacitySnapshot::usage` carries
  it, so the orchestration graph's `accounts` show it; the window protocol's method
  `subscription_usage` returns the list.
- `CurrentModel` (`ark_runtime::router`), `Ark::current_model`, `ModelsBuilder::follow`,
  `ModelResolver::default_version`: the session's model as a shared handle (ADR 0014).
- Capabilities that need an account, owned by ARK (ADR 0011): the new crate `ark-connect`
  (`Catalog`, `Capability`, `Provider`, `Connections`, `Account`, `CapabilityStatus`,
  `Secrets`, `Keychain`, `MemorySecrets`, `oauth`), on with `ArkBuilder::connections(true)` or
  `connections_with(..)`. First capabilities: Mail (send) and Calendar (see and add events) on
  a Google account; sign-in in the person's browser (OAuth with PKCE), tokens in the OS
  keychain, revoked on disconnect ([`EXTENSION.md`](docs/EXTENSION.md#capabilities-and-accounts),
  [`GOOGLE.md`](docs/GOOGLE.md)).
- `/connect [capability]` and `/disconnect <capability>` in both front-ends
  (`ark_interact::capabilities`), and Settings › Capabilities in the window (methods
  `capabilities`, `capability_connect`, `capability_disconnect`).
- `ark_tools::ToolSource`, `Engine::add_source`, `Engine::has_sources`: tools that come and
  go while ARK runs; MCP plugins are one.
- `Ark::tools()` and `ark_sdk::offered_tools`: every action a turn would be offered.
- An agent asks for a capability mid-task (ADR 0013): with connections on, the first message
  lists what can be added, `acquire_capability {need}` asks the person ("ARK needs this to
  finish: Connect Google…", Connect or Not now), the sign-in runs in their browser, and the
  capability's tools join the running turn. `AcquisitionProvider::active` and `offer`,
  `AcquisitionResult::tools` and `with_tools`, `Connections::capability_for`, `connectable`,
  `tools_of`, `ark_interact::capabilities::Connector`, `App::sign_in`.
- `ark_interact::approval_choices` (and `Choice`): the answers both front-ends offer for an
  approval; the window's `approval` event carries them as `choices`.
- "Always in this project": `ApprovalDecision::AllowAlways`, kept per project in the store
  (`ApprovalGate::keep_in`, `ark_runtime::{lasting_grants, remove_lasting_grant,
  LastingGrant}`); `/grants` in both front-ends, Settings › Permissions in the window
  (methods `grants`, `grant_remove`) ([`EXTENSION.md`](docs/EXTENSION.md#approvals-that-last)).

### Changed
- `acquire_capability` takes `need`, as the runtime always read it (its schema said `id`).
- An approval's headline is "ARK wants to:" or "<agent> wants to:" (was "The agent wants to
  run:", with the task id).
- Each acquisition is its own approval class (`acquire_capability:<need>`).
- A cancelled call on a managed vendor session drops that session (`OpenReason::Cancelled`):
  the next call opens a new one instead of resuming a reply cut off halfway.
- A subscription's usage limit fails the turn at once and says when it resets; it was
  retried like an overloaded service.

### Breaking
- `OpenReason` has a new variant, `Cancelled`; a `match` over it needs an arm.
- `AcquisitionResult` has a new field, `tools`; build it with `info`, `changed` or
  `with_tools` instead of a struct literal.
- `ApprovalDecision` has a new variant, `AllowAlways`; a `match` over it needs an arm.
- `CapacitySnapshot` has a new field, `usage`; a struct literal of it needs `usage: None`.

### Fixed
- `/tools` and the window's `tools` list the web's actions.
- A script added under `ark-ui/web/js/` is embedded in the next build; before, the build
  script missed it and the page that imports it stayed blank.
- An empty command line (`command` in the window protocol) no longer starts an empty turn.
- An agent without a model of its own runs on the session's model as it is now, not as it was
  when the orchestrator was built (ADR 0014). Before, a person who switched the conversation
  to a local model still got new agents on the first model (for example Claude Sonnet). An
  agent created before the switch moves to the new model from its next task.

## [0.8.1] - 2026-10-04

A patch from v0.8.0 only (branch `release/0.8.1`), for ARK Desktop 0.1.0. The same fixes are in
v0.9.0.

### Changed
- The approval headline says who asks, not which task: "writer wants to:" ("ARK wants to:"
  for the conversation). The line under it says what (an app's `ToolLabeler::approval`).

### Fixed
- The window's markdown no longer throws on a table that is still streaming in.
- The Files panel no longer prints `null` under a folder's list.

## [0.8.0] - 2026-10-03

Capabilities an app turns on without code of its own: MCP servers added while ARK runs,
skills read on demand, and web search and fetch on every model with no key. The public API is
trimmed of items no caller used.

### Added
- MCP plugins change while the engine runs: `Engine::plugins()` (`ark_runtime::Plugins`:
  `list`, `remove`, `restart`), `Engine::add_plugin(PluginSpec)`, `PluginStatus`. A change
  applies from the next turn or task ([`EXTENSION.md`](docs/EXTENSION.md#mcp-plugins)).
- `/plugin` in both front-ends (list, add, remove, restart; `add` takes words or a pasted
  `mcpServers` snippet), `ark_interact::plugins`, and Settings › Tool servers in the window
  (methods `plugins`, `plugin_add`, `plugin_remove`, `plugin_restart`).
- `Engine::tools()` (the registry plus the plugins connected now) and `Engine::for_turn()`.
- `McpAdapter::connect_within`, `McpClient::connect_within`, `McpAdapter::running`.
- Skills (Agent Skills format) from the project folder's `.agents/skills` and the store's
  `skills`: a one-line-per-skill catalog in the first message (reported as `skills` in
  `ContextReport`), `skills.read` and `skills.file` on demand (`ark_runtime::skills`,
  `Harness::skills`, [ADR 0009](docs/adr/0009-skills-load-on-demand.md)). The conversation
  reads all of them (`ArkBuilder::skills`, `Ark::skills`, `Ark::skill_names`); an agent reads
  the ones `AgentSpec::skills` names; `agents.create` takes `skills`.
- `/skills` in both front-ends (`ark_interact::skills`), Settings › Skills in the window (method
  `skills`; Add skills opens the Terminal with `npx skills add` typed).
- `ark-bench`: the `skill` workload. Live on qwen3:4b it passes 3 of 3 in 2 calls
  (`bench/RESULTS.md`).
- The web: `web.search` and `web.fetch` (`ark_tools::web`: `WebAdapter`, `SearchBackend`,
  `ResultPage`, `Wikipedia`, `distill`), on with `ArkBuilder::web(true)` or
  `ArkBuilder::web_with(adapter)`, offered to orchestrated agents whose `tools` allow them.
  Search uses the model's own search first, then keyless sources; no key
  ([`EXTENSION.md`](docs/EXTENSION.md#the-web), [ADR 0010](docs/adr/0010-web-search-without-a-key.md)).
- `Provider::web_search` and `SessionBackend::web_search` (default: none), with `WebHit`;
  implemented by `Anthropic` (the `web_search` server tool) and `CliModel` (Claude Code
  `WebSearch`, Codex `--search`); `ManagedSession` and `Limited` forward it.
- `ark_runtime::web` (`ModelSearch`, `on_model`, `distiller`), `Ark::web_adapter`,
  `Ark::web_tools`.
- `ark-bench`: the `web` workload (a loopback page and a fixed search engine); the naive loop
  is offered the web too when the app turns it on.
- `examples/writing-agent` turns the web on.

### Changed
- An MCP server has 20 s to answer `initialize` and `tools/list` (`mcp::HANDSHAKE_TIMEOUT`);
  a server whose connection is dropped is stopped.
- A plugin that fails to start at open stays listed with its error instead of being skipped.

### Breaking
- `Engine::registry` no longer holds MCP plugins. Code that read or ran plugin actions
  through it uses `Engine::tools()`; `Engine::run_action` and the SDK's turns already do.
- `AgentError` has a new variant, `Plugin(String)`; a `match` over it needs an arm.
- Removed public items no caller used. Use instead: `Interaction::ark_mut` (none; configure
  through `App` before open), `Harness::budget_tokens` (the budget given to
  `Harness::budget`), `budget::WorkingSet::round_count` (none), `Registry::from_adapters`
  (`Registry::new()` plus `with`/`register`), `PendingApproval::is_closed`
  (`PendingApproval::closed().await`), `Ark::ask_simple` (`Ark::ask`, or `ark_runtime::Agent`
  for the loop without a context budget).
- `ContextReport` and `AgentSpec` have a new field, `skills`; a struct literal without
  `..Default::default()` (or `AgentSpec::new`) needs it.

## [0.7.0] - 2026-10-03

The measured runtime, phases P0 to P2, and the window that apps build on: projects as folders,
a top bar with tabs, a Settings page, Terminal, Browser and Files, and a chat that creates
agents and hands them tasks. New runtime defaults: action preloading, the ready-to-call list,
small arrays shown whole, and the planner off. They were measured live on qwen3:4b
(`bench/RESULTS.md`, P2). They were not re-measured on Claude Haiku for this release; only
preloading has a partial Haiku run (P1). On qwen3:4b, `parallel` and `team` still cost about
1.4 times the naive loop per success.

### Added
- `/agent` and `/task` in both front-ends: add, pause, resume, end and message agents, hand
  out, cancel and retry tasks. `ark_interact::orchestration::{Control, ControlOutcome,
  control, parse_agent, parse_task}`; the window's methods run the same `control`.
- `ark-bench`: the naive baseline loop (`--harness naive`, `ark_bench::naive`), orchestrated
  workloads (`Workload::team`, `suite::TeamSpec`, the `team` workload), fault injection
  (`--faults`, `--seed`, `ark_bench::faults`), record and replay (`--record`, `--replay`,
  `ark_bench::cassette`), per-run traces (`--traces`). `RunRecord.faults_injected` and
  `.replay_diverged`. See `docs/BENCH.md`.
- Action preloading, off by default: `Harness::preload(n)` (`ARK_PRELOAD_ACTIONS=n`) loads up
  to `n` actions whose name or summary matches the task's words before the first step
  (`ark_runtime::catalog::matching_actions`), saving the `load_action` round trips. On by
  default only if live runs pay for it (ROADMAP, P2).
- `ark-bench` reports record runtime settings read from the environment (`env.*`).
- Window: projects are folders. The sidebar lists projects with their chats beneath, folds
  the ones not current, and renames a project or changes its folder from its menu.
  `Interaction::{open_project, add_project, rename_project, set_project_folder,
  project_sessions}`; page methods `open_project`, `add_project`, `rename_project`,
  `project_folder`, `sessions {project}`.
- Window: a top bar with tabs for app views and buttons for app panels marked `nav`, and a
  Settings page whose sections apps register with `ark.setting` (⌘,).
- The chat creates agents and hands them tasks: in an orchestrating app each turn gets
  `Orchestrator::chat_tools()` (`agents.create`, `agents.delegate`, `agents.wait`), so a
  person asks in words instead of filling a form. `TurnOptions::tools` and `TurnTools` add
  tools to one turn, offered directly; `Ark::context_preview_with` previews them. See
  `docs/ORCHESTRATION.md`, "From the chat", and ADR 0007.
- Window: `ark.compose(text)` puts text in the composer, unsent.
- Window: Terminal, Browser and Files in every app, opened from a + menu in the top bar
  (⌃`, ⌘T, ⌘P) or by the agent's `present` (`terminal`, `browser`, `file`). The terminal is
  the project's shell in its folder; Files browses, edits and saves files inside the folder.
  Page methods `term_open`, `term_write`, `term_resize`, `term_close`, `files`, `file_read`,
  `file_write`; event `term`. A `present` the window cannot show is a toast instead of nothing.
- Whole small results, off by default: `DistillerSet::whole_below(tokens)`
  (`ARK_WHOLE_RESULT_TOKENS`) passes a result no distiller recognizes whole when it is that
  small, instead of a summary the model may have to open with `get_evidence`.

### Changed
- Switching or opening a project returns to its most recent session instead of starting a
  new one (`Interaction::set_project`).
- Window: `nav` on `ark.view` and `ark.panel` puts the view in a top-bar tab and the panel
  in the top bar's + menu, not in the sidebar. "New session" is "New chat".
- **Action preloading is on by default** (2 actions; `ARK_PRELOAD_ACTIONS=0` turns it off).
  Measured live on qwen3:4b and Claude Haiku (`bench/RESULTS.md`, P1).
- Preloading drops a match from another adapter that scores below the best match minus 1
  (one coincidental word no longer brings in a distractor), and keeps an adapter's weaker
  matches next to its strong one (find, then list).
- **The first message names the loaded actions** under `== READY TO CALL ==`, one line each
  with its summary, so a model calls them instead of loading others from the index.
  `ARK_READY_LIST=0` turns it off. Counted under the capability index in `ContextReport`.
- **The generic summary shows a small array whole** (up to 400 characters of JSON) instead
  of its count, and says how to read the rows of an array it counts (`get_evidence` with
  `match`). `ARK_INLINE_ROWS=0` restores the count only. Results with a distiller are
  unchanged. Measured live on qwen3:4b (`bench/RESULTS.md`, P2).
- **The planner is off by default** (`Harness`, `ArkBuilder`, `Orchestrator` agents,
  `BenchSettings`). Its steps cost calls on small tasks and helped none: on qwen3:4b, 42/42
  without it against 41/42, 0.0181 against 0.0189 per success (`bench/RESULTS.md`, P2
  overhead; ADR 0008). `.planner(true)` or `AgentSpec::planner(true)` turns it back on;
  `ark-bench --planner` measures it.
- `ark-bench`'s simulated model reads raw JSON results as it reads ARK's cards, and finds
  the task in a plain transcript. ARK's simulated numbers do not move.
- `bench/baseline.json` has the `team` workload (14 workloads); `bench/baseline-naive.json`
  is the naive loop on the same suite.

### Breaking
- `ark_bench::BenchSettings` has `harness`, `faults`, `cassette` and `traces`: struct
  literals add them, or end with `..Default::default()`.

### Fixed
- A task waiting on a paused agent ends as soon as it is cancelled (it noticed within 250 ms).
- A Settings section an app registers while Settings is open joins its list.

## [0.6.0] - 2026-10-03

Several agents as compositions of a few primitives, run on subscriptions first.
Guide: `docs/ORCHESTRATION.md`. Example: `examples/agent-team`.

### Added
- **`ark-orchestrate`**: agents (`AgentSpec`: identity, instructions, model, allowed tools,
  delegation permissions, parent, budget), tasks (`TaskSpec`/`TaskRecord`: objective, state,
  owner, dependencies, artifacts, budget, attempts), delegation (`agents.delegate`,
  `agents.wait`), messages (`agents.message`), lifecycle (`spawn`, `run`, `suspend`,
  `resume`, `cancel`, `terminate`, `retry`), a scheduler that respects each account's
  capacity, budgets (steps, tokens, model calls, time, delegations), crash recovery, and an
  observable `Graph` with a stored event stream (`subscribe`, `events`, `task_trace`,
  `Graph::to_mermaid`). Each agent works in its own child project; a delegated agent gets a
  brief, never the delegator's conversation.
- **Managed vendor sessions** (`ark_model::session`): `SubscriptionRuntime` gives each lane
  (agent) its own `ManagedSession`, a `Provider` that resumes the vendor session with only
  what is new, monitors its context, rotates before `max_context_tokens`, rehydrates from
  ARK's budgeted request, recovers after a crash (`SessionLedger`), and releases idle
  sessions. `CliModel` implements `SessionBackend` (Claude `--session-id`/`--resume`, Codex
  `exec`/`exec resume`). Settings: `ARK_SESSION_REUSE`, `ARK_SESSION_MAX_CONTEXT`,
  `ARK_SESSION_MAX_TURNS`, `ARK_SESSION_IDLE_S`.
- **Capacity** (`ark_model::capacity`): `Capacity`, `Limiter` and `Limited` queue calls per
  account (default 2 at a time for `claude` and `codex`, 4 otherwise;
  `ARK_<ACCOUNT>_CONCURRENCY`) and cool down after a rate or usage limit.
- **Rule sets**: `ark_core::{RuleSet, Rule, RuleBinding, RuleScope, RuleRef}`, stored and
  versioned with `ark_runtime::RuleBook` (`Ark::rule_book`), bound to a project, agent or task,
  selected per task by `ark_runtime::rules::select`, looked up with the `rules` tool
  (`RulesTool`), and traced (`HarnessEvent::Rules`, `TaskRecord.rules`). Project bindings
  apply to single-agent turns too.
- **Diagrams**: `ark_tools::diagram` (`Diagram`, `DiagramKind`, `validate`; not in `ark-core`, which names no file format) and the opt-in
  `ark_tools::diagram::Diagrams` tool (`diagram.create`, `diagram.read`) store validated
  Mermaid diagrams as `diagram` artifacts, with versions.
- `Registry::filtered` and `ark_tools::action_matches`: a per-agent view of the tools.
  `Engine::scoped`: an engine over the same store with another registry.
- `Store` typed records (`put_record`, `get_record`, `list_records`, `delete_record`) and
  event streams (`append_event`, `list_events`, `last_event`).
- `Project.parent`; front-ends list only top-level projects.
- `ark_model::Message` and `ToolDef` derive `Serialize` and `Deserialize`.
- `ark_interact::orchestration` and `App::orchestrator`: `/agents` and `/agents <task>` in
  both front-ends. Workspace protocol methods `graph` and `task_trace`, events
  `{kind: "orch"}`.
- `examples/agent-team`: a coordinator and two readers on different subscriptions.
- Orchestration from a window: `App::orchestrate(f)` builds one orchestrator per project, the
  first time the interaction works there, with an approval gate that reaches the front-end.
  Workspace protocol methods `agents`, `spawn_agent`, `submit_task`, `cancel_task`,
  `retry_task`, `suspend_agent`, `resume_agent`, `terminate_agent`, `message_agent`, `task`
  (record and timeline), `orch_events` (stored events) and `tools`. They answer while a
  conversation turn runs. `orch` events carry the timeline `line` and, for an agent's runtime
  event, the `activity` the conversation would show. See `docs/UI.md`.
- `ApprovalRequest.agent` and `.task`: an orchestrated agent's question says who asks. The
  window shows it in a tray outside the conversation, and a turn's end no longer closes it.
- `ark.view({ id, title, icon, nav, render })`: an app view in the main column, in place of
  the conversation, listed in the sidebar (`ark.openView`, `ark.showConversation`).
- Labels for ARK's own tools: "Delegating to reader-a: …", "Messaging lead", "Drawing …".
- `Ark::session_router`, `Ark::subscriptions`, `ArkBuilder::subscriptions`: the runtime
  behind single-agent managed sessions (see Changed); its state is in the store
  (`ark_runtime::StoreLedger`). An orchestrator shares the `Ark`'s runtime and capacity.
- When ARK lets a vendor session go it deletes the vendor's file for that id
  (`SessionBackend::discard`; `ARK_SESSION_KEEP_FILES=1` keeps them).
- `SubscriptionRuntime::stored` and `ModelResolver::stored_session`: the graph shows an
  agent's stored session before it runs again after a restart.
- `examples/writing-agent`: `/review <n>`, an editor and two readers per manuscript project.
- Named profiles: `OrchestratorBuilder::profile(name, summary, profile)`,
  `Orchestrator::profiles`, `AgentSpec::profile_name` (stored, unlike `AgentSpec::profile`,
  so it survives a restart and a front-end can choose it), `AgentNode.profile`. Workspace
  protocol method `profiles`; `/agents` shows an agent's profile next to its model.
- The project folder in ARK: `Engine::set_workspace`, `Interaction::workspace` and
  `::set_workspace`, `/folder [path]` in both front-ends, workspace protocol method
  `workspace` (`{path}` sets it), `ProjectSummary.workspace`.
- Connection events reach app scripts in a browser: `ark:reconnecting`, `ark:disconnected`
  (with `retry()`), `ark:connected`.
- `ark.pickFolder({ title })`: the window's own folder picker (`undefined` in a browser,
  where the page cannot see the runtime's disk).
- `ark_interact::approval_title`: the approval headline both front-ends show. The terminal
  now names the agent and task that ask ("reader-a (task t-1a2b3c4d) wants to run:"); the
  window's `approval` events carry it as `title`.

### Changed
- **Single-agent turns on a subscription keep a vendor session.** With a `claude:` or
  `codex:` model, `Ark::ask_session` resumes one managed vendor session per conversation and
  sends only what is new (median uncached input 59.3k to 37.8k in `bench/RESULTS.md`).
  `ARK_SESSION_REUSE=0` restores stateless calls.
- A CLI rate or usage limit is `ProviderError::Overloaded` (was `Api`), so the harness
  retries it after the account's cool-down. Apps that matched `Api` for it match `Overloaded`.
- Headless `-p "/command"` runs the slash command and prints its report, instead of sending
  the text to the model.
- `ark-interact` depends on `ark-orchestrate` and re-exports it (`ark_interact::ark_orchestrate`).
- An orchestrated agent's project has no folder of its own: it works in its top-level
  project's folder, read when it runs, so changing the folder moves existing agents too.
- Claude Code is told its only tool is `StructuredOutput`. With the earlier wording Sonnet
  first tried to call a host tool by name and failed: one delegation took 4 internal turns
  and 3,049 input tokens, now 2 turns and 1,384 (measured with `claude -p`, 2 runs each).
- `ContextReport.rules` and a `Rules` row in `/context`.
- Results of `rules.*` and `diagram.*` reach the model in full instead of a generic summary.
- Claude Code gets ARK's tools as bullets ("use them only inside StructuredOutput"), not
  Markdown headings. With headings Haiku gave up a one-tool task 2 of 2 times; with bullets
  it made the right call in the minimum 2 turns. Sonnet is unchanged. Probes: `bench/protocol/`.
- A delegated result clipped in the delegator's evidence card says how to read the rest.
- Claude Code: a reply that took more than 2 internal turns and asked for no tool, while
  tools were offered, means the model gave up after refused native calls; ARK asks once more
  with a one-line correction, in the same vendor session. Seen once in 9 live runs with a Haiku reader.

### Fixed
- Agents created before the project folder was chosen kept working in the old folder.
- The window opens artifacts an orchestrated agent produced (they live in its child project).
- `ark.h` sets CSS custom properties given in `style` (`{ "--hue": 200 }`).

### Breaking
- `Engine.store` is an `Arc<Store>`. Method calls are unchanged; code that moved the store
  out of an engine takes a clone of the `Arc`.
- `HarnessEvent::Rules` is a new variant: add an arm where you match the enum exhaustively.
- `ContextReport` has a `rules` field and `Project` a `parent` field: struct literals add
  them (or use `..Default::default()` / `Project::new`).
- `ark_interact::trace::EntryKind::Rules` is a new variant.
- `ApprovalRequest` has `agent` and `task` fields: struct literals add `agent: None, task: None`.
- `ProjectSummary` has a `workspace` field: struct literals add `workspace: None`.
- `ark_sdk::prelude` exports `Rule`, `RuleSet`, `RuleBinding`, `RuleScope`, `RuleBook`,
  `RuleSelection`, `Diagram` and `DiagramKind`; a glob import of another module with the same
  names becomes ambiguous.

## [0.5.0] - 2026-10-01

### Added
- Subscription models through the vendor's official CLI: providers `claude` (Claude Code,
  Claude subscription) and `codex` (Codex, ChatGPT plan), e.g. `--model claude:sonnet`. ARK
  runs the tools; the CLI's own tools are off. See `docs/OPERATIONS.md#subscriptions`.
- `ark_model::catalog()`: the backends usable on this machine (signed-in subscriptions,
  Ollama models, APIs with a key) and the command that fixes one that is not ready.
- Workspace: the model menu lists those backends and switches with one click. New protocol
  method `models`. `/model` with no argument lists them too.

### Changed
- The model chip and `Summary.model` name the model only (`ModelRouter::label`), without
  "(single tier)". `/model` still shows the routing.
- Documentation reorganized for maintainers: `AGENTS.md` (rules and change lifecycle; `CLAUDE.md`
  imports it), `ROADMAP.md` at the root (state, limits, next), `docs/RELEASING.md` (release,
  app upgrade, rollback; replaces `docs/VERSIONING.md`), diagrams in `docs/ARCHITECTURE.md`,
  provider and development environment variables in `docs/OPERATIONS.md`.
  `docs/EXTRACTION_MAP.md` is folded into `docs/ARCHITECTURE.md` (Origin).
- Comments that described the extraction history now describe the code. No library change.

## [0.4.1] - 2026-10-01

### Fixed
- `scripts/ci.sh` ran the benchmark from `./target/release` even when `CARGO_TARGET_DIR`
  pointed elsewhere, so `scripts/verify-release.sh` (which honors `CARGO_TARGET_DIR`) failed
  at its last step. Found by verifying v0.4.0 from a fresh clone. No library change.

## [0.4.0] - 2026-10-01

Production hardening, measurement, and a cheaper harness. No new front-end surface.
Before/after numbers are in `bench/RESULTS.md`; the benchmark harness is in `docs/BENCH.md`
and running ARK in production in `docs/OPERATIONS.md`.

### Added
- **`ark-bench`**: a benchmark and eval harness for any ARK application. Workloads, a metered
  provider that records every request (size by part, measured usage, latency, first token,
  prefix shared with recent requests), a simulated model whose policy reacts to what each
  request contains, JSON reports and comparisons with regression gating, and a generic suite
  of 13 workloads. `bench/at-commit.sh` builds the current bench against an earlier commit.
- Provider resilience: `ProviderError::{Status, Overloaded, Malformed}` and
  `is_transient()`; `HttpConfig` (connect, idle and request timeouts, output cap);
  `RetryPolicy` (`ARK_MODEL_RETRIES`, default 2) used by the harness; `HarnessEvent::Retry`;
  `ModelCall.retries`, `Metrics.provider_retries`.
- `Reuse { Auto, Stable, Volatile }` on `ActionDescriptor` (`.reusable()`, `.volatile()`).
- `get_evidence` takes `match` and `offset`; `ark_runtime::evidence`.
- Context reports for every model call: `ContextReport.role`, `model_output`,
  `action_schemas`; the planner, critic and forced answer emit one.
- `Session::begin_turn` / `fail_turn`, `Turn.{incomplete, error, pid}`,
  `Engine::recover_interrupted`, `Invocation.pid`, `session::INTERRUPTED`.
- `ark_interact::logging` (`ARK_LOG`, file or stderr), `TraceMode` (`ARK_TRACE`),
  `TRACES_KEPT`; `Store::{project_stats, session_text_chars, prune_turn_traces,
  list_running_invocations}`.
- `TurnOptions::present`, `Harness::presentation`, `Interaction::set_presentation`,
  `Ark::context_preview_for`.
- `Usage::cache_write_tokens`, `Usage::uncached()`, `Metrics.cache_write_tokens`.
- Events: `HarnessEvent::Stall`, `ModelCall.first_token_ms`, `ToolResult.raw_tokens`.
- `scripts/ci.sh`, `scripts/check-version.sh`, `scripts/verify-release.sh`,
  `.github/workflows/ci.yml`.

### Breaking
- `ActionDescriptor::reusable: bool` is replaced by `reuse: Reuse`. `.reusable()` still
  marks an action `Stable`.
- `Selection` keeps `text` and `line`; `address`, `symbol` and `function` become named
  `marks` (flattened in JSON, so `{"address": "0x1000"}` still parses). Use
  `Selection::mark(name)`.
- The runtime tools offered by default are `load_action`, `get_evidence`, `search_evidence`
  and `record_state`. `list_actions` and `record_note` are no longer offered (still handled);
  `present` is offered only when the front-end can present.
- Request layout: the working state is the last message of each request instead of part of
  the system prompt; the system prompt is fixed for a turn and ordered instructions, index,
  project; loaded tools keep their load order; artifacts are listed as `art:<12 hex>`, at
  most 12. Default prompt text changed accordingly (a MINOR behavior change by our policy;
  in 0.x it ships with the breaking release).
- Anthropic requests carry `cache_control` breakpoints and merge consecutive messages of
  one role; the default output cap is 8192 (was 2048). Ollama sends `num_predict` (4096).
- New fields on `HarnessEvent::ModelCall` and `ToolResult`, new variants `Retry` and `Stall`:
  exhaustive matches need the new arms.
- The default activity labeler no longer treats `process`, `function` or `symbol` arguments
  as the subject.
- A tool call whose arguments are not valid JSON is `ProviderError::Malformed`, not `{}`.

### Fixed
- The stall guard counted every retrieval as progress, so a model repeating one search ran
  to the step limit.
- A read after a write in the same turn could be served the stale first result.
- A short artifact id and the full id were different calls to the dedup check.
- `get_evidence` could not reach rows past the first 40 of a large output.
- With no output cap and Ollama's context shifting, one degenerate reply ran for over 20
  minutes; there was no request or idle timeout on any provider.
- The common distiller summarized any `*.read` / `*.write` / `*.list` action as a file
  operation and dropped its fields (`notes.read` became "read  (0 lines)").
- The working-state digest showed the oldest six items of each section, not the newest.
- After every turn the workspace loaded the whole transcript and every stored tool output to
  count them; memory and turn time grew with each turn.
- A turn killed mid-run, or ending in an error, vanished from the transcript; a tool call
  killed mid-run left no record.
- The browser event stream reconnected every few seconds forever, and a reconnect during a
  turn could lose or repeat events.
- The HTTP server spun when `accept` kept failing.
- Warnings (for example a plugin that failed to load) went nowhere: no logger was installed.
- A reply that writes a tool call as text (`record_state({...})`) is no longer taken as the
  final answer: the model is told once that nothing ran and asked again. With the working
  state as the last message, qwen3:4b did this in 4 of 39 live runs.

## [0.3.0] - 2026-09-30

An interaction layer both front-ends share, and a graphical workspace on it.

### Added
- **`ark-interact`**: the layer between the runtime and any front-end. `App` (an app described
  once), `Interaction` (project, session, approvals, turns, views), `Command` / `Reply` /
  `Effect`, structured `Report`s (`Block`, `Table`, `Tone`, ...), `Labels` / `ToolLabeler` /
  `ActivityLabels`, `Activity`, `trace` (`TurnRecord`, `TurnRecorder`, `interpret`), `inspect`
  (context, usage with optional `Pricing` / `ARK_PRICE`, working state, sessions) and
  `ChannelApprover`. Generic commands gain `/rename` and `/project`.
- **`ark-ui`**: the graphical workspace. `ArkUi` builder (accent, logo, welcome, suggestions,
  assets, scripts, stylesheets, handlers), `Host` with a `call` protocol and an event stream,
  a Tauri window (feature `desktop`, default) and a loopback HTTP server with a per-run token.
  The page's extension API (`window.ark`): panels, artifact viewers, tool renderers, actions,
  status lines, events. See `docs/UI.md`.
- Steering: `Steering`, `TurnOptions::steer`, `Harness::steering`. A message sent while the
  agent works reaches the model at the next step; one that arrives too late is returned in
  `HarnessOutcome::steered` for the next turn. `HarnessEvent::Steered`, `Turn::steered`.
- `Usage::cached_tokens` (Anthropic cache reads, OpenAI `cached_tokens`), summed in `Metrics`
  and `SessionUsage`.
- `HarnessEvent::ToolResult::context_tokens` and `ContextReport::retrieved`: what a result
  added to the context, and evidence fetched on demand shown apart from tool results.
- Sessions: `Session::set_title`, `rename`, `transcript_page`; a `turn_traces` store table
  (`put_turn_trace`, `get_turn_trace`), so a stored turn replays as it ran.
- `Ark::shared_engine()`: read-only store access while a turn runs.
- `HarnessEvent` is `Deserialize`.
- Page tests (`crates/ark-ui/tests/page.rs`): the real page in WebKit, opt-in with
  `ARK_UI_PAGE_TESTS=1`. A neutrality test covering the runtime's code as well as the
  front-end layers.

### Breaking
- `ark-cli` renders `ark-interact` and holds no command logic. `ArkCli::new(app)` replaces
  `ArkCli::builder()`; the agent, commands, labels and status move to `App`. `Command::run`
  takes `&mut Interaction` and returns `Reply`; `CommandContext` and `CommandOutcome` are gone.
  `ToolRenderer` is now `ark_interact::ToolLabeler`.
- `ark-cli` keys: while the agent works, enter steers and tab queues (enter used to queue).
- New `HarnessEvent` variant `Steered`; `ToolResult` gains `context_tokens`;
  `HarnessOutcome` gains `steered`. Exhaustive matches need the new arm.
- `Ark` holds its `Engine` in an `Arc`.

### Fixed
- A failed result is no longer reused. Dedup served a failed call's card back as "reused
  prior evidence", so a retry after a timeout or a missing tool never ran again. Only a
  successful result is reused; a failure runs again, and repeating the same failure counts as
  no progress for the stall guard.
- Cancelling a tool call kills its whole process group. Before, a shell's children (for
  example the `sleep` in `sh -c "sleep 30"`) outlived the cancelled call.
- An `artifact` argument given as a short id (`art:dc3b0c93c72c`), a prefix or a name is
  resolved to the full id before the adapter sees it. The model is shown short ids, so its
  calls used to fail with "artifact not found".
- `-p` printed the answer twice after the model narrated before a tool call (the final
  answer was compared with all streamed text, narration included).
- Default activity labels no longer carry domain verbs; "Showing show …" and "1 model calls"
  wording.

## [0.2.0] - 2026-09-30

The interactive shell, and what the runtime needed to support it.

### Added
- **`ark-cli`**: the interactive terminal shell every ARK app opens into. Streaming output,
  semantic activity lines, an approval menu, esc/ctrl+c to stop, input history and multiline
  editing (reedline), sessions and `--resume`, generic slash commands (`/context`, `/usage`,
  `/trace`, `/state`, `/compact`, ...), consumer commands, labels, themes and status sections,
  and a headless `-p` / `--json` / `--events` mode. See `docs/CLI.md`.
- `Provider::complete_streaming` with native streaming for Anthropic, OpenAI and Ollama. The
  default implementation emits the full text once, so every provider works.
- `ApprovalGate`, `Approver`, `ApprovalRequest`, `ApprovalDecision`, `FixedApprover`, and
  `AgentProfile::approval_classifier`: ask a human before `Mutate` actions and remember
  "allow for this session" per approval class.
- `CancelToken` and `Harness::cancel`: cooperative cancellation; `HarnessOutcome::cancelled`.
- `ContextReport`, per-request `HarnessEvent::Context`, and `Harness::preview_context`.
- `Session` (with `CarryPolicy`, `Turn`, `SessionRecord`, `SessionUsage`, `CompactReport`):
  stored conversations with a bounded carry-over, `Session::compact`, and store tables
  `sessions` / `session_turns`. `ResearchState::compact`.
- SDK: `Ark::start_session`, `open_session`, `latest_session`, `ask_session`,
  `context_preview`, `harness`, `router`, `set_router`, `settings`, `find_or_create_project`,
  `ArkBuilder::engine`, `TurnOptions`.
- `ActionDescriptor::reusable`: results that stay valid across turns.
- `ARK_PROVIDER=mock` with `ARK_MOCK_SCRIPT` (and `ARK_MOCK_DELAY_MS`) replays a scripted model
  for end-to-end tests of front-ends. `MockProvider::with_fallback`, `with_chunk_delay`.

### Breaking
- `HarnessEvent` is owned (`String`/`Value` fields instead of borrows), `Clone` and
  `Serialize`, and has new variants: `Step`, `Context`, `TextDelta`, `ModelCall`,
  `ApprovalRequested`, `ApprovalResolved`, `Cancelled`. `ToolCall` gains `id`; `ToolResult`
  gains `id`, `invocation`, `duration_ms`, `error`; `Answer` gains `streamed`. Match arms that
  bind fields need `..`.
- `HarnessOutcome` gains `cancelled`.
- `ArkBuilder` reads `ARK_CTX_BUDGET` for its default budget, as `Harness::new` already did.

### Fixed
- Consumer distillers now run before ARK's common distiller, as `docs/EXTENSION.md` always
  said. Before, a domain `list` or `read` summary was shadowed by the generic one.
- Evidence is only reused across turns for inputs that cannot change (a content-addressed
  `artifact`, or a `reusable` action). A workspace file read in an earlier session is no
  longer served stale. Within a turn, identical reads are still reused. `Mutate` calls are
  never deduplicated.

## [0.1.3] - 2026-09-30

### Added
- `HarnessOutcome::grounded()`: true when the answer cites at least one piece of evidence.
  A convenience for consumers that gate on grounding.

## [0.1.2] - 2026-09-30

### Added
- `ingest_bytes` now also indexes an `ArtifactKind::Other(_)` artifact for full-text search
  when its bytes are valid UTF-8 (previously only `is_textual()` kinds were indexed). Lets a
  consumer's text-bearing domain format (a plist, a JSON trace, an outline) be searchable
  without ARK knowing the format. Binary `Other` bytes fail the UTF-8 guard and are skipped.

### Breaking
- `AcquisitionProvider` is now an **async** trait (`find`/`propose`/`acquire` are `async fn`),
  since acquiring a provider is inherently I/O (probe, install, connect). Refines the seam
  introduced in 0.1.1 before it ships in a released consumer.

## [0.1.1] - 2026-09-30

### Added
- **Capability-acquisition seam** (async `AcquisitionProvider` trait, `AcquisitionResult`), injected
  via `AgentProfile::acquisition`. When a profile supplies one, the runtime enables the
  `find_capability` / `propose_capability` / `acquire_capability` meta-tools and dispatches to
  it (acquisition gated by the same approval as a `Mutate` action). Previously these meta-tools
  were stubbed as "not enabled". Mirrors the existing `KnowledgeProvider` seam and stays
  domain-agnostic (a security agent installs a disassembler; a writing agent could wire a
  thesaurus). Discovered while migrating the security workbench onto ARK.
- **`HarnessEvent::Capability { action, detail }`**: emitted when an acquisition meta-tool
  runs, so a UI can surface it. This is additive; match arms should already carry a `_ =>`.

## [0.1.0] - 2026-09-30

### Added
- Initial extraction of a domain-agnostic agent runtime from a security-research workstation.
- Crates: `ark-core`, `ark-model`, `ark-store`, `ark-tools`, `ark-runtime`, `ark-sdk`.
- Two agent loops (`Agent`, `Harness`) with a managed context budget, model routing,
  deterministic distillation, progressive tool disclosure, a persistent working state +
  investigation graph, a self-check + critic, an approval gate, and full metrics.
- Model providers: Anthropic, OpenAI(-compatible), Ollama, and a scriptable Mock.
- Out-of-process tools via an MCP plugin bridge.
- `AgentProfile` as the single injection point for domain persona, capabilities, distillers,
  and knowledge.
- Example agents: `minimal-agent`, `writing-agent`.
- Docs: README, architecture, extension guide, versioning, roadmap, extraction map.

The security-specific vocabularies of the source (a fixed capability enum, binary/RE artifact
kinds, typed RE/network evidence families, the security persona and distillers) were removed
from the core and made consumer-supplied.
