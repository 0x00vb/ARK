# 0004. ARK's store is canonical; vendor sessions are disposable

Status: Accepted
Date: recorded 2026-10-03 (introduced with managed sessions, unreleased 0.6.0)

## Context
Subscription providers (`claude`, `codex` CLIs) keep their own session history. Resuming a
session sends less input, but a vendor's history can grow past its limit, be lost, or diverge
from what ARK decided the model should see.

## Decision
ARK's store is the source of truth. Each agent (lane) gets its own managed vendor session that
is reused while healthy, rotated before `max_context_tokens`, and rebuilt from ARK's own
budgeted request when it is not. Lane state is in the store, so a restart resumes or
rehydrates. When ARK lets a vendor session go it deletes the vendor's file.

## Consequences
- Losing a vendor session costs one larger request, never lost state.
- Execution can move (another process, later a remote runtime) without moving vendor state.
- Vendor-specific session handling stays in `ark-model`.

## Check
`docs/ORCHESTRATION.md` (managed sessions); tests in `ark-model` and `ark-orchestrate`.
