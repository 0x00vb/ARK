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
