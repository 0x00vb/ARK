# 0094: A resource can expand without owning a conversation

Status: Selected implementation. Local Core CI and consumer qualification passed. Unreleased.

Date: 2026-11-02

## Problem

A project document can need the whole work area. Keeping its editor beside an empty chat
wastes that area. Creating another composer inside the editor would duplicate routing,
drafts, uploads, approvals and Stop. It could also imply that one agent owns the document.
The current project-panel API incorrectly captures the opening session even though the tab
identity is project-scoped.

## Decision

A shipped panel can opt into `expandable`. Core owns Expand and Restore in the pane header.
Expansion gives the resource the main work area and reparents the active conversation's
existing surface into a small floating dock. The conversation title identifies the recipient.
The dock can reveal the existing history and tool details. The existing global approval tray
remains visible. No Composer, conversation, task or agent is created by expansion.

The exact original DOM nodes return to their positions on Restore or close. Drafts, handlers,
upload origins, queue state and Stop stay with those nodes. Switching conversations changes
the dock while retaining the project document. Project or runtime changes exit expansion.
Async close guards retain their existing authority and may refuse closing.

A project-scoped panel receives an API with no captured session. Its selection goes to the
currently active chat only when the runtime and project match. Calls retain the existing
same-project opening-chat authorization; the project identity does not grant new authority. Expanded resource focus takes
precedence over the floating conversation's own view focus. Document ownership, revision
checks and agent proposal approval stay in the consumer.

## Boundaries

| Core | Consumer |
| --- | --- |
| Pane expansion, dock placement, active conversation and restoration | Design document, selection, commands, comparisons and proposals |
| Existing send, uploads, approval and Stop behavior | Which first-party panel opts into expansion |
| Runtime/project focus confinement | Multiple agents proposing edits to the same project document |

A floating composer is a presentation of the current conversation. It does not promise that
all orchestrated agent messages already receive the selected-resource context. That path must
be verified with actual task submission and proposal review before V1 acceptance.

## Required evidence

Native and headless checks must cover the same input element and draft through Expand and
Restore, recipient switching, history and tool review, approval visibility, Stop, upload
origins, asynchronous close refusal, project/runtime changes, keyboard, narrow layout and
repeated lifecycle cleanup. No release or full V1 claim follows from source review alone.

An agent conversation supplies an explicit surface, recipient location and focus setter.
The shell does not infer recipient identity from arbitrary view DOM. Switching recipients
clears the outgoing selection. The existing task context carries a frozen selection for a
new task. Existing messages accept an optional context, bounded to 8 KiB UTF-8 per message
and 64 KiB pending per recipient. Typed steering and durable inbox records preserve it
through late delivery and restart. Visible message text stays separate. Context is data,
never a permission or a way around revision, approval or Stop checks.

## Qualification

Final current-source Core scripts/ci.sh passed on3November2026. The consumer exercised
Expand/Restore using the same composer, native agent presentation, human correction and
proposal Keep/Undo. Source-bound regression and lifecycle evidence is recorded in
../ark-desktop-design-studio/experiments/design-studio/RESULTS.md. This does not authorize
a release or claim that every orchestrated agent automatically receives selected context.

The floating surface starts as a compact bar. Click expands the existing composer and focuses
its input; Minimize preserves the draft. Its header can move by pointer drag or Alt+arrow keys.
Both dock and pane resize are observed for bounds. Running turns and approval attention keep
the necessary controls visible. Restore aborts added input handlers and disconnects observers.
