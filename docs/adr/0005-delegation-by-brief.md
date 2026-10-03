# 0005. A delegated agent receives a brief, never a conversation

Status: Accepted
Date: recorded 2026-10-03 (introduced with `ark-orchestrate`, unreleased 0.6.0)

## Context
Passing the delegator's conversation to a worker multiplies context cost by the number of
agents and leaks unrelated material into every worker.

## Decision
A delegated agent gets a brief: the objective, the delegator's context note, its inputs,
and clipped results of tasks it depends on. Each agent works in its own child project with its own context and tools. A
delegation is a recorded tool call.

## Consequences
- Agents can run on different models and accounts.
- A worker that needs more must ask (`agents.message`) or read from the store.
- Brief size is visible per task in the graph.

## Check
`crates/ark-orchestrate/tests/orchestration.rs` checks that a reader's requests contain neither
the coordinator's context nor another reader's task.
