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
| Next | Desktop pins v0.42.0 |

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
