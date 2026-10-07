# 0047. What waits reaches a person's phone: Web Push from the runtime, devices signed in with a code

Status: Accepted (ADR 0043 step 3, 2026-10-07)
Date: 2026-10-07

## Context
ADR 0043 step 3: a question on a runtime the person is not looking at must reach them away
from the computer, be answered there, and the task must continue. Step 1 made one list of what
waits; nothing carried it beyond an open window. ARK Desktop notifies only for its own runtime's
`approval` events. A runtime on the person's own server (My machine) has nobody's control plane
in front of it (no entry page, ADR 0040), so a phone cannot come in with a one-time link: an app
added to an iPhone's Home Screen has cookies of its own and does not open links.

## Decision
| Question | Decision |
|---|---|
| How a notification travels | Web Push, sent by the runtime itself to the device's push service (Apple, Google, Mozilla, Microsoft), end to end encrypted to the device (RFC 8291 `aes128gcm`, RFC 8292 VAPID). No service of ARK's in between; a push service sees when, never what. `ring` (already a dependency) does the cryptography |
| What is sent | Each item of the attention list (ADR 0043 step 1) and each question a chat asks while its turn waits, to each device once, about a minute after it appeared, and only while it waits. Not to a device added after the item, not while that device's page is open and in view (it hears later if the item still waits), nothing older than a day. A busy push service (429, 5xx, the network) is tried again a minute later. The words are the list's (`ark_interact::attention::notice`): the place as the title, the item's line as the body; a project's results and failures replace each other (one tag), a question has its own. What went to which device is marked (`attention_notified`, per item and device), so a restart or a second process does not send it again; the runtime looks once at start for what waited before |
| What a tap does | Opens the runtime's page on the item (`#item=<id>`, or a message to a page already open): a question shows its card, a result or a failure opens as its row does; one answered meanwhile says so |
| Devices | A device record per subscription (`push_device`: endpoint, the device's public key and auth secret, a name, when it was first added, the token or session that added it, the address it saw). Only known push services are accepted, at most 10 devices. A device is dropped when its push service says it is gone, or, on a runtime with saved tokens, when the token or session that added it is revoked or ends (on a local runtime with a one-time token, when it is removed). Removed from another device, it stays removed until turned on again on itself. Lists show names and times, never keys |
| The runtime's key | A P-256 key per store, in `push-key` (mode 0600), beside `access.json`. It only signs messages to devices whose own keys are in the same store; ARK's `Secrets` would add a keychain prompt to a headless server for no protection |
| Signing a device in | A signed-in device makes a code (`POST /pair`; ARK Desktop: `token pair`): 8 characters without 0, 1, I or O, good for 10 minutes, traded once on the new device's sign-in page for a session of 90 days, never past the session that made the code. The new session is named under the one that made the code (`laptop › Phone Safari`, the device naming itself): revoking `laptop` signs it out too (ADR 0040's rule holds). Settings › Devices lists the devices signed in this way, with Sign out (their session ends, their notifications stop). Five wrong codes in 10 minutes end every open code |
| The page | `sw.js` (public, like the page's files) always shows what arrives: a phone stops delivering to a worker that shows nothing. `manifest.webmanifest` names the app, its accent and its logo, so a phone can add the page to its Home Screen. Settings › Devices: Notifications on this device (Off \| On; it says why when it cannot work), a code for another device, the devices that get notifications. A page with no session where a code signs in shows only the code field |

## Consequences
- A person with My machine gets questions, failures and results on the phone with no ARK
  service and no ARK cost; ARK Cloud's runtimes, when they come, send the same way.
- Push services need outbound HTTPS from the runtime, also on a private network (Tailscale).
- An iPhone needs the page on its Home Screen and an https address; Settings › Devices says so.
- A notification's title and body show on a lock screen: the project and the action. The phone's
  own setting hides previews; ARK sends nothing more than the list shows.
- Not built: answering from the notification itself (actions differ per platform; a tap opens
  the page, one more tap answers), a native phone app, an ARK-run relay, notifications for every
  chat message, merging several runtimes' lists (step 4).

## Check
- `crates/ark-ui/src/push.rs`: RFC 8291 Appendix A gives the RFC's body byte for byte; a device
  opens what was sealed for it; the VAPID JWT verifies; only known push services.
- `crates/ark-ui/src/notify.rs`: once, after the wait, to devices added before the item and not
  in view; a host with a device gets one notice with the list's words for a failure, and not again.
- `crates/ark-ui/tests/serve.rs`: a code made by a signed-in device signs a phone in for 90
  days; the manifest and the worker are public; a device is tied to the session that added it.
- `crates/ark-ui/src/access.rs`: a typed code works once in any case; wrong codes end it.
- Dogfood: a real push through Google's push service to Chromium (the notification's words and
  item), the tap's page opening the item, light and dark, 1280 and 390 wide.
