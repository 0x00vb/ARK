# 0060. A project Terminal does not need a chat

Status: Accepted (owner asked for implementation 2026-10-10; plan gate passed after two reviews)
Date: 2026-10-10
Amends: 0052 (a resource may be owned by a project instead of an opening chat)

## Context

The Terminal pane is a project shell, but its authority currently comes from the chat that
opened it. A person who only wants a shell must first open or create a chat. That changes the
wrong object and makes the Terminal look like part of a conversation.

ARK's window, ARK Desktop and a browser share the web client. The iPhone is a native client of
the external protocol. A manually started `ark-cli` is itself a chat client and does not present
project resources.

## Decision

A Terminal may have project authority. Opening one names an existing project and a computer. It
does not open, create or select a chat, read a transcript or send model input. The existing
chat-scoped resource path remains valid for panes an agent presents in a conversation.

The host issues an opaque, ephemeral route after it validates the source project and selected
computer. The route binds the source project, destination runtime, destination project replica,
resource kind and terminal id. A client cannot gain authority by supplying those fields. A
project route initially permits Terminal operations only.

| Place | Open |
|---|---|
| This computer | Validate the project and folder, issue a route, then open or attach to its project PTY |
| Another computer | Validate the live link, copy the project visibly, resolve its replica, issue a route, then open or attach there |

A remote refusal, failed or cancelled copy, missing capability, stale route or lost link leaves
no usable route and never falls back to this computer. A route is removed when its shell ends,
its project or link is removed, its credential is revoked, or the host restarts.

Each write has a monotonically increasing sequence within its route. Native clients use bounded
HTTPS `POST` writes, not the durable mutation queue and not a second persistent socket. A write is
at most 16 KiB, only one is in flight, and adjacent keystrokes may be coalesced for at most 12 ms.
The client advances its local sequence before sending. It never follows a redirect, retries after
an authentication challenge, cancellation or uncertain response, or accepts a late response for
another route generation. The host accepts the sequence once or refuses it. Disconnect disables
input at once. This keeps normal typing responsive without inventing an offline input queue.

Output uses the authenticated event stream. A terminal event carries the opaque route, terminal
generation, byte cursor, optional data, gap and ended fields. The client applies only the current
route and generation, ignores cursors it already applied, and performs a bounded read after a gap
or reconnect. Reads retain ADR 0052's generation and cursor replay. Resize is idempotent and may
be repeated after the viewport settles.

One PTY is shared by a project on a computer. Closing the pane only removes that view's
subscription. The shell ends when the person exits it or its process ends. The protocol retains
an explicit end operation for clients that already have a suitable artifact lifecycle action, but
the shared page adds no new control for it. Output stays in the Terminal. It never enters a chat,
transcript or model context.

### Interface

The shared page keeps one Terminal concept. The existing Terminal pane, its existing plus-menu
entry and its shortcut open the current project's shell through project authority. There is no
project-menu action, main-column Terminal view, new button or Terminal section. Opening the pane
leaves the current chat unchanged. A project without a folder shows the pane's existing error
state.

Opening captures the current project and the current chat's selected computer. It never falls
back to this computer after a remote failure. The pane is keyed by project, so chats in one
project do not create duplicate Terminal tabs. Its existing tab label names the captured project
and computer. Switching projects disposes the old pane before it can accept more input.

A native client must use an existing artifact presentation path for Terminal. It must not add a
Projects section, project page, Open Terminal button or Terminal destination only to reach it.
The iPhone does not yet have a generic interactive artifact path, so it retains the generated
protocol types and transport endpoints but ships no Terminal renderer or entry point. Renderer,
keyboard, accessibility and lifecycle decisions wait until that artifact path exists.

## Consequences

- A person can use a project's shell without chatting.
- The web client supplies the behavior to ARK's window, a browser and ARK Desktop. Shipping
  Desktop still requires an ARK release and consumer pin.
- Shipping iPhone support follows the released generated protocol. A local implementation is
  not described as shipped before that order is complete.
- `ark-cli` does not change because it does not present project resources.

## Check

- A project with no chats opens a local Terminal without changing the current chat.
- Forged project, runtime, kind and terminal ids, stale routes and generations, revoked links,
  duplicate writes and cross-project operations fail before I/O.
- Remote copy failure and cancellation create no route and do not execute locally.
- Page tests cover the existing entry points, one pane per project, captured placement, project
  switching, ordered input and the narrow layout.
- Native protocol tests keep the generated Terminal payloads and transport endpoints compatible.
- Old runtimes omit `project.terminal`; old clients ignore the additive protocol minor. New
  clients never show a nonfunctional Terminal destination against an old runtime.
