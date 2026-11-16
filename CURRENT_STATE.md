# Core 0.49.0 release: pictures in agent chats, fixes from the owner's traces (2026-11-15)

| Release state | Value |
| --- | --- |
| Latest tag | v0.49.0 (2026-11-15), over v0.48.1 (bee8398). ADR 0121 (pictures go with an agent chat message), ADR 0122 (a note after narrated calls is nudged once, recent answers carried whole) |
| Change | `submit_task` and `work_admit` take `task.images` (person only; sha256 and size checked when the turn starts; refused for delegation, `agents.start`, schedules, automations, group turns); `hello` answers `images: true` and a chat asks its own runtime; `chatView` passes pictures. A brief task's one-line note after narrated calls gets one nudge; recent answers carried up to 4000 and 2000 characters with their lines; the authority line in an agent's prompt when it has someone to ask. Breaking: `TaskSpec.images` |
| Found from | The owner on 2026-11-15: a picture dropped in an agent chat only said "Screenshot..."; the trace review `../board/evidence/trace-review-20261115/REPORT.md` |
| Gates | qa (haiku): plan gates for both (images: 3 blockers; traces: 1 blocker, 4 majors), implementation gates (images: 3 blockers, then a recheck pass with should-fix items; traces: 1 blocker, then a recheck pass). Live Haiku: a picture of a red circle with "42" described correctly; an agent with changes asking called `files.write` and was approved, no "read-only" claim. The live rerun of the nudge rule on the final code was refused by the permission classifier for a subagent and waits for the owner |
| Release commit | `ci.sh` with page tests: fmt, protocol, version, macOS 11 check, clippy, workspace tests (one run before the release commit failed `conversations` `search_finds_a_fact...` on a score tie, 15 of 15 alone, card `recall-search-tie-20261115`), node 22 of 22, WebKit 178 of 178 plus `chat-view-images` 11 of 11 (now in `ci.sh`), page suite 67 of 69 (the 2 known), notification 3 of 3, release build, simulated benchmark no delta against the regenerated baseline (23 workloads) |
| Consumer | Desktop 0.17.0 pins this tag |
| Not verified | A picture dropped in the real Desktop window with a live model; the phone (card `phone-agent-images-20261115`) |
| Evidence | `../board/evidence/agent-chat-images-20261115/`, `../board/evidence/trace-fixes-20261115/` |

# Pictures in agent chats: ADR 0121 on branch agent-images, not released (2026-11-15)

| State | Value |
| --- | --- |
| Change | `submit_task` and `work_admit` take `task.images`; read and fingerprinted at admission, checked at the turn start, shown to the model, stored with the turn, on `TaskNode.images`; `hello.images`; `chatView` passes `{ images, named }` and exposes `addImages`, `stashImages`, `restoreImages`; `ark_files::images` is the one reader |
| Next | Release v0.49.0 (`TaskSpec` has a new public field: Breaking), then Desktop bumps its pin. Commits `f188daa`, `084e8b8` (and one for this note) |
| Open | A project on another computer keeps the "Added to the project" line: the page cannot read that runtime's hello (`ark.taskImages()` is false there). A busy chat refuses a picture with a message (the chat view's composer does not queue). The phone sends no pictures to a task (card `phone-agent-images-20261115`) |
| Evidence | `../board/evidence/agent-chat-images-20261115/` |

# Core 0.48.1 release: a chat opens at its newest message (2026-11-15)

| Release state | Value |
| --- | --- |
| Latest tag | v0.48.1 (2026-11-15), over v0.48.0 (766d8b8) |
| Change | A chat opens at its newest message (the saved scroll position is gone; `Thread.restore()` after the workspace re-attaches a surface, which WebKit resets to the top). Away from the end, a round arrow over the composer goes back and follows again. A sent message no longer stays the saved draft; queued ones and text put back after a failed send do |
| Found from | The owner on 2026-11-15, with a screenshot of the arrow they wanted |
| Gates | qa (haiku): 2 blockers (queued and failed-send drafts) and a leak, fixed with tests that fail without them; recheck pass, one pre-existing gap carded (`queued-message-reload-20261115`). WebKit 181 of 181, node 22 of 22, page suite 67 of 69 (the 2 known). Release commit: see below |
| Release commit | `ci.sh` with page tests: fmt, protocol, version, macOS 11 check, clippy, workspace tests, node 22 of 22, WebKit 178 of 178, page suite 67 of 69 (the 2 known); notification 3 of 3; release build; simulated benchmark no token or cost delta |
| Consumer | Desktop 0.16.1 pins this tag |
| Evidence | `../board/evidence/chat-scroll-end-20261115/` |

