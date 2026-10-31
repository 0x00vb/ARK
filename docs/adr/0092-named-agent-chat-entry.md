# 0092. New chat chooses a durable conversation identity

Status: Accepted for implementation
Date: 2026-10-31

## Context

The person could create a named agent only from Agents, while New chat always created a
separate general conversation. Agents could create colleagues but could not select their
model or reasoning. This left the primary entry inconsistent with the intended chat flow.

## Decision

The shared shell accepts one New chat handler from its consumer. Sidebar actions, project
menus, the command palette, keyboard shortcuts and the page's /new command use that entry.
Without a handler, Core keeps its general conversation behavior. Internal last-tab recovery
creates a general conversation directly, without asking for a new destination.

Desktop chooses General chat, an existing project agent or a new named agent. A new identity
has an optional role and model selection. Reasoning uses the existing model picker's effort
choices. Opening or creating an identity sends no message and starts no task. Existing
agent records, conversations and sidebar entries remain the authorities.

The new CreateAgent resource operation uses the existing project authorization boundary.
Older runtimes refuse this operation rather than interpreting it as an edit. A stable namespaced
key identifies the resource origin and request. An atomic receipt separately binds the
canonical spec digest and resulting agent. Creation is atomic,
refuses unrelated duplicate names and returns an exact retry without overwriting later edits.
Native creation provenance and the existing SpawnAgent editing operation are unchanged.

Agent tools accept optional model and reasoning strings. A colleague snapshots its active
creating task's resolved model, including account and effort. An explicit model uses its own
default effort; reasoning alone changes the inherited model's effort. Ambiguous or malformed
choices are refused. General chat tools use their orchestrator's default when model is omitted.
The existing provider:model@effort record carries the selection; there is no migration.

A new explicit selection passes the resolver's admission policy before publication. The
standard resolver checks current account readiness, advertised model and supported effort.
Custom resolvers must opt into explicit selection validation. Resolution remains the final
execution authority and never falls back to another provider account. Permission inheritance,
creation quotas, cancellation and independent conversation boundaries remain unchanged.

## Alternatives

| Option | Result |
| --- | --- |
| Create a blank chat, then convert it into an agent | Leaves an unused identity and ambiguous history. Rejected. |
| Keep creation in Agents and add explanatory copy | Does not make the primary action useful. Rejected. |
| Choose the destination before any persistent creation | Reuses durable identities without migration. Chosen. |

## Verification

Rust tests cover optional input semantics, account and effort inheritance, retry conflicts,
later edits, unavailable selections and project-scoped atomic creation. Actual WebKit tests
exercise creation, existing conversations, cancellation, keyboard access, retained error
inputs and stale-origin refusal. Workspace evidence records full checks and native dogfood.
