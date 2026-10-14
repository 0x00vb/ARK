# 0068. Tabs form a spatial window layout

Status: Accepted
Date: 2026-10-14

## Context

ADR 0063 lets one window show two chats beside each other. The split button does not map to the
place a person wants the chat to occupy, and replacing the second chat prevents comparison layouts
such as one large chat beside two stacked chats. A tab already represents the chat. Moving that
object is a clearer control than adding layout menus.

Dragging a tab outside the window crosses a native boundary. ARK's legacy foreground session is
owned by the host, not by a native window. The session-addressed `chat_*` calls already let a client
read and operate one chat without changing that foreground. A detached window can use those calls
as its ownership boundary. The ambient window remains one presentation of the main window, not an
independent home for a detached chat.

## Decision

The window stores its visible chat arrangement as a binary tree of horizontal and vertical splits.
A person drags a tab onto the left, right, top or bottom edge of a visible chat to place it there.
The target appears only during the drag and previews the resulting half before the drop. Dragging
onto another tab reorders the tab rail. Dividers resize adjoining regions and keep their ratio.

The tree may nest in either direction, with at most four visible chats. Extra open chats remain in
the tab rail. Four is a constraint, not a runtime limit: it keeps a chat and composer usable in the
minimum supported window while bounding transcripts, approvals and live event surfaces. A narrow
window shows only the active leaf and keeps the full layout for when room returns.

Every visible leaf owns its transcript, running turn, queue, approvals, draft and scroll as in ADR
0063. A drop may only combine chats from the same project and runtime. Invalid and interrupted drops
change nothing. The selected chat and drop preview provide immediate feedback. Reduced motion uses
the same state changes without spatial transitions.

The layout and divider ratios are local to a native window identity. Restore discards missing,
duplicate or invalid leaves and always includes that window's foreground chat. Closing a tab
collapses its empty parent.

In a native app, releasing a tab beyond the window opens that chat in a full ARK window near the
release point. The new page addresses its own foreground chat through `chat_*`; changing chats or
projects there does not change the original window. One chat has at most one detached window, so a
second drag brings the existing one forward. The source removes its tab only after native window
creation succeeds. A failed or interrupted detach changes nothing. A browser has no detach action.

The detached window inherits the consumer's configured native material, title bar, size constraints
and minimum size. It has its own saved tabs, split tree and Browser child view. Closing its last tab
closes that native window. It never changes the main window into its ambient presentation.

## Consequences

- The split button remains as a keyboard-accessible shortcut. It adds the most recent hidden tab to
  the right of the active chat, or closes the other visible chats when the layout is already split.
- The visual target is the pane edge itself. There is no permanent layout toolbar or explanatory
  copy.
- The two-pane local-storage value is read only for migration. New state uses the layout tree.
- Runtime-wide state still exists for compatibility with one-window clients. Detached windows use
  addressed chat operations and window-local presentation state instead of mutating it.

## Check

- Tabs reorder without changing the selected chat or stopping work.
- Edge drops produce left, right, top and bottom splits, including nested layouts.
- Four chats can remain live; a fifth drop is refused without changing the layout.
- Dividers resize, persist and remain operable with pointer or arrow keys.
- Closing any leaf collapses the tree. A reload restores valid leaves, drafts and scroll positions.
- Narrow width and reduced motion have page coverage. An invalid cross-project drop changes
  nothing.
- A detached page opens and changes its own chat without moving the runtime-wide foreground. Its
  saved layout cannot replace the main window's layout.
- Native creation validates the chat before opening. A repeated detach focuses the existing window,
  and a native failure leaves the source tab and layout intact.