# Core 0.48.0 release: an agent schedules work from its own chat (2026-11-13)

| Release state | Value |
| --- | --- |
| Latest tag | v0.48.0 (2026-11-13), over v0.47.0 (d2e9280). ADR 0120 (an agent schedules work from its own chat) |
| Change | `agents.schedule` and `agents.unschedule` on a named agent's chat turn (not on scheduled runs, started or delegated work, or Plan); each run posts once into that chat as the agent's message, scrubbed; the chat's mode decides whether creating it asks (Auto no, ask mode yes); `to` is only the asker; an agent sees only schedules it made; at most 10, at most once an hour. A schedule reporting into a named agent's chat no longer fails at its first run (admission checked the chat against the wrong project) |
| Found from | The owner's plan of 2026-11-12 (P4) and the decision "a new schedule inherits the chat's mode" |
| Gates | qa: plan (4 blockers decided), implementation (1 blocker), two rechecks (2 + 2 blockers), each fixed with a test that fails without it. Release commit: see below |
| Owner to confirm | An agent with `allow_mutate` does not ask for a schedule even in ask mode (it asks for nothing today) |
| Not verified | A live model scheduling from chat; the real app |
| Release commit | `ci.sh` with page tests: fmt, protocol, version, macOS 11 check, clippy, workspace tests, node 22 of 22, WebKit 170 of 171 (`agent-sidebar` ⌘N fixture failed under load, 28 of 28 three times alone; it also flakes on v0.46.0), page suite 66 of 68 (the 2 known), notification 3 of 3, release build, simulated benchmark no delta |
| Fresh clone | `verify-release.sh v0.48.0`: stopped at `ark-browser` `a_person_surface_has_its_own_context_frames_navigation_and_popups` (Chrome start under load, as on v0.36.0 to v0.44.0); it passes alone, and the release tree passed every workspace test |
| Consumer | Desktop 0.16.0 pins this tag (committed; signing waits on the Apple developer agreement) |
| Evidence | `../board/evidence/business-team-20261112/p4/` |

# Core 0.47.0 release: an agent answers later, agents' exchanges in one line (2026-11-13)

