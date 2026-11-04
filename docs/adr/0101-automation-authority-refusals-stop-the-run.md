# 0101. Automation authority refusals stop the run

Status: Accepted. Live schedule qualification remains open.
Date: 2026-11-04

## Context

A live manual acceptance run ended as completed, with a refusal in its answer and no
file effects. Its original call arguments were not recorded, so the claimed whitespace
cause is unproven. Separate actual Orchestrator tests reproduced a definite defect:
a frozen-bound refusal could be followed by an allowed effect and a completed run.

The guard returned an ordinary refusal without retaining a run fault. A later model
answer could therefore finish the step. A finished answer does not override authority.

## Decision

Genuine authority denials retain the first fault in the run's shared guard. They stop
later actions and model requests and fail the run. This includes grant integrity,
unapproved actions or inputs, changed descriptors, frozen-bound mismatches, delegation
outside the grant and exhausted call limits or quotas. General, named and delegated
work share the fault.

Malformed permitted internal read queries and limits return a recoverable refusal.
They consume no quota. Ordinary adapter or schema validation remains recoverable.
These outcomes use the existing typed verdict and error paths, not error-text matching.
A request to load an unapproved action remains terminal even with malformed arguments.

Protected work advertises the intersection of its existing tools and the frozen grant.
The intersection runs after adapter registration in General, named and delegated work.
Scoped engines freeze that registry. Derived core actions, skill catalogs and delegation
guidance follow it. Protected profiles use the existing flat action index and disable
capability acquisition. This does not add a tool absent from the existing agent registry.
Ordinary chat profiles and project instruction text are unchanged.

Missing or malformed action lookup names are correctable queries. A valid unapproved
name remains a terminal denial. Existing storage and integrity failures still halt work.

Text-only and read-only tasks may still complete without mutations. This change does
not infer semantic success from an answer or classify every voluntary refusal.
It does not expand permissions or change the ordinary Engine refusal policy.

## Verification

Actual Orchestrator tests cover general and named work, a refused bound followed by
an answer, a refused action followed by an allowed action in the same batch, an allowed
control, corrected internal read arguments, shared delegated faults and store reopen
without replay. The original red tests and failed live attempt remain retained.
Live manual, forbidden, timer and restart checks must use a fresh store and recorded
call arguments. Evidence: workspace board/evidence/ark-daily-use-20261104/schedules.
