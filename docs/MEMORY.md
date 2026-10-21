# Project memory

Project memory holds reviewed corrections that apply to later work in the same project.
Working state still holds the current task. Rules still hold instructions and standards.
No external service, model evaluator, embedding database or account is required for memory.

## Correct and review

| Action | Shared command |
|---|---|
| List active memories and suggestions | `/memory` |
| Remember a correction you wrote | `/memory remember topic \| correction` |
| Read the text, origin and review actions | `/memory show id` |
| Accept a suggestion | `/memory accept id` |
| Reject a suggestion | `/memory reject id` |
| Replace an active correction | `/memory replace id \| new correction` |
| Remove a record's topic and text | `/memory forget id` |
| Preview recall for a next message | `/context next message` |

The examples above are command syntax, not captured output. A topic says when the correction
applies. Replacement keeps that topic and names the record it replaces. Only one active
memory may have that topic. A competing replacement must be reviewed against the current
record. Repeating the same suggestion preserves the first origin and does not create another
pending item. Remembering identical pending text accepts that item with your chat as reviewer.

The model can discover `memory.suggest` after the project has an active or pending lesson,
or when the current task explicitly says remember, memorize, learn from, or save a correction.
These are word matches, not automatic correction detection. Ordinary empty projects do not
advertise a memory tool. Retired-only projects need a new explicit request to offer it again.
The tool uses the existing approval gate to store a pending lesson. Its result links to the shared review report. It cannot accept, forget or
replace active guidance by itself. A suggestion may name an active predecessor, but it does
not retire it until you accept. Use the same shared commands in a terminal or page.

## Recall and stale guidance

Every reasoning request selects relevant active memory for the exact project ID. A new chat
in that project can reuse it. Independent projects, including agent child projects, do not
inherit memory automatically. There is no global profile or silent cross-project sharing.

Selection is deterministic word overlap, with higher weight on the topic. It returns at most
five complete lessons under the smaller of 1,000 estimated tokens or one eighth of the request
budget. This includes the section heading, trust note, citation IDs and JSON escaping. A
lesson that cannot fit stays out. No matching lesson adds no memory text.

The request identifies each lesson as `[memory:id]`. `/context` reports Project memory
separately. `/context next message` previews selection without running a model. A preview without a current or unfinished task cannot choose task-specific memory. The memory block is background data in the first
user message, below current instructions in authority. It never changes permission rules.

The harness rebuilds the canonical memory section before reasoning and forced-answer calls.
If a provider waits to retry while memory changes, the old retry is abandoned and the next
bounded attempt rebuilds it. A continuously changing project can exhaust the existing step
limit or three final-answer refresh attempts and return an explicit error. An in-flight
request cannot be recalled. Text already copied into an answer, another message or a tool
result is not retroactively removed.

## Persistence and limits

Memory lives in the existing SQLite records table under project scope. One immediate
transaction validates scope and review state, activates a replacement and retires its
predecessor. Sources name an existing same-project chat or invocation. No raw conversation
is automatically mined or copied into memory.

Topics are one line, at most 128 UTF-8 bytes. Corrections contain at most 2,000 bytes.
A project can hold 512 active or pending memories. Rejection, replacement and forgetting free
that allowance. Retired records remain available by exact ID and keep the store's existing
retention policy. Scope and transition failures do not overwrite the person's previous text.

Forget clears a memory record's topic and body and removes it from future canonical recall.
It does not delete existing conversations, invocation inputs, output files, backups or forensic
copies of SQLite pages. This feature is not secure erasure. No search-index copy or exported
skill is created by this slice.

## Verification

Store tests: `crates/ark-store/tests/project_memory.rs`.
Request and approval tests: `crates/ark-runtime/tests/project_memory.rs`.
Shared command and fresh-chat tests: `crates/ark-interact/tests/interaction.rs`.

The `memory_compare` runtime example is a one-shot routing and retention probe. It does
not prove completed-task quality. The SDK `memory_tasks` example runs paired coding and
report-writing tasks through the real Files adapter and configured provider. Both arms get
identical files, task, searchable correction history, permissions and budgets. Only the
memory arm has an accepted lesson. Each arm has a fresh store and standalone provider
requests. The validator checks saved artifacts without feeding repairs to the model.

On 2026-10-20, Claude Haiku completed eight runs, with 48 model calls:

| Task | Baseline correct / calls | Memory correct / calls |
|---|---|---|
| Invoice implementation | yes / 6 | yes / 7 |
| Invoice after correction replacement | yes / 8 | no / 9 |
| Sales report | yes / 5 | yes / 4 |
| Sales report with current table override | yes / 5 | yes / 4 |

Both document tasks saved correct reports with one fewer call. The replacement coding run
recalled the current rule but exhausted its eight tool steps before saving its implementation.
It remains a failed task. Overall correctness was 4/4 without memory and 3/4 with memory.
Total calls tied at 24 per condition. This small sample supports a narrow document-task
benefit, not a general reliability or cost improvement. No outgoing request contained the foreign-project marker. Canonical memory sections
excluded the retired memory ID. Both arms could still read the old rule in the historical
file. Current task instructions overrode the remembered default format.

Evidence is retained in the workspace at
`board/evidence/project-memory-2026-10-20/live-haiku-1/`: paired fixture hashes, actual
requests, token counts, elapsed time, saved artifacts, validation output and incremental
`results.json`. The grader has positive and negative checks. Python validation runs in the
native execution sandbox with a deadline, CPU and file size limits and a sampled RSS guard;
it does not claim a hard total-memory limit. These are synthetic local fixtures, not private
customer projects or a statistical success-rate estimate.

Use the fleet's absolute `ark-env` wrapper for builds and runs. The repository gate includes
the simulated benchmark against the committed baseline. Deterministic retrieval checks alone
do not establish lower cost or higher completed-task success.

Full `scripts/ci.sh` passed on this candidate, including workspace and documentation tests,
release build and all 22 simulated workloads with exact committed-baseline parity. The SDK
checks ordinary empty-project index parity and explicit first use through session requests.
Actual terminal checks covered persistence across processes, replacement, review, isolation,
preview and forget. Actual page checks covered remember, replaced state and the link to the
current correction. Screenshots and command captures are retained with the evidence.

Current validation status is recorded in `CURRENT_STATE.md`. ADR 0079 owns the boundary.