| Release state | Value |
| --- | --- |
| Latest tag | v0.47.0 (2026-11-13), over v0.46.0 (a5156f2). ADR 0118 (an agent answers later in the same chat), ADR 0119 (agents' exchanges are read from what agents already keep) |
| Change | P3: `work.later {brief}` on the person's own turn or a group-chat turn hands the work to an unlisted conversation of the same agent and posts the answer once into the same chat as the agent's next message (`Post.kind = message`, no card); the tile shows it working, Stop ends it and posts nothing, at most 3 open, approvals as the turn. P2: `exchange_lines` and `exchange_talk`; `chatView({ exchanges })` draws one quiet line under a turn that reached other agents ("2 messages with CMO") and opens their messages over the chat (Back or Esc returns). Stored and shown text from agents is scrubbed (`ark_store::scrub_text`) |
| Found from | The owner's plan of 2026-11-12 (`../board/evidence/business-team-20261112/PLAN.md`, P2 and P3) |
| Gates | Each branch: plan/design and implementation reviews by qa (haiku) with rechecks; blockers fixed with tests that fail without them (scrubbing, offer scope, failed ask, stale read, occurrence key, hidden lanes). Release commit: see the ci line below |
| Not verified | A live model calling `work.later` for slow work and not for a quick question; real agents messaging each other in Desktop; the phone (P5) |
| Open | Exchange view: no focus trap; Esc is captured at document level; a hidden document does not refresh until the next event. P3: the phone draws a message post as a card until P5 |
| Release commit | `ci.sh` with page tests: fmt, protocol, version, macOS 11 check, clippy, workspace tests, node 22 of 22, WebKit 171 of 171, page suite 66 of 68 (the 2 known); notification 3 of 3; release build; simulated benchmark no delta |
| Fresh clone | `verify-release.sh v0.47.0`: all checks passed |
| Consumer | Desktop 0.16.0 pins this tag (committed a06c1ba; signing waits on the Apple developer agreement) |
| Evidence | `../board/evidence/business-team-20261112/p2/`, `../board/evidence/business-team-20261112/p3/` |

# Core 0.46.0 release: a chat makes a group of agents, links open, selection shows (2026-11-13)

| Release state | Value |
| --- | --- |
| Latest tag | v0.46.0 (2026-11-13), over v0.45.0 (be08c0c). ADR 0117 (a chat makes a group of agents with shared rules) |
| Change | `groups.set` on the person's own turn (first call): new members with their own role and personality, existing agents join, shared always-rules (20 x 300 per group, 20 per agent), sibling `agents.message` and `agents.start`, no delegation. Work that reaches an agent only through a group (any hop) asks before it changes anything, and a group-only member cannot message an agent with `allow_mutate`. New agents are made first; a failure ends those made and writes nothing else, and the same call later makes them again. `agents.create` no longer copies look, standing, title, description or delegates. Links in replies: web links open in the browser (`ark_open_link`), relative paths open in the file view. `--selection` token per palette; no cyan |
| Found from | The owner's requests of 2026-11-12 and 2026-11-13: agents that work as a team from chat (plan `../board/evidence/business-team-20261112/PLAN.md`, P1), selection not visible on any palette, links in replies do nothing |
| Gates | fmt, protocol, version, macOS 11 check, clippy `-D warnings`; workspace tests pass; node 22 of 22; WebKit fixtures 146 of 146 (new `palette.test.mjs`, `links.test.mjs`); real-host page suite 66 of 68 (the 2 on the v0.36.0 to v0.45.0 known list: `an_apps_conversation_draws_exactly_as_the_chat`, `primary_buttons_read_in_light_and_dark_with_any_accent`); notification 3 of 3; release build; simulated benchmark no delta. Five qa reviews (plan, implementation and three rechecks): blockers fixed, each with a test that fails without it (rollback and retry, group-only work asks, group-only message refused, a person-ended member stays ended, delegation chain asks, window group members join) |
| Not verified | A live model calling `groups.set` at the right moment and not for a one-off request; a Desktop screenshot of a group made from chat; a remote-runtime window's links (`window.open` there is still dropped) |
| Open | ADR 0113 group chats: a member's `@Name` reply starts another member's turn as a person turn (board card `group-chat-handon-gate-20261113`) |
| Fresh clone | `verify-release.sh v0.46.0`: all checks passed, release verified from a fresh clone of the canonical remote |
| Consumer | Desktop 0.16.0 pins this tag (committed; signing waits on the Apple developer agreement) |
| Evidence | `../board/evidence/business-team-20261112/` |

# Core 0.45.0 release: fixes from the owner's first day on 0.44.0, an agent keeps the role it is told (2026-11-09)

| Release state | Value |
| --- | --- |
| Latest tag | v0.45.0 (2026-11-09), over v0.44.0 (cc27fbe). ADR 0114 and 0115 amendments, ADR 0116 (an agent keeps the role its person gives it) |
| Change | One-row composer; a new group offers only the sidebar's agents (`ark.hiddenFolders()`); the sidebar's round buttons share one size (mouse and touch). Agents on another computer: `cancel_task` answers within 3 s (`stopping`), `agent_folder` acts on that computer (absolute path, checked there), `agent_move` keeps a model the target offers, the listing asks every computer at once. ADR 0116: `role.set` and `AgentSpec.standing`, offered only on the person's own turns and only as the first call of the turn, no card |
| Found from | The owner on 2026-11-09: deleted agents in the group picker (a hidden folder's agents), composer not like the reference, a slow "hi" and a dead Stop on an agent moved to a second Mac, "Checking…" stuck, folder should be on the server, button sizes; then: tell a new agent its role in chat, no UI |
| Gates | fmt, clippy `-D warnings`; workspace tests 1571 of 1571 (then `ark-ui` + `ark-orchestrate` 534 of 534 after the last review fixes); node 22 of 22; WebKit fixtures 139 of 139; page suite: see below; simulated benchmark no delta. Three implementation reviews and one plan review (qa); blockers fixed with tests that fail without them |
| Not verified | A real second computer (Stop, model list, folder); a live model calling `role.set` at the right moment and not for a one-off request (needs the owner's OK); the native window by hand |
| Fresh clone | `verify-release.sh v0.45.0`: all checks passed, release verified from a fresh clone of the canonical remote |
| Consumer | Desktop 0.15.1 pins this tag (published 2026-11-10) |
| Evidence | `../board/evidence/agent-fixes-20261110/` |

# Core 0.44.0 release: agent details, groups, an agent's computer (2026-11-09)

| Release state | Value |
| --- | --- |
| Latest tag | v0.44.0 (2026-11-09), over v0.43.1 (ccdff2f). ADR 0113 (group chats), ADR 0114 (an agent runs on a computer the person picks), ADR 0115 (an agent's details; apps without the assistant) |
| Change | Details pane: Name, Model, Computer, Folder, pet. `agent_folder`; `workspace_dir` takes the nearest folder and refuses an agent folder that is gone. `ark.newAgent({assistant:false})`: no ARK tile, latest agent opens, one New agent button when none. Groups: `group_*` calls, router or @Name, at most 6 replies a message, tiles for a `groupConversation` view, turn `author`. `agent_move` and placed agents in `agents {all}`. Phone: `agent.create` takes `title` and `look`; `AgentRow` gains `model`, `description`, `folder`, `own_folder`, `place`, `session`, `location` |
| Found from | The owner's requests of 2026-11-09 (change an agent's folder and name, setup out of the composer, no default ARK, group chats like Grok bots, pick each agent's computer, deletes reach the phone) |
| Gates | `scripts/ci.sh` with page tests: fmt, protocol, version, macOS 11 check, clippy, workspace tests, node 22 of 22, WebKit fixtures 136 of 136, real-host page suite 66 of 68 (the 2 on the v0.36.0 to v0.43.0 known list: `an_apps_conversation_draws_exactly_as_the_chat`, `primary_buttons_read_in_light_and_dark_with_any_accent`), notification 3 of 3, release build, simulated benchmark no delta. Two implementation reviews (qa, Haiku): blockers fixed with tests or cut (Description); `../board/evidence/agent-groups-20261109/REVIEW.md` |
| Fresh clone | `verify-release.sh v0.44.0`: fmt, protocol, version, macOS 11 check and clippy pass, then it stops at `ark-browser` `a_person_surface_has_its_own_context_frames_navigation_and_popups` (Chrome start under load, the environment failure of v0.36.0 to v0.40.0). Workspace tests, release build and benchmark passed in the release tree |
| Consumer | Desktop 0.15.0 pins this tag; ARK for iPhone build 20 sends `agent.create` with `title` and `look` |
| Not done | A group message to live models (needs the owner's OK); the native window by hand; a real second computer for `agent_move`; groups on the phone; placed agents on the phone |
| Evidence | `../board/evidence/agent-groups-20261109/` |

# Core 0.43.1 release: name a new agent, delete one (2026-11-09)

| Release state | Value |
| --- | --- |
| Latest tag | v0.43.1 (2026-11-09), over v0.43.0 (497c8d3). `where: "agent"` actions, toast with Undo, window calls `agent_end` and `agent_restore` |
| Change | ADR 0112 amendment: an app adds items to an agent tile's menu; `api.toast(msg, {action})`; top-level window calls end an agent in its own top folder (chats stay) and spawn its stored spec again. Chat-scoped resource ops still act only on their chat's folder (a test proves a chat ref cannot end another folder's agent) |
| Found from | The owner's review of 0.43.0 / Desktop 0.14.0: + did not ask a name, pets did not show, no delete |
| Gates | `scripts/ci.sh` passes. Web fixtures agent-sidebar, project-work, work-state: 72 of 72. Implementation review (qa): 1 blocker (authority widening, replaced by window calls), 4 majors, all fixed |
| Dogfood | Desktop `--serve --tokens` on embedded assets and a copy of the owner's store: named agent created with its pet, Delete from Settings and across folders, Undo restores |
| Release | Owner OK 2026-11-09 |
| Consumer | Desktop 0.14.1 pins this tag |
| Not done | Native window not opened by hand. iPhone does not draw `look` yet |
| Evidence | `../board/evidence/agent-polish-20261109/` |

# Core 0.43.0 release: one sidebar and an agent's look (2026-11-09)

| Release state | Value |
| --- | --- |
| Latest tag | v0.43.0 (2026-11-09), over v0.42.1 (6b8f9fd). One sidebar, + makes an agent, `AgentSpec.look` (ADR 0112) |
| Change | ADR 0112: no fixed rail; round Search and + at the sidebar top (+ calls the app's `ark.newAgent`, else New chat); tiles centered; profile and app views in the foot; trailing-edge grip resizes 200 to 400 px, hides under 150; `AgentSpec.look` (display, max 64) in graph nodes and `agents {all}` rows; `agentIdentity((name, look))` |
| Found from | The owner's review of 0.42.0 with a reference screenshot |
| Gates | `scripts/ci.sh` passes. Web fixtures: agent-sidebar 22 of 22 (4 new, each failing without its change where checked), project-work 23 of 23, work-state, vm-module 22 of 22. Implementation review (qa, Haiku): 7 majors, all fixed |
| Dogfood | Desktop `--serve --tokens` on a copy of the owner's store: + made Agent 1 to 3 with pets, rename and pet saved (`look` read back from the store) |
| Release | Owner OK 2026-11-09. Tag v0.43.0 on origin and github. Release commit: `ci.sh` passes, page suite 66 of 68 (the 2 on the v0.36.0 to v0.42.0 known list). `verify-release.sh v0.43.0` passes from a fresh clone, every step |
| Consumer | Desktop 0.14.0 pins this tag; published 2026-11-09 |
| Also | Client protocol `AgentRow` gains `look` (optional in Swift), found by v0.42.1's route test after the rebase |
| Not done | Native window not opened by hand. iPhone does not draw `look` yet |
| Evidence | `../board/evidence/agent-sidebar-20261109/` |

# Core 0.42.1 release: the phone lists agents and their chats (2026-11-09)

| Release state | Value |
| --- | --- |
| Latest tag | v0.42.1 (2026-11-09), over v0.42.0 (79dc80e). Client protocol 1.11 (ADR 0111) |
| Change | `GET /client/v1/agents`, `GET /client/v1/agents/{project}/{name}/conversations`, mutation `agent.conversation.start`; `GET /client/v1/chats` lists each agent's chats; a send to a non-default chat runs in or steers that chat (`TaskSpec.thread`) |
| Consumer | Desktop 0.13.1 pins this tag; ARK for iPhone builds on protocol 1.11 |
| Gates | pending |

# Core 0.42.0 release: agent-first window (2026-11-09)

| Release state | Value |
| --- | --- |
| Latest tag | v0.42.0 (2026-11-09), over v0.41.0 (9473c65). Agent-first window (ADR 0109, ADR 0110) |
| Change | ADR 0109: an agent has many chats (`TaskSpec.thread`), they run at once (lane lock per chat), `AgentRecord.running` with derived state, steering and late mail per chat. ADR 0110: the sidebar lists agents (ARK first), Recent chats in the title, `/history`, folders in the details pane and palette, `conversations.search`/`read` recall, `memory.suggest` for agents' chats with the person. `ark.rail` and `ark.projectSection` removed |
| Found from | The owner's request of 2026-11-09 (agent-first navigation, a reference screenshot). Owner chose: every existing chat becomes ARK's; one agent's chats run in parallel |
| Gates | Plan (fail, then pass after revision), design (pass after revision) and implementation reviews by `qa`; findings fixed with tests. Rust: fmt, clippy `-D warnings` workspace, `cargo test --workspace` pass. Web fixtures 128 of 129 (1 skipped). Real-host page suite 66 of 68: the 2 failures are on the v0.36.0 to v0.41.0 known list. Release build pass. Simulated benchmark: `board` +51 tokens (one new chat sees the recall tool), baseline regenerated |
| Dogfood | Desktop runtime on a copy of the owner's store (272 chats, 8 agents): tiles across folders, ARK Recent chats and `/history` across 7 folders, agent chat opens in its folder, New chat with the agent. Found and fixed: agent chat titles showed the rendered brief. No model call made |
| Evidence | `../board/evidence/agent-first-nav-20261109/` (PLAN, DESIGN, CONTRACT, gate logs, screens, dogfood) |
| Not verified | A live model using recall; the native Desktop window by hand; a physical phone; iPhone client calls (session aiengineering-e9 adds `client/v1` on top) |
| Live check | Owner OK: Haiku through `writer -p` on a scratch store. The first run showed recall missing (catalog apps index only tagged actions) and its results cut by the generic distiller; fixed (loaded with the turn, own distiller). Then Haiku called `conversations.search` and `conversations.read` and reported the earlier chat's figures. Logs `../board/evidence/agent-first-nav-20261109/live-haiku/` |
| Consumer | Desktop 0.13.0 pins this tag; published 2026-11-09 |
| Fresh clone | `verify-release.sh v0.42.0`: fmt, protocol, version, macOS 11 check, clippy and the whole workspace test suite pass; the run was then stopped by a SIGTERM before its release build and benchmark, which passed in the release tree |

# Core 0.41.0 release

| Release state | Value |
| --- | --- |
| Latest tag | v0.41.0 (2026-11-08), over v0.40.0 (6c5d9f4). Fixes from the owner's real Desktop traces (ADR 0108) |
| Change | `files.read` attaches png, jpeg, gif, webp up to 3.5 MB; `browser.screenshot` (Observe); one check nudge after an unchecked successful change whose namespace has an Observe action (a volatile Mutate call such as `computer.look` counts as a look), with the reply held so nothing flickers; `self_check` reads `changed N; checked after: yes/no`; `== THIS SESSION ==` line in the first message; `research_state` keyed by project and session (old rows to `#legacy`); secrets scrubbed in stored turns, traces, working state and notes; HTTP 410 is `ProviderError::Retired` and hides the model |
| Found from | 71 traced turns in `~/.ark-desktop/meta.db` (2026-10-03 to 2026-11-07). Evidence and plan: `../board/evidence/ark-trace-fixes-20261108/` |
| Checks | Plan, implementation and mutation reviews (Haiku); every fix has a test that fails without it (10 mutations caught). Gate results below |
| Not verified | A live model run (owner OK needed); the native Desktop window |

# Core 0.40.0 release

| Release state | Value |
| --- | --- |
| Latest tag | v0.40.0 (2026-11-07), over v0.39.0 (6e59609). Every chat starts in Auto (ADR 0107); an image is a picture in the composer and goes with the message (ADR 0106); a streaming reply can be scrolled up |
| Change | Auto runs every action that does not ask at every call (`ask_each_time`, `person_only`); with nobody to ask it runs only `auto_eligible`. A chat nobody chose a mode for (revision 0) is Auto, old ones too; the page's switch-to-Auto step is gone. `send` and `chat_send` take `images` (3.5 MB each, 20 MB in all, eight at most); the model is shown them; the page shrinks a larger one; `Turn::images` and `TranscriptTurn::images` keep them; Anthropic and OpenAI send a person's images. `upload_add` answers `as`. The thread lets go of the end at a wheel, touch or scrollbar |
| Found from | The owner: chats stopped on permission questions; a dropped screenshot was only named in the draft and never seen; a streaming reply could not be scrolled up |
| Checks | Two vstack runs (images, Auto) with one and two independent reviews; blockers fixed with tests that fail without them (split chats lost images; older chats went back to Manual after a message). Gates in the table below |
| Not verified | A live model reading a real image; the native Desktop window; touch; a chat placed on another computer whose project copy is skipped |

# Core 0.39.0 release

| Release state | Value |
| --- | --- |
| Latest tag | v0.39.0 (2026-11-07), over v0.38.0 (d9869a0). A panic no longer strands a chat; Enter steers a running turn; Markdown edits in place |
| Change | `Registry::run` turns a tool panic into a failed call. The host supervises each turn task: a dead turn ends with an error, closes its questions and frees the chat. Enter sends into the running turn (row reads Sending), ⌘↩ queues, split chats do the same. A `.md` file has no top bar; the block under the caret shows its Markdown (`md-inline.js`), saves on its own and merges a conflicting save by lines. A docx run of only spaces no longer panics |
| Found from | The owner opened a docx in Desktop: `task 394 panicked with message "byte range starts at 1 but ends at 0"` (`runs_markdown`), then the next turn never ended (trace `live`, the turn task was dead, `turn_finished` never sent) |
| Checks | fmt, protocol, version, macOS 11 check, clippy `-D warnings`, release build, simulated benchmark (no delta). Rust 1425 pass, 5 fail: the 5 `ark-browser` Chrome-start tests (as on v0.38.0). Web fixtures pass, including `md-inline.test.mjs` (12). Real-host page suite 61 of 67 in the full run; its one new failure, `the_plus_menu_opens_files_and_a_terminal_in_the_project_folder`, clicked the removed Markdown Edit button, was updated and passes alone. The other 5 failures are on the v0.36.0 to v0.38.0 list (`an_apps_conversation_draws_exactly_as_the_chat`, `primary_buttons_read_in_light_and_dark_with_any_accent`, `streamed_text_keeps_its_last_chunk_and_answers_once`, `text_of_a_failed_attempt_is_taken_back_and_the_retry_shows_once`, `the_window_on_a_runtime_elsewhere_has_its_controls_and_a_way_back`); `steering_lands_where_it_was_read_and_replays_the_same` passes now. Two independent reviews (default model, opus) found 2 data-loss blockers and 4 majors, all fixed with tests |
| Not verified | The native Desktop window; touch; long documents; mixed line endings are rewritten to the first-seen style |

# Core 0.38.0 release

| Release state | Value |
| --- | --- |
| Latest tag | v0.38.0 (2026-11-06), over v0.37.0 (49c5dfd). Dictation shows words while the person talks (ADR 0105) |
| Change | `dictation_preview` call, `TranscriptionProvider::segments` (default method), audio uploads while recording, no dictation labels except the model download and failures, Send closes the composer row through a spacer |
| Found from | The owner's screenshot of Desktop 0.11.0 (Core v0.36.0): `.composer-bar:has(.mode-chip)` zeroed Send's auto margin while the mode chip was hidden, so Send sat beside the microphone. v0.37.0 removed the chip. `composer-layout.test.mjs` fails on v0.36.0 (Send ends at 462 of 1020) and passes now |
| Checks | fmt, protocol, version, clippy `-D warnings`, release build, simulated benchmark (no delta). Rust 1485 pass, 11 fail: the 5 `ark-browser` Chrome-start timeouts (also on v0.37.0). Web fixtures 103 of 103 (one `work-state` failure under load passed alone and on rerun). Real-host page suite 61 of 67 with the same 6 failures as v0.36.0 and v0.37.0; the dictation scenario passes. New: `dictation_live.rs` (2), `composer-layout.test.mjs` (4, fails on v0.36.0). Independent QA found 6 defects in the first build (silence stalled previews, final text kept `[BLANK_AUDIO]`, others); all fixed, silence case fails without its fix. Not run: live Whisper on a real microphone, live benchmark |
| Not verified | Streaming dictation with real speech and the real model; native Desktop |

# Core 0.37.0 release

| Release state | Value |
| --- | --- |
| Latest tag | v0.37.0 (2026-11-06), over v0.36.0 (dd9407b). A chat changes its own mode when asked (ADR 0104) |
| Change | `mode.set` tool (person approves every change, applies from the next message), no mode chip, Send at the right edge, the open-chat strip stays over a new chat. Breaking: `createChatSurface` and `chatView` drop `mode` and `controls` |
| Known gaps | `chat_execution_mode` emits no summary event (other windows of a chat learn its mode on their next summary). No mode indicator on the page |
| Checks | fmt, protocol, clippy, release build, simulated benchmark (no delta), tests 128 binaries with only the 5 `ark-browser` Chrome-start timeouts failing, web fixtures 99 of 99, real-host page suite 61 of 67 (the same 6 fail on v0.36.0). Not run: live benchmark |
| Verification | Tag on `origin` and `github`. Fresh clone via `verify-release.sh v0.37.0` passed fmt, protocol, version and clippy, then stopped at the `ark-browser` Chrome-start timeouts (environment, as on v0.36.0) |
| Consumer | Desktop 0.12.0 pins this tag; published 2026-11-06 |

# Core 0.36.0 release

| Release state | Value |
| --- | --- |
| Latest tag | v0.36.0 (2026-11-06), over v0.35.0 (5b97a81). A calm chat page (ADR 0103). Breaking: Work and Automations pages removed |
| Change | Rail with `ark.rail` pins, title-bar identity chip and details pane, local greeting, composer chips hidden, new chats set to Auto |
| Checks | Web fixtures pass (work-state 27, project-section 42, upload-drafts 15, compose-append 3). Real-host page suite serial: 61 pass, 6 fail, the same 6 fail on v0.35.0 (`an_apps_conversation_draws_exactly_as_the_chat`, `primary_buttons_read_in_light_and_dark_with_any_accent`, `steering_lands_where_it_was_read_and_replays_the_same`, `streamed_text_keeps_its_last_chunk_and_answers_once`, `text_of_a_failed_attempt_is_taken_back_and_the_retry_shows_once`, `the_window_on_a_runtime_elsewhere_has_its_controls_and_a_way_back`). Three independent reviews triaged and fixed |
| Known gaps | `chat_execution_mode` emits no summary event (other windows of a chat learn its mode on their next summary).  |
| Consumer | Desktop 0.11.0 pins this tag; published 2026-11-06 |
| Verification | Tag on `origin` and `github`. Same tree: fmt, clippy, release build, simulated benchmark, node 22 of 22, WebKit fixtures 98 of 98, page suite 61 of 67 (the same 6 fail on v0.35.0). Fresh clone via `verify-release.sh v0.36.0` passed protocol, version and clippy, then stopped at the 5 `ark-browser` Chrome-start timeouts (environment) |

# Core 0.35.0 release

| Release state | Value |
| --- | --- |
| Latest tag | v0.35.0 (2026-11-05). Claude Code serves ARK's system prompt from its cache (ADR 0102). |
| Change | The system prompt names no project; on Claude Code a turn's preloaded actions arrive with the second request; a reply after Claude Code's structured-output enforcement is not asked again |
| Measured | 5 coding tasks x 3, live `claude:sonnet`: est. cost per success $0.080 to $0.047 (Claude Code $0.069), fresh input 211k to 88k tokens. Evidence `../board/evidence/ark-vs-claude-code-20261105/` |
| Consumer | Desktop 0.10.2 pins this tag; published 2026-11-05 on the Mac download and updater |
| Verification | Tag on `origin` and `github`. Fresh clone of `origin` at v0.35.0: version, fmt, clippy, release build, simulated benchmark (no delta) and 128 test binaries pass. Known environment failures only: 5 Chrome-start timeouts (`ark-browser`) and the 6 page tests that fail on v0.34.0 and v0.33.2 too. Logs `../board/evidence/ark-vs-claude-code-20261105/release/` |
| Not covered | orchestrated agents (`agent.rs`) still name the project in their system prompt; Claude Code's own structured-output enforcement retry |

# Core 0.34.0 release

| Release state | Value |
| --- | --- |
| Tag | v0.34.0 (2026-11-05). Compact Automations cards and the Work home. |
| Change | Automations as compact cards; Work as composer over grouped open work; runs open in the normal chat; `work-chat` and `automation-editor` views removed (ADR 0097) |
| Consumer | Desktop 0.10.1 pins this tag and opens the receipt's conversation instead of `work-chat` |
| Evidence | Web tests and real-host `page.rs` scenarios in `../board/evidence/automations-work-cleanup-20261105/` |
| Checks | Local and public fresh-clone (`verify-release.sh v0.34.0` from GitHub): fmt, protocol, version, clippy, Node 22/22, WebKit 97/97, release build and benchmark pass. Two `ark-browser` tests (Chrome takes over 10 s to start under swap) and six page tests fail the same way on v0.33.2 on this Mac |
| Not in this change | Deleting an automation (no runtime operation yet); webhook setup, execution inputs and per-step agents have no page control |

# Core 0.33.2 release

| Release state | Value |
| --- | --- |
| Tag | v0.33.2 (2026-11-04). Public checks recorded below. |
| Consumer | Desktop0.10.0 pins this tag after publication and fresh-clone qualification. |

The integrated source combines Design Studio support, direct assigned-agent Work chats,
compact automation controls, explicit automation creation and the native Computer backend.
Stored records remain compatible. Public Computer adapter changes are documented in the
changelog and ADR0095. Remote0.32.5 delivery documentation is merged into this history.

The implementation passed complete Core CI and two independent reviews before release.
The release metadata snapshot requires full CI and public fresh-clone verification.
Evidence: `../board/evidence/integration-release-20261104/` plus the local-integration,
work-automation-ui and studio-polish evidence folders from3November.

Unfinished phone-listener work, managed Blender work and inference experiments are excluded.
The normal installed Desktop app/profile is unchanged until verified signed OTA promotion.

Core0.33.0 passed source/public-clone checks and remains published unchanged. Desktop's
macOS11 package exposed an upstream bridge build-target coupling afterward. Core0.33.1
vendors only the exact pinned macOS crate and isolates its native source target from the
application target. Runtime source and application-name lookup are unchanged. The source
still uses a deprecated AppKit name resolver. A future resolver change requires separate
behavior review. Compiler warnings stay enabled; no pragma or warning flag is disabled.
Full release and fresh-public qualification must repeat for this patch.

Core0.33.2 corrects Computer opening a stopped application. Only the initial typed
AppNotFound inventory result reaches the existing guarded launch. Permission, timeout,
launch and post-launch errors propagate. Packaged native name and bundle-ID checks are
required before Desktop publication. Full source and public-clone checks repeat.
