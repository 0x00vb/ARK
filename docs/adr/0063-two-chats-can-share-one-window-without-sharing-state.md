# 0063. Two chats can share one window without sharing state

Status: Proposed
Date: 2026-10-11

## Context

ARK already keeps several conversations running (ADR 0017), but a connected page has one
foreground chat. `open_session` replaces that chat, and the page keeps one transcript, composer,
scroll position, approval stack and resource location. Drawing two copies of the current page
would mix authority and state even if the columns looked separate.

People need to compare results, watch one task while writing to another and keep related work in
view. Tabs alone improve switching. A two-pane split improves comparison. Neither should change
where work runs or let an action land in the wrong project copy.

## Decision

Add session-scoped conversation operations and events before adding split view. A window may keep
many chat tabs open and show at most two at once. Each visible pane owns one complete chat surface.
The same chat cannot appear twice in one window.

| State | Scope |
|---|---|
| Transcript, running turn, queued messages, approvals, draft and scroll | chat pane |
| Resource location, project copy and computer | chat, unchanged from ADR 0052 |
| Agents, Board and Automations | project |
| Files, Browser, Terminal and inspectors | one shared side pane, opened from the active chat |
| Default model | current app choice until a separate decision changes ADR 0017; a running turn keeps its starting model |

The protocol gains bounded chat snapshot, submit, steer and cancel operations that take a session
resource reference. Existing foreground operations remain compatible. Events already name their
session and are routed to the matching pane. Opening a chat that is already visible focuses its
pane. Closing a tab never stops its work. A working chat stays in the project list and can be
reopened.

Tabs are grouped by project. A split initially accepts two chats from the same project and
computer. Cross-project or cross-computer splits wait for evidence that the extra location
signifiers do not create mode errors. Narrow windows keep tabs but collapse to one visible pane.

Drafts and scroll positions are window UI state. They are saved locally by runtime and session,
never sent to the runtime until submitted. A crash restores open tabs and drafts. Missing or
revoked chats reopen as a recoverable placeholder without discarding the draft.

## Consequences

- Split view is not implemented by calling `open_session` back and forth. That would race events,
  approvals and state-changing calls.
- At most two visible chats limits memory, duplicated controls and small-window failure. Extra
  chats stay as tabs.
- The active pane is always visible through focus, toolbar selection and side-pane origin. Keyboard
  movement changes focus without animation. Pointer-created splits use one short, interruptible
  spatial transition and have a reduced-motion cross-fade.
- Closing, switching and splitting are safe during a turn. They do not cancel, retry or move work.
- ARK keeps no Git, pull request or product navigation type. A consumer can add those project
  surfaces without changing this protocol.

## Check

- Two chats run and receive interleaved events without either transcript, approval or queued
  message entering the other pane.
- A local chat and a copied remote chat cannot be combined in the first split implementation.
- Closing a running tab leaves the turn running and reopening shows its complete state.
- A crash restores two tabs, both drafts and their scroll positions without submitting either.
- Compact width, keyboard-only use, reduced motion and a disconnected runtime have explicit page
  tests.
