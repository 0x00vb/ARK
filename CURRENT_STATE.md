# Local integration candidate, 3 November 2026

Both repositories use `codex/local-integration`. Released baseline: Core0.32.5 and
Desktop0.9.17. Completed Design Studio, automation menus/controls and Computer backend
donors are combined without modifying the originals. Historical completed donor fixes
already present in the baseline are deduplicated. Active phone work, stalled Blender
work and stopped inference experiments remain separate.

The live native app is `/tmp/ark-local-integration-20261103/ARK Development.app`.
It reads the integrated Core/Desktop UI directories on disk and reloads on saves.
Its private test profile is preserved at its existing proof-store path. A pre-integration
backup is retained. The normal installed app/profile is unchanged.

Verification and provenance: `../board/evidence/local-integration-20261103/RESULT.md`.
No commit, push, shared-branch merge or release.

| Release state | Value |
| --- | --- |
| Latest tag | v0.32.5 (2026-11-02), published baseline. Local integration is unreleased. |

Work and Automations UI follow-up is locally verified on the same candidate. Work sends
create or reuse the assigned agent and open its existing sidebar chat directly. Controls,
composer, owner menus and Automations naming are simplified. Final browser95/95, surface63/63,
actual-host flows, serialized Rust/lint/slop, release build and unchanged benchmark pass.
Desktop qualification combines the complete runs with isolated passing retries. The private
native proof profile still has an invalid Ollama model configuration; real-host scripted
answers and native navigation/error display were verified. No commit or release.
Evidence: `../board/evidence/work-automation-ui-20261103/RESULT.md`.

Studio polish on this integration branch requires explicit automation creation and moves
only Studio's original floating conversation default to the right. It publishes measured
conversation height so the design dock stays clear. Desktop owns design tools, paint,
fonts and library. Full Core CI passed with correct shared CARGO_TARGET_DIR; Desktop full
checks and real-host/native controls passed. Both independent final reviews found no remaining blockers. Workflow complete.
Evidence: `../board/evidence/studio-polish-20261103/RESULT.md`. No commit or release.
