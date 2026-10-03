# 0002. Working state is the memory, not the transcript

Status: Accepted
Date: recorded 2026-10-03

## Context
Replaying a conversation into each request makes cost grow with session length and lets
old, irrelevant output crowd the context.

## Decision
The model sees a budgeted request built from working state, distilled results and on-demand
reads from the store. Terminal or window history is never replayed into a prompt. Every
context source goes through the budget and shows up in `ContextReport`.

## Consequences
- Request size stays flat over long sessions ("flat memory over 2,000 turns", `ROADMAP.md`).
- Large data stays in the store; tools read it on demand.
- A new context source must fit the budget and be visible in `/context`.

## Check
`crates/ark-sdk/tests/long_session.rs`; `ark-bench`.
