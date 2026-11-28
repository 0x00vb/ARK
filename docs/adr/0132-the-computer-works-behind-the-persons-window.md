# 0132. The Computer works behind the person's window

Status: Accepted. Date: 2026-11-28. Amends 0018 (one screen, the person first), 0095 (native Computer on agent-desktop).

## Context

The owner on 2026-11-27: the Computer "should be able to use apps without stealing the windows
from me, like use apps while I continue using other ones". Every action brought the app to the
front (`launch_app` with `activate`, `focus_window` on `open`, `in_front` before keys, menus,
points and scrolls), control presses ran the adapter's headed policy (window focused, real
pointer), and every action first waited until the person stopped typing for 1.5 s.

agent-desktop already has a headless policy: accessibility actions, values set directly,
`AXSelectedText`, keys posted to the app's process. A physical step it cannot do headless is
reported `NotDelivered`, so nothing reached the app.

## Decision

| Step | Behind the person's window |
|---|---|
| `open` | Find the app's window, or start the app without activation; never focus it |
| `look`, `see` | As before: the window's accessibility tree, a picture of that window only |
| `click` on a control | The app's accessibility action (headless) |
| `type` | Set the field's value; if it does not take it, select-all and typing |
| `keys` | Posted to the app's process (headless): they reach what that app has focused, never the person's app. A key bound to a proved window (sending a message) always borrows, so it reaches only that window |
| `menu` | The menu bar's accessibility actions, as before |
| `point`, `scroll`, a control or key with no headless way | Borrow: wait until the person pauses (at most 2 s, else "try again when they pause"), bring the window forward (1 s), do the step (at most 3 s), give back the window they had in front and the pointer. The give-back runs on every path once focus was tried, and without the operation's authority, so a call that timed out still returns the person's window |

A fallback to the headed step happens only when the headless one reports `NotDelivered`, so no
press or key lands twice. A value that reached a field and read back wrong is reported, not typed
over. `Screen::in_background` tells the service: it then waits for nobody
before an action and asks for no app to be in front (`in_front` passes). A screen that is not
in the background (the fake one by default, another platform later) keeps the old rules.

An app in the background has no focused window. The window acted in is the one the agent last
looked at or opened while it exists, else the app's front-most visible one. The look names that
window. Before, the focused window was chosen, which for an app the person used was their own
document too; acting in the wrong document is no more likely than it was. One difference: with
several windows and none focused, the old code refused and the new one picks the front-most
visible window.

One borrowed step fits one native call (12 s): headless try 3 s, wait for a pause 2 s, focus
1 s, the step 3 s, the give-back 1.5 s.

## Alternatives

| Option | Why not |
|---|---|
| A second, virtual display for the agent | macOS has no public API for one; a private one breaks across releases |
| Keep headed actions and put the person's window back after each | Every step would flash the app forward and move the pointer |
| Refuse every step with no headless way | Points and scrolls would never work; the borrow waits for a pause instead |

## Consequences

- The person keeps typing in their own app while an agent fills a form elsewhere.
- Keys to an app with nothing focused (Calculator's Escape) need a pause: while the person keeps
  working they are refused with a reason the model can act on.
- Checked live on the owner's Mac on 2026-11-28 with `ARK_COMPUTER_LIVE=1` (Safari stayed in front): `the_native_screen_types_into_an_app_behind_the_persons_window`, `the_native_screen_presses_and_keys_an_app_behind_the_persons_window`. Without that variable both tests skip and pass, so CI does not prove this.
- Not checked live: a send to a messaging app in the background, menus of a background app.
