# 0094: Work conversations and automation sequences

Status: Accepted product direction. Implementation in progress.
Date: 2026-11-01.

The owner approved this direction in the implementation brief. The two-sprint estimate is a planning target. It does not limit implementation or waive a quality gate.

## Work

New chat opens a local draft. It creates no agent, task or conversation. General is the default. The person can select an existing agent.

Sending admits an immutable request under a project, origin and request key. Admission commits its receipt, WorkItem, bound conversation and optional agent task together. A duplicate recovers the same receipt. A changed request under the same key is refused. Execution starts after admission and uses the frozen model and agent choices.

A WorkItem owns its conversation. Followups use that WorkItem and conversation. An agent's permanent conversation remains separate. General runs the existing conversation loop and creates no agent. Work projects results, states and questions from the same durable records. Archiving is personal. Resolving a failure names its execution and current digest; it does not change history or run the work again.

The store admits one cooperative writer process. Scoped immediate transactions protect validation and mutation. Per-execution ownership prevents a second orchestrator from repeating live work. Reading another execution is observation. A begun turn with uncertain effects is not automatically retried. Saved answers are restored before another model call.

## Automations

Routines, schedules and workflows become one automation library. List and Agenda are views. Steps and Canvas edit the same definition revision. Canvas positions do not change execution semantics.

The first version is a sequence of at most eight steps. Steps run an agent, request approval, or retain a result. Bindings reference earlier typed outputs. Conditions, branches and arbitrary graphs are outside this version. Each execution freezes its revision and inputs, owns a conversation, and appears in Work under the same execution identity.

Manual, schedule and webhook are trigger variants of the same definition. The existing scheduling calculations remain authoritative for time zones and daylight saving. Manual runs do not move the next scheduled occurrence. A webhook verifies authentication, size and replay identity before admission. No source-specific messaging integration is required for this version.

A0 and A1 are sources for selective reuse, not an accepted merge. Reuse the store writer, provider-neutral event contracts and per-call guard where they fit. Event admission and quotas must fail closed and use durable transactions. Authority applies to descendants. A saved definition does not grant unattended effects. Changes to its authority require a new activation approval for future admissions. Existing runs keep their frozen grant until explicit revocation. ScheduleRecord remains authoritative; reading a legacy schedule never migrates it.

## Release gates

Keep existing clients able to read conversations, results and approvals. An unsupported editing operation fails explicitly. Do not substitute an execution program or silently broaden authority.

Use independent plan, design and implementation QA. Test storage failures, concurrent admission, restart, cancellation, uncertain effects, project boundaries and real app interaction. Synthetic providers establish deterministic contracts. They do not establish real execution or product acceptance. Core releases before the consumer pin changes.
