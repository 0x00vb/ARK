# 0131. A finished turn shows its answer, not its steps

Status: Accepted. Date: 2026-11-28. Amends 0076 (an app's conversation keeps its work block open while work goes on).

## Context

The owner on 2026-11-27: lines like "Writing videos/censu-sales-demo/package.json and 1 more
step" clutter the chat; remove them. Every finished turn had one, folded over its tool rows,
and a running turn showed every row as it came.

## Decision

| State | What the chat shows |
|---|---|
| Running | The steps stay folded. The working line names the step that runs ("Writing notes.md"), then "Writing" while the answer streams |
| A step failed, was denied or was refused | At once, while the turn runs and after: one line naming the first such step. It opens the block's steps |
| Finished, nothing went wrong | No line. The block is hidden |
| A picture the Computer took | Under the folded steps, outside the block, so it stays in sight |
| Stopped | The "Stopped." notice, as before; a step the stop cut short gets no line |

A step's call details still open from the line of a step that went wrong. The rows stay in the
page, so an app that reopens a row (`carry`) still can.

## Alternatives

| Option | Why not |
|---|---|
| Keep the summary with shorter words | The owner asked for no line; the answer already says what was done |
| Hide the block during the turn too, with no live label | The person could not tell what runs for minutes (gulf of evaluation) |
| Show a line for a cut step | "Stopped." already says the turn ended early |

## Consequences

- Inspecting a successful call from the chat is gone. The trace keeps every call.
- `page.mjs` scenarios and `work-lines.test.mjs` hold the rule.
