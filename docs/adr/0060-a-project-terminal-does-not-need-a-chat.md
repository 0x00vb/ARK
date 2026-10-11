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

One PTY is shared by a project on a computer. Closing or leaving a view only removes that view's
subscription. **End Terminal** ends the shared PTY for every attached view and always confirms
with the project and computer names. Output stays in the Terminal. It never enters a chat,
transcript or model context.

### Interface

The authoritative project list includes projects with no chats. In the shared page, `Open
Terminal` is a text action in each eligible project's menu. On iPhone, Chats begins with Projects;
a project row opens a project page whose first action is `Open Terminal`, followed by that
project's chats. It opens a full-screen project view. The current chat remains unchanged. The
command palette offers it only when it has a current project with a folder. On Desktop or the web
page running on that computer, a project without a folder offers `Choose folder`. On iPhone it is
not a button: Terminal stays unavailable and says `Choose a folder in ARK on <computer>`.

The view shows the bound project, computer and connection state before input is enabled. One
eligible computer is selected before connecting. Several require a choice before any copy or
PTY starts. Unsupported and unavailable computers remain visible with their state but cannot be
chosen. Controls precede the terminal in keyboard order: Computer, End Terminal, then the
terminal. The terminal is the main surface.

Connecting, copying, disconnected, refused, cancelled, stale, ended and output-gap states are
visible and recoverable. Output remains readable when input is disabled. Changing project or
computer disables input before detaching the old view. A deleted project becomes a read-only
ended view with a Close action.

The iPhone uses the same state model after the generated protocol is released. Computer choice
is a sheet and Terminal is a full-screen project destination. No projects shows `No projects`.
No eligible computers shows the computers already known to the runtime with their reason and
`Add or update computers in ARK Desktop`. A missing `project.terminal` capability hides Open
Terminal and shows `Update this computer`. Removing the selected computer or project immediately
makes the view read only. A deleted project remains readable with Close. Ended and stale shells
offer Reopen.

The native renderer is SwiftTerm 1.99.0, pinned by exact tag under its MIT license. ARK uses its
UIKit view inside SwiftUI, never its sample SSH app. It supplies ANSI, 256 and true colour, UTF-8
and grapheme rendering, selection, paste, hyperlinks, resizing and VoiceOver. The app caps local
raw replay at 256 KiB to match the runtime, configures rendered scrollback to 5,000 lines and
disables recording. Terminal bytes are memory-only: they do not enter logs, analytics, crash
metadata, restoration state or caches. User-invoked Copy intentionally places only the selected
text on the system clipboard.

Tapping the terminal opens the software keyboard. An input accessory has Keyboard, Esc, Ctrl,
Tab and arrow controls. Ctrl is a one-shot modifier with visible selected state. Keyboard dismiss
leaves output scroll and selection available. Paste uses the system edit menu and sends only after
the person invokes Paste. A two-finger drag scrolls output without input. The same selection,
copy, scrolling and keyboard dismissal remain available while the route is read only.

Hardware commands that the terminal never consumes are Command-Option-C for Computer,
Command-Option-E for End Terminal and Command-Option-Left Arrow for Back. They appear in the
system shortcut overlay. A hosting controller intercepts those three `UIKeyCommand` values before
the SwiftTerm view forwards input. A physical-keyboard spike must prove every chord reaches ARK
while the terminal is first responder before the dependency is accepted. End Terminal has a
destructive accessibility trait and confirms the project and computer.

On inactive, background, lock, network loss, project switch or computer switch, the app disables
input synchronously before covering, refreshing, copying or navigating. Returning revalidates the
route, generation and byte cursor before input returns. The grid stays inside safe areas and the
space above the keyboard. Rotation and keyboard changes send one idempotent resize after layout
settles. Chrome uses Dynamic Type. Terminal cells have an independent, grid-safe text-size control.
Long project and computer names truncate visually but remain complete to VoiceOver.

ARK defines tested light and dark terminal palettes for ANSI colours, foreground, background,
cursor, selection, links and read-only overlay. Streaming output is not announced line by line.
The terminal has one stable accessibility label, exposes readable selected and line content, and
announces only connection, gap and ended state changes. Computer rows expose their full name,
availability, disabled reason and selection state. No native client hand-writes a parallel wire
model.

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
- Page tests cover keyboard and narrow layouts, long names, dark mode and every connection state.
- Native tests cover backgrounding, lock, network loss, external keyboard input, Dynamic Type,
  safe areas, project switching and write non-retry after the generated protocol is released.
- Physical-device checks cover VoiceOver reading, software-keyboard resize, all three hardware
  shortcuts and background-to-foreground revalidation; simulator results do not close those gates.
- Old runtimes omit `project.terminal`; old clients ignore the additive protocol minor. New
  clients never show a nonfunctional Terminal destination against an old runtime.
