# 0112. One sidebar; + makes an agent; an agent has a look

Status: Accepted. Date: 2026-11-09. Amends 0110 (sidebar), 0103 (rail).

## Context

After 0.42.0 the owner asked for the window's left side to follow a reference screenshot: no
fixed rail, agent tiles centered, search and + at the top with + making an agent (not a chat),
the sidebar resized by dragging instead of a hide button, the app's buttons beside the profile at
the bottom, and an agent whose name and pet the person can change.

## Decision

| Part | Decision |
|---|---|
| Rail | Removed (`#destinations`, `destinations.js`). The window is sidebar, main, pane |
| Top | Round Search and + buttons, right-aligned. No hide button, no New chat row. New chat stays on ⌘N, `/new` and a tile's menu |
| + | Calls the app's `ark.newAgent({ open({ location }) })`. With no handler (ARK alone) it starts a chat |
| Tiles | Wrap and center: one tile is centered, two sit side by side in the middle, a row of three is full, a fourth starts a centered row |
| Foot | The profile (`ark.profile`: initial and name, else Settings) opens Settings; the app's `nav` views are round icon buttons beside it, on one line |
| Width | A grip on the trailing edge: drag 200 to 400 px, remembered; drag below 150 px hides the sidebar and keeps the width; double click resets; arrow keys move 16 px. ⌘B and the title bar's button show and hide it |
| Look | `AgentSpec.look`: a display string the app chooses (Desktop: a pet, `"arch-green"`). Opaque to ARK. It is in the graph's agent nodes and `agents {all}` rows, and goes to `ark.agentIdentity((name, look) => key)` |
| Name | The person's name for an agent is `AgentSpec.title`, as before. The internal `name` never changes, so delegation, mail and chats keep working |

## Alternatives

| Option | Result |
|---|---|
| Keep the pet in the app's browser storage | Lost on another window, device or restart; the phone could not show it |
| Native CSS `resize` on the sidebar | No hide gesture, no keyboard, a corner handle in WebKit |
| Rename the agent's internal `name` | Breaks delegation lists, mail and chat lookups that key on it |

## Consequences

- A Chat button no longer returns to the conversation from a view: an agent's tile does.
- Apps that drew into the rail with `nav` views now draw into the sidebar foot; the call is the same.
- An app that does not register `ark.newAgent` keeps + as New chat.

## Amendment (2026-11-09): delete from the sidebar

The owner asked to delete agents. A tile's menu lists the app's `where: "agent"` actions. Ending
an agent in another folder goes through two window calls, `agent_end` and `agent_restore`
(`{project, agent}`), on the host's runtime-wide entry like `agents {all}`. A first version let
chat-scoped resource calls name another `project`; review rejected it, because a chat reference
must act only on its own chat's folder (`resource.rs`, ADR 0052). Delete ends the agent (its
chats and records stay); Undo restores the same spec. Hard delete is cut.

## Amendment (2026-11-28): two columns, big faces, the last message

The owner on 2026-11-27, with a reference screenshot: the sidebar should read like it (two big
faces on top, then rows of face, name and the last message), and step lines clutter the chat.

| Part | Now |
|---|---|
| Tiles | Two columns, 64 px faces. ARK and the pins, in pin order; at least two (the most recent fill), at most eight. A third tile starts a centered row. Arrow keys move by a row of two |
| Rows | 40 px face, the name, one line of the agent's last message. No date; "Needs you" stays |
| Preview | `agents {all}` `preview` is the first line with words of the newest answer in the agent's latest chat (Markdown marks at the line start, code fences, `**` and backticks dropped), else the chat's title. One turn is read, four when the newest has no answer; a chat that does not read shows its title |
| Width | 272 px by default with a fine pointer (was 236), 288 px otherwise (was 252) |

Rejected: a date on every row (the order already says it, and it took the preview's space);
three columns of 64 px faces (names cut to a few letters at the default width).
