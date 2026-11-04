# Core 0.33.0 release

| Release state | Value |
| --- | --- |
| Latest tag | v0.33.0 (2026-11-04). Release source prepared; public checks recorded below. |
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

## Codex daily-use readiness candidate

`ark-daily-use-schedules` contains the Codex host transport candidate (ADR 0098).
The official macOS arm64 0.166.0 binary and metadata qualification were checked
without inference. Model tests and focused lifecycle checks passed before the
account identity refinement. Independent implementation review and actual protected
manual, timer and restart acceptance remain open. No commit or release was made.
Evidence: `../board/evidence/ark-daily-use-20261104/schedules/`.
