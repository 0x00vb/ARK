# 0099. Saved automations bind model connections

Status: Accepted
Date: 2026-11-04

## Context

A selected model name does not identify its account. Recreating a provider after an
activation can connect the same model to a different account. A saved grant must not
silently authorize that replacement, including after a process restart.

## Decision

FrozenSelection stores a map from each reachable provider model identifier to its
connection revision. Revisions serialize as decimal strings so JavaScript preserves all
64 bits. The map covers both model tiers and reachable agents. Duplicate identifiers
with different revisions prevent activation.

The runtime checks the effective intersection of engine and harness guards before each
model request, including retries, planning, final answers and criticism. Automation
preflight checks both tiers before recovering saved actions. A changed or unrecorded
connection refuses the run. The refusal remains attached to the run even when an optional
model stage returns an ignored error.

Existing grants omit the map and retain their exact digest. They allow only providers
whose revision is zero. An account-bound provider with a nonzero revision requires a new
activation. No migration rewrites an existing approved digest.

The Codex transport separately enforces its full private account identity. The public
revision is a change detector, not an authentication credential or identity disclosure.

Vendor lane admission compares the durable account configuration revision. Model grants
compare the execution connection revision. These values are separate because lazy
qualification can discover an execution identity after a lane has opened. Managed and
stateless providers preserve that execution revision when forwarding a protected call.

## Verification

Runtime tests exercise changed accounts, unchanged accounts, retries and either side of
the engine/harness guard intersection. Orchestrator tests exercise both tiers, delegated
models, unknown models, restart, persistent refusal and legacy digest compatibility.
Evidence is in the workspace board/evidence/ark-daily-use-20261104 directory.
