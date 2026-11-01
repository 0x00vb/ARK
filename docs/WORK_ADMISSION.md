P0 persistence design

Store::transact_records runs one deterministic closure under BEGIN IMMEDIATE. RecordTransaction exposes scoped get/list/put/insert/delete and session creation, with no raw connection. Read validation and mutation happen in one transaction. Memory, events and execution start happen after commit.

Compared designs:
1. CAS batches plus Board revision fences. Correct but every writer must maintain fences, legacy encoding must remain exact, and membership scans need extra ordering.
2. Scoped transaction closure. Chosen: no phantom membership scans, no Board API field, same atomic mechanism for WorkItem mutations, admission and event quotas.

Do not call Store again or run model, process, network or filesystem effects inside the closure. Failed closure or commit publishes no state. Existing serialized records remain compatible. Busy writer lock is cooperative across current runtime processes.

## Contract evidence, 1 November 2026

P0 implementation evidence is separate from release acceptance. The owner made the two-sprint
limit a planning target. Work continues until the remaining gates pass.

| Contract | Implemented behavior | Executable evidence |
|---|---|---|
| Receipt and conversation | Admission commits one immutable receipt, WorkItem and private conversation. Duplicate requests converge. Different work for one agent has different contexts. | `work_admission_is_atomic_scoped_and_keeps_complete_request_identity`; `work_contexts_isolate_state_and_explicit_files_and_survive_reopen_and_compaction` |
| Work retention | Scheduled occurrences keep their Work and conversation after history pagination. Cancellation and failure resolution name exact executions. | `tests/schedules.rs`; Host `another_host_reads_each_scheduled_conversation_and_stop_only_cancels_the_selected_one` |
| Schedule destination | The existing ScheduleRecord owns time. Activation switches to an immutable revision with a cutover watermark. Explicit legacy conversion freezes policies. Source edits refuse stale activation; cursor progress remains valid. | `activation_cutover_rejects_an_occurrence_at_the_exact_watermark`; `legacy_cutover_refuses_changed_configuration_but_allows_cursor_progress`; `legacy_policy_refresh_is_explicit_revisioned_and_cannot_replace_an_active_source` |
| Writer and publication | Scoped immediate transactions validate and mutate together. Result journals and outbox records precede publication. Delivery filters due work before applying a bound. | `a_saved_answer_repairs_its_conversation_without_calling_the_provider_again`; `result_delivery_selects_due_records_before_limiting_the_queue` |
| Transitive authority | Frozen grants, descriptors and shared quotas cover steps and delegated tasks. Pending approvals retain the exact invocation. Unconfirmed changes stop continuation. | `pending_automation_approval_survives_process_exit_for_general_and_agent`; authority module tests; delegated execution tests in `conversation_tests.rs` |

The focused orchestration library has97 passing unit cases. Runtime has125. Actual Desktop
serve mode completed an eight-step Claude sequence with three approvals and a process restart,
a due schedule, and a signed local webhook using an existing agent. These are recorded in
`../../board/evidence/ux-implementation-p0-20261031/`.

Full source-bound gates, the two final review iterations and release verification remain
separate requirements. The local webhook run does not establish public HTTPS reachability.

## Reusing an admitted request

Save as automation reads a scoped WorkTemplate from the original submission receipt. It copies
only the objective, explicitly supplied artifact references and selected responsible agent.
Conversation history, model answers, action grants and private context do not become permanent
instructions. The draft remains manual until the person configures and activates it. A legacy
item without an original submission does not offer a guessed template.

Work details expose per-execution usage from saved task and conversation records. Token
estimates retain their approximate marker. A reported monetary cost appears only when its
execution reported one; missing billing is shown as unavailable. Rows are not summed across
parent and child executions.

## Shared empty-state composer

A consumer can provide `ark.newChat({ open, render })`. The optional render handler receives
its captured project location and scoped API, and returns the usual view control. Work uses
that same composer when its unfiltered main list is empty. Consumers without render retain
the New chat entry button.

An optional `isComplete()` control returns true only after a successful send, with no pending
text or uploads. On returning to an empty list, Work replaces only that completed composer.
An orchestration refresh keeps an unfinished composer mounted, including its focus and text
selection. A cached hidden composer may complete its original admission but cannot replace
the person's current view.
