# 0056. A message is drafted, sent and proved as separate steps

Status: Accepted (amends 0018, 0042 and 0045)
Date: 2026-10-09

## Context

An ARK Desktop turn asked WhatsApp to send `Hola` to a named contact. The Computer was not
offered, so the model opened WhatsApp with a project process and used blind AppleScript. The
command eventually exited successfully, but ARK could not see which chat was open or whether a
message was sent. A successful input command is not proof of an external effect.

Typing and Return were also one `computer.type` call. That left no safe point to check the
conversation and draft before the irreversible key press. Stop could race the key press, and a
restart could forget text still present in another app.

The shipping WhatsApp app exposes enough accessibility evidence to do better. It identifies the
conversation header, transcript, message bubbles and composer. Outgoing bubbles identify their
direction and destination. These names belong to WhatsApp, not to ARK's generic macOS reader.

## Decision

`computer.type` only replaces text. A native message app uses three separate actions:

| Action | Contract |
|---|---|
| `computer.draft` | Re-read the exact conversation and focused composer, persist a safety record, place the text, then prove the draft |
| `computer.send` | Re-read the same conversation and draft, commit under the same lock as Stop, press Return, then prove a new outgoing message |
| `computer.cancel` | Clear only the unchanged in-memory draft, then prove the composer empty |

An app supplies a `MessageEvidence` recognizer over generic accessibility observations. The
platform reader exposes identifiers, descriptions, focus and frames, but names no app. ARK
Desktop first uses a strong recognizer when it knows an app's accessibility contract. It also
has an app-neutral fallback: exactly one focused composer, exactly one exact recipient, and an
outgoing item whose accessibility description proves both the exact text and outgoing direction.
If an app exposes less than that, ARK can still open, read and operate it, but a send stays
unconfirmed. The app boundary never becomes the capability boundary.

Before a draft is placed, the Computer durably writes `computer-hazard.json`. The versioned file
is mode 0600, synced before and after its atomic rename, and its parent directory is synced. It
contains stable interface identifiers and a keyed recipient MAC, never the recipient or message
text. The key comes from the app's secret store only when the person turns the Computer on. Any
read, key or durability failure leaves the Computer off or fails closed.

The safety state is global, not owned by a turn. It survives Stop, owner release, off and on,
permission loss and restart. `draft_present` blocks mutations until the exact composer is proved
empty. `submitted_unconfirmed` means Return was posted but the outgoing message was not proved;
it also blocks a retry. A person can clear the real composer and choose Check again. That may make
the Computer safe to continue, but it does not turn an unconfirmed effect into a confirmed one.

Stop, off, release and send share one dispatch lock. If Stop takes it first, Return is not posted.
If send commits first, Stop cannot recall the key and verification still runs. Inside that lock,
send brings the approved app to the front, proves that it is still in front, then re-reads the
bound conversation and composer immediately before Return. A different foreground app receives
nothing.

An action can return typed external-effect metadata outside its model-visible JSON. The runtime
keeps it by work and passes every answer through one finalizer. Model-visible output cannot forge
that metadata. An unresolved unconfirmed effect replaces model text with the action's trusted
uncertainty message before the answer is stored. If the provider already streamed an optimistic
answer, the UI withdraws it, including trailing whitespace, before showing the replacement. Only
new provider evidence for the same effect can mark it confirmed. The action is never retried
automatically.

Outgoing evidence uses a stable identity derived from direction, recipient and text. Mutable
delivery words such as Sent and Read are excluded. Repeated identical messages are distinguished
by their count, so a status-only change is not mistaken for a new message.

Computer activity is a sequenced generic event with a phase, app and optional target frame. It
contains no owner or draft. `ark-ui` owns the native overlay mechanics and accessible controls;
the consumer supplies its brand and app-specific recognizers. A server runtime never exposes the
local Computer.

Project processes refuse common direct desktop-automation commands as a usability guard. They
remain uncontained project commands, and wrappers can bypass the guard. It is not a security
boundary.

## Consequences

- A message takes one more approval, but the approval names the destination and exact text.
- A command exit code can no longer become a false sent claim.
- A possibly populated external composer can temporarily block Computer mutations across a
  restart. The person resolves it in the app, then asks ARK to check again.
- Strong app-specific evidence improves confirmation. The generic fallback refuses ambiguity and
  never turns a missing direction marker into proof.
- Desktop feedback remains visible across apps, while Stop and Check again stay available in the
  ARK window and menu when an overlay cannot be shown.

## Check

- `crates/ark-computer/tests/computer.rs`: confirmed and unconfirmed sends, a Return error after
  the safety boundary, persistence across restart and a second owner blocked by the tombstone.
- `crates/ark-runtime`: an unconfirmed external effect replaces false model claims through the
  shared finalizer, including continued and orchestrated work. Model-visible JSON cannot forge
  the effect.
- `crates/ark-ui`: a WebKit test withdraws a false streamed success with trailing whitespace;
  unit tests cover terminal phases and stale overlay events. The native pass covers Stop, Check
  again, reduced motion and display clamping.
- ARK Desktop: WhatsApp and generic evidence fixtures, local-window-only enablement and one
  controlled self-chat run with a unique message.
