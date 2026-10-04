# Architecture decisions

One file per decision that constrains future work. The code and tests are the truth for how
things work; an ADR records why a choice was made, so the next session does not undo it.

## Index

| # | Decision | Status |
|---|---|---|
| [0001](0001-no-domain-in-core.md) | The runtime names no domain, tool or file format | Accepted |
| [0002](0002-bounded-context.md) | Working state is the memory, not the transcript | Accepted |
| [0003](0003-deterministic-before-inference.md) | Code decides what code can decide; a model call needs a benchmark | Accepted |
| [0004](0004-ark-state-canonical-vendor-sessions-disposable.md) | ARK's store is canonical; vendor sessions are disposable | Accepted |
| [0005](0005-delegation-by-brief.md) | A delegated agent receives a brief, never a conversation | Accepted |
| [0006](0006-release-by-immutable-tag.md) | A release is an immutable git tag; consumers pin it | Accepted |
| [0007](0007-the-chat-creates-agents-and-tasks.md) | The person's chat creates agents and tasks; forms are not the way in | Accepted |
| [0008](0008-planner-off-by-default.md) | The planner is off by default | Accepted |
| [0009](0009-skills-load-on-demand.md) | Skills: names in context, instructions on demand | Accepted |
| [0010](0010-web-search-without-a-key.md) | Web search: the model's own first, keyless after, no key | Accepted |
| [0011](0011-capabilities-and-connections.md) | Capabilities people see, connections ARK owns | Accepted |

0001 to 0006 were recorded on 2026-10-03 from decisions made earlier and documented in
`AGENTS.md`, `docs/ARCHITECTURE.md`, `docs/ORCHESTRATION.md` and `docs/RELEASING.md`.

## Writing one

- When: a choice between real alternatives that a later change could quietly reverse (a
  boundary, a data format, a protocol, a measured-and-rejected idea).
- Name: `NNNN-short-slug.md`, next free number. Add a row to the index.
- Never rewrite an accepted ADR's decision. Supersede it: new ADR, old one gets
  `Status: Superseded by NNNN`.
- Keep it under a page.

```markdown
# NNNN. Title

Status: Proposed | Accepted | Superseded by NNNN
Date: YYYY-MM-DD

## Context
What forced a choice. Facts, numbers, links.

## Decision
What we do, in one or two sentences.

## Consequences
What gets easier, what gets harder, what is now ruled out.

## Check
The test, script or review step that keeps it true.
```
