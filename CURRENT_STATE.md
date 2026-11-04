# Core 0.33.1 release

| Release state | Value |
| --- | --- |
| Latest tag | v0.33.1 (2026-11-04). Release source prepared; public checks recorded below. |
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
