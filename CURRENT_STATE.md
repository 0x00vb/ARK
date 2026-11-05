# Claude Code cache reuse (in development, 2026-11-05)

| State | Value |
| --- | --- |
| Where | worktree `../ark-claude-cost`, branch `claude-cost` from v0.34.0. Uncommitted. Desktop test build in `../ark-desktop-claude-cost` (gitignored `[patch]`) |
| Change | ADR 0102: the system prompt names no project; on Claude Code the turn's preloaded actions arrive with the second request; a reply after Claude Code's structured-output enforcement is not asked again |
| Measured | 5 coding tasks x 3, live `claude:sonnet`: est. cost per success $0.080 to $0.047 (Claude Code $0.069), fresh input 211k to 88k, success 14/15 plus 5/5 extra (installed 15/15). Evidence `../board/evidence/ark-vs-claude-code-20261105/round2/` |
| Gates | fmt, workspace clippy `-D warnings`, ark-model/ark-runtime/ark-bench/ark-sdk/ark-interact tests, simulated bench (baseline regenerated, +0.2% tokens), QA GO. Full `scripts/ci.sh` and page tests not run: free disk under 20 GB |
| Not covered | orchestrated agents (`agent.rs`) still name the project in their system prompt; Claude Code's own structured-output enforcement retry |
| Next | owner: commit and release; Desktop pin |

# Core 0.34.0 release

| Release state | Value |
| --- | --- |
| Latest tag | v0.34.0 (2026-11-05). Compact Automations cards and the Work home. |
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
