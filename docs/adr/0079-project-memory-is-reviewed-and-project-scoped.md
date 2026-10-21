# 0079. Project memory is reviewed and project scoped

Status: Proposed
Date: 2026-10-20

## Context

Working state, evidence and rules survive a task. They do not provide a reviewed lifecycle
for corrections reused by new chats. Agent Beacon shows the value of reviewing lessons with
provenance. ARK already owns SQLite records, shared commands and a bounded context builder.

## Decision

Use the existing records table. A memory has an id, project, topic, body, source, state,
optional predecessor and timestamps. Sources distinguish a person's session from an agent's
invocation. States are Pending, Active, Rejected, Superseded by an id, and Forgotten.
Only a person's shared command can activate, replace, reject or forget. An agent can suggest
pending memory through a project-bound tool. Tool inputs cannot choose another project.
The SDK offers that tool when active or pending records already exist, or the current task
explicitly requests remembering or learning a correction. Retired-only and ordinary empty
projects have no memory capability-index cost. Both requests and previews use this policy.
First use can also be the shared remember command. No raw-chat correction classifier runs.

All writes and state checks use one immediate SQLite transaction. Exact repeated proposals
are idempotent. Replacement activates the successor and retires the predecessor atomically.
A competing replacement fails rather than replacing a newer correction. The store rejects
unknown projects, foreign ids, invalid transitions and oversized content. Each project may
hold at most 512 active or pending records. Retired history does not consume this allowance;
it retains the existing store lifecycle. Exact repeated suggestions retain their first source.

`/memory` lists this project's memories. `remember topic | correction` saves a person's
correction. `show id` exposes source and text with Accept/Reject actions for pending items.
`replace id | correction` uses the original topic and `forget id` removes its content from
the memory record. Shared reports supply existing terminal and page rendering, including
empty, error and pending states, without new CSS, panels or navigation.

The harness retrieves only active lessons with deterministic word matching, at most five
and at most the smaller of 1,000 estimated tokens or one eighth of its request budget.
It counts the complete rendered section in ContextReport. `/context next message` previews
task-specific selection. A preview without a current or unfinished task has no task to match. Each reasoning request and forced
answer refreshes that section from canonical storage, replacing the previous section.
If memory changes during a provider retry, the harness rebuilds under its existing reasoning
step limit. Forced answers allow at most three snapshot refresh attempts.
Memory is cited background data, not authority. Current instructions and corrections win;
permissions never come from remembered text. Preview uses the same selection.

## Alternatives

An external memory backend duplicates state and adds a required service. Reusing rules for
learned content conflates instructions with lower-trust observations and lacks review state.
Keep rules for standards and private memory for reviewed project lessons. Add no embeddings,
graph database, skill exports or cross-project sharing in this slice.

## Consequences

No additional database or dependency. New public Rust data/methods and a serde-defaulted
ContextReport field are additive but downstream struct literals may need the new field.
Normal capture does not add a model call. Agent suggestions add one discoverable capability
only on explicit first use or in projects with active or pending memory.
Forgetting removes memory text, not copies already present in conversations, invocation logs,
outputs or backups. Those retain their existing lifecycle. An in-flight model request cannot
be recalled; a later request refreshes the canonical memory section.

## Check

Store restart, scope, idempotency, concurrent replacement, forgotten content and size limits.
Runtime request inspection, bounded selection and refresh. Shared commands and fresh chats.
A paired correction benchmark distinguishes deterministic behavior from live-model outcomes.
