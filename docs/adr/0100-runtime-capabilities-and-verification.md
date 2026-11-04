# 0100. Describe execution boundaries and verify changed outcomes

Status: Accepted. Live qualification remains open.
Date: 2026-11-04

## Context

The matched daily-use condition passed eight native cases and six ARK cases after trace
review. One ARK coding result was correct but had not been tested. One recovery result
refused a standalone Python script. The execution description directed every project
command to process.start, even when the provided Python runtime could execute the source.
The other recovery repetition executed the unchanged script through that runtime and
recorded its actual failure and successful retry.

## Decision

The execution description distinguishes compatible standalone project source in the
provided runtime from commands needing other installed programs or the project's own
environment. The latter still need process.start. Sandbox, packages, network restrictions,
approvals and input validation are unchanged.

Default instructions ask for available checks that exercise a changed artifact's requested
outcome. Verification is proportional. Repeating equivalent checks is unnecessary. The
answer reports actual checks and any required checks that could not run. A successful
write alone does not establish changed behavior.

## Verification

Run the existing execution containment and artifact tests, complete Core CI and simulated
benchmarks, then qualify changed model behavior in a separately frozen live condition.
Keep the prior failed condition and its unknown usage. Never replace failed attempts or
weaken graders to claim parity. Token and cache counts remain separate from billing.
