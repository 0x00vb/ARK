# 0042. The Computer sees pictures, uses menus and scrolls

Status: Accepted (amends 0018)
Date: 2026-10-06

## Context
ADR 0018 gave agents an app's window as text and numbered controls, through macOS
accessibility. A live run on real apps (2026-10-06: WhatsApp, Finder, Notes, Safari, Chrome,
ChatGPT, Spotify) found these limits:

| Found | Effect |
|---|---|
| Catalyst apps (WhatsApp) keep text in `AXDescription`, not `AXValue` | The look showed no text |
| Rows, list items and pressable text were not controls | An agent could not open a file in Finder or pick a sidebar entry |
| Every control in a long list was listed, also when scrolled out of sight | 150 controls of a list, none of what was on screen; `scroll` changed nothing an agent saw |
| No menus and no scrolling | Most app commands and most content were out of reach |
| A text set from outside (`AXValue`) | WhatsApp did not turn on its Send button |
| Spotify (CEF) shows nothing to accessibility, and ignores `AXManualAccessibility` | No way to use it as text |
| No provider sent images | No way to see an app like Spotify, a canvas or a game |

## Decision

| Question | Decision |
|---|---|
| What a look lists | Controls, rows (`row`), the items of a list (`item`), and text or groups the app made pressable (`button`). Only what is inside the window and each scroll area around it. Not the window's own buttons, not scroll bar parts, not a row's editable name |
| Names | Invisible direction marks removed, special spaces made plain, a phone number read out digit by digit made whole again. A row or button without a name takes its inner text |
| Typing | The field is focused, then `cmd+a`, then the text as keys (`delete` for an empty text). Only a field that takes no focus gets `AXValue` set |
| Menus | `computer.menu {app, path}`. A path to a menu lists its items with their shortcuts and greyed state. A path to an item chooses it (`AXPress`). The app comes to the front before an item is chosen |
| Scrolling | `computer.scroll {app, direction, pages?, ref?}`: pixel wheel events placed at the window's centre, or the element's, without moving the pointer |
| The pointer | `click` with `double` or `right`, and a plain click when a control refuses its own press, at the control's centre. The pointer goes back where it was |
| Electron | `AXManualAccessibility` is set on the app. When that is new and the first look is empty, the look waits 0.7 s and reads again |
| Pictures | `computer.see {app}`: a picture of the app's front window only (`screencapture -l <window>`), at most 1280 pixels on its longer side. Never the whole screen: other windows can hold anything |
| Clicks in a picture | `computer.point {app, x, y, what?}` in the pixels of the owner's last picture of that app. Refused outside the picture, for another app, or when the window moved since |
| Pictures to the model | `ActionOutput::pictures`, kept by the engine per invocation, then attached to the tool result message (`Message::pictures`, never serialized). Anthropic: image blocks in the `tool_result`. OpenAI: a `user` message after the `tool` message. Ollama: `images`. Claude Code: image blocks in its stream input. Codex: `image` items on the app server. `codex exec` and other providers: none, and the result says so (`Provider::reads_pictures`) |
| What the person sees | A question to click a spot in a picture shows that picture with a ring on the spot (`computer_picture`: the newest picture of the app). A click is refused once a newer picture of that app was taken, so the card's picture is the one the click maps from. A step that took a picture shows it under the step while the engine keeps it (the last 12, in memory) |
| Pictures in context | A new picture removes the older ones from the working set (their text stays). Each costs `PICTURE_TOKENS` (1600) in the budget |
| Approvals | Unchanged from 0018: every action but `apps` asks each time. Grants per app (`grant_per`, one answer for every action in one app) are built in `ark-core` but not used by the Computer until the owner turns them on |

## Consequences
- An agent can use most native, Catalyst and web apps through text, and any app through
  pictures with a model that reads images (Claude, OpenAI's models, Codex).
- A picture is about 1600 tokens. A look is a few hundred to a few thousand. Agents should look
  first and take a picture only when the look shows little; the action descriptions say so.
- Taking a picture needs macOS's Screen Recording permission, asked on first use.
- `codex exec` (Codex's fallback without the app server) sends no pictures.
- Long tasks still ask at every action, so nothing runs unattended yet.

## Check
- `crates/ark-computer/tests/computer.rs`: typing as keys, menus by name (a greyed item refused),
  scrolling and double-click positions, a picture and a point mapped to the screen (refused
  outside the picture), and the 0018 checks.
- `crates/ark-ui/tests/page.rs` (WebKit), `the_person_sees_what_the_agent_saw_and_where_it_will_click`:
  the picture under its step, the ring at the spot, no caption, the click at the mapped point.
- `crates/ark-runtime/src/budget.rs`, `only_the_newest_picture_stays_and_it_counts`.
- `crates/ark-model/src/{anthropic,openai,ollama}.rs`: where each puts a tool result's picture.
- `crates/ark-core/src/action.rs`, `a_grant_key_splits_answers_by_input_and_never_widens`.
- By hand: `cargo run -p ark-computer --example probe -- look <app>`, `shot <app>`, `do <action>
  <json>`.
