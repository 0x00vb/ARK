# 0018. The Computer: one screen, one agent, every action asked

Status: Accepted, amended by 0042 (rows and items, menus, scrolling, pictures)
Date: 2026-10-04

## Context
ROADMAP capability C5: agents use the apps on the person's computer. Unlike a browser context,
the screen cannot be split. There is one keyboard focus and one front window, and the person
uses them too. Several agents and background chats may want it at once (ADR 0017). Reading
another app's window can expose anything on screen (mail, messages, a password manager), not
only what an agent changes.

Two ways to see an app were open:
- **Screenshots** with clicks at coordinates. This needs a model that reads images, and none
  of ARK's providers send images today, including the subscription CLIs and local models.
- **The operating system's accessibility tree.** This is text, the way ADR 0015 shows a page:
  the window's text and a numbered list of its controls. It works with every model ARK runs on
  and costs far fewer tokens than an image.

## Decision
A crate, `ark-computer`, with the same shape as the Browser: a `PerAgent` source
(`Computers`) over one `ComputerService` per process, and a `Screen` trait for the platform.

| Question | Decision |
|---|---|
| What an agent sees | `computer.look {app}`: the front window's text and its controls (button, text field, checkbox, link, …), each numbered. `computer.apps` lists the open apps |
| What it does | `open {app}`, `click {app, ref, name}`, `type {app, ref, name, text, submit?}`, `keys {app, keys}` (`cmd+s`). Each answers with the window after, so the agent sees what happened |
| What asks | Everything but `apps` is `Mutate`, through the approval gate, and `ask_each_time`: Allow once or Deny only. No "for this session" or "always" answer covers a later call, so nothing on the screen ever runs unattended (a scheduled run at 3 a.m. waits for a person). Reading a window asks too, because it can show anything |
| What the person approves | The control by name: "Clicking “Send” in Mail", "Typing “Plan” into “Title” in Notes" (`ark_interact::labels`). The tool refuses when the name is not the one at that number in the agent's last look, so the approval says what is pressed, and a window that changed since is caught |
| Keys go where they were approved | Text a field will not take directly, Return, and shortcuts go to the app in front. They are sent only after the approved app is brought forward and confirmed in front; otherwise nothing is typed and the agent is told why |
| Who uses the screen | One owner at a time (a chat `session:<id>` or an agent `<project>/<agent>`). The first to act holds it. Others wait until its turn or task ends (the owner's tools are dropped and give it back) or it has been idle for 30 s (`IDLE`) |
| The person first | Before every action the agent waits until the keyboard and mouse have been left alone for 1.5 s (`QUIET`). The process's own keys don't count |
| The person in control | While an agent holds the screen, the window shows "‹who› is using your computer · Stop" (`computer` event, `computer_stop`). Stop takes the screen back. That owner's computer actions fail until its turn or task ends, with words that tell it to stop and say what is left. The rest of its task goes on |
| Element numbers | They belong to the owner's last look, in that look's app. A number for another app, a stale number or a look by another owner is refused |
| Permission | macOS's Accessibility permission, once. The first action asks the system to show its prompt and waits up to two minutes for the person to allow ARK, then goes on. Nothing to set up beforehand |
| Turning it on | `ArkBuilder::computer(true)`. A catalog capability "Computer: use the apps on this computer, one agent at a time". Only macOS has a `Screen` today; elsewhere it is not offered |

## Consequences
- Any model can use apps. One look is a few hundred to a few thousand tokens, clipped the way
  the Browser's are (the card shows 40 controls and 800 characters, the rest through
  `get_evidence`).
- Apps that expose little to accessibility (some games, canvases, remote desktops) show little
  and can't be driven this way. Screenshots and coordinates are a later step, for models that
  read images.
- An agent holding the screen moves windows to the front. Agents and the person share one
  front window: quiet-waiting avoids typing over the person, but it doesn't stop an agent from
  bringing its app forward between the person's keystrokes. Stop is the remedy.
- Every screen action is one question, so a long task on the screen asks often. That is the
  price of never acting unattended; grants per app and per action can come later, with a way to
  bound them in time.
- Not in the bench: no workload has a screen. Tested against a fake screen, and read-only
  against the real one.

## Check
- `crates/ark-computer/tests/computer.rs`: acting by number and name (a wrong name refused),
  keys only to the approved app in front, one agent at a time with the next waiting, an idle holder giving way, the person first and taking it back, permission asked once
  then the action going on. The real screen: the open apps listed, and with
  `ARK_COMPUTER_LIVE=1` the front window read and counted, never printed.
- `crates/ark-runtime/src/approval.rs`, `an_action_that_asks_each_time_is_never_covered_or_remembered`.
- `crates/ark-ui/tests/page.rs` (WebKit), `the_person_sees_who_uses_the_computer_and_takes_it_back`
  (Allow once and Deny only).
- `crates/ark-interact/src/labels.rs`, `scheduling_and_the_computer_read_naturally`.
