# ARK for iPhone

Status: approved and in implementation. The first native and protocol slices exist.

## 1. Product decision

Build a native iPhone app in SwiftUI. The owner's explicit request for a native app triggers the
external-client protocol work named in the roadmap. The existing browser page remains supported,
but it is not the iPhone release and it does not decide the native app's navigation.

| Concern | Decision |
|---|---|
| Product | A pocket client for checking work, answering, and continuing a chat |
| First native release | iPhone, distributed through development builds and TestFlight before App Store review |
| Runtime | An ARK runtime at a stable HTTPS address |
| Authority | The runtime is the only authority for chats, work, approvals, files and results |
| Local state | Navigation, restoration hints and unsent drafts only |
| UI | SwiftUI screens. No embedded web app |
| Repo boundary | ARK owns the generic protocol. The owner-confirmed sibling `ark-ios` consumer owns the app |
| Android | Not in native v1. The browser page remains available |
| Minimum system | iOS 18.0. Support it and every later iOS release that can run the app |

The mental model is: **ARK is a team you message. Work happens on a computer. The phone lets
you check it, answer it, and keep it moving.**

The app is not a small Desktop window. It does not expose every panel, editor or setup control.

## 2. Native value

App Store release is gated on native utility, not a wrapper around the runtime page.

| Native capability | Person-visible value |
|---|---|
| SwiftUI Home, Chats, Conversation, Review, Result and Me | Native navigation, restoration, Dynamic Type and accessibility |
| APNs notifications | A waiting question can reach the phone while the app is closed |
| Notification routing | A tap opens the exact item, then refreshes it before enabling an action |
| PhotosPicker and document picker | A photo or file can be attached without browser upload behavior |
| System share sheet | Results can be handed to another app |
| Keychain credential | The phone remains signed in without exposing a token to a URL or app storage |
| Native lifecycle handling | Foreground reconnects and resyncs. Background suspension is represented honestly |

App Review receives a stable review runtime and least-privilege review access that remains valid
through review, is explicitly revocable and is not a production bypass. Review notes explain the
native workflows. The app has no dependency on another installed app.

## 3. Jobs and navigation

The first release supports these jobs, in order:

1. See what needs me.
2. Answer a question or approval.
3. Check what is working and what finished.
4. Continue or start a chat.
5. Open and share a result.

Use three destinations. Frequent tab changes do not animate.

| Destination | Contains | Excludes |
|---|---|---|
| Home | Needs you, Working, Recent results | Dashboard metrics and a duplicate activity feed |
| Chats | Chats, search, New chat | Projects, Terminal, agent setup, provider setup and a file tree |
| Me | This phone, notifications, appearance, help and settings | Protocol and runtime words outside Advanced |

## 4. Screens

### Pair this iPhone

The journey starts on a signed-in Desktop or Cloud client:

1. Settings > Devices > Add an iPhone shows a QR code containing only the configured public
   HTTPS runtime address. It also shows the address as selectable text.
2. The app scans the QR code or accepts the address manually. It validates HTTPS before sending
   anything.
3. The existing client shows a separate eight-character pairing code.
4. The app types or pastes that code. The runtime exchanges it once for a native device
   credential and mutation epoch.
5. After Home is useful, the app explains notifications and asks for permission.

A loopback-only runtime refuses Add an iPhone and explains that a reachable HTTPS address is
needed. A QR code or link never contains a credential or mutation.

### Home

Home answers one question: **What needs my attention now?** Sections appear only when non-empty.

| Section | Row contents | Primary action |
|---|---|---|
| Needs you | Teammate, request, project, computer | Answer or Review |
| Working | Work, current step, elapsed time, computer | Open chat |
| Recent | Result or failure, project, time | Open result or Continue |

Questions stay expanded because the choice is the work. Results remain compact. Empty Home shows
the quiet mascot, “Nothing needs you”, and the most recent chat. It does not show a feature list.

### Chats and conversation

Chats groups recent project chats and searches loaded titles locally. New chat chooses a project,
then the computer where it will run, then the composer. One available computer is shown as a quiet
line instead of a choice. An unavailable computer stays visible and read only.

The conversation has Back, title and computer in its top bar; messages, activity, results and
approval cards in the thread; one connection state; and attachment, 16 point composer, Send or
Stop at the bottom.

- The computer becomes fixed after the first message.
- Model choice stays in the overflow menu.
- A failed send keeps the draft and never claims it was sent without a durable receipt.
- Offline state is read only. No mutation queues silently.
- The composer stays above the keyboard and home indicator.
- Phone navigation never changes the chat shown on Desktop.

### Future Terminal artifacts

The iPhone does not yet have a generic path for interactive artifacts, so Terminal is not part of
the first release. If that path is added, Terminal must use it. It must not add a Projects section,
project page, Open Terminal button or standalone destination. The project-scoped protocol in ADR
0060 remains available for that future artifact.

### Review, result and Me

A short approval is an inline card. A long request is a page. Every choice has words and an icon;
destructive or lasting choices are separate. A resolved request says so and returns to Home.

The result screen previews text, Markdown, images and PDFs. Other files show their name, kind and
size with Download and Share. It does not recreate Desktop editors.

Me contains notification state, connected runtime, appearance and Sign out this iPhone. Advanced
contains the runtime address, protocol diagnostics and device details.

## 5. UI development kit

Use SwiftUI and system controls first. Do not add a general component library. Use ARK's paper,
ink, mist and one blue accent from `ark-site/BRAND.md`. Add a missing brand token at the brand
source before copying it into the app asset catalog.

| Primitive | Contract |
|---|---|
| `ARKScreen` | Safe-area aware title, scroll content and optional bottom action |
| `ARKRow` | Entire row is one action, 44 point minimum target, title, state and metadata |
| `ARKButton` | Primary, secondary and destructive roles with immediate pressed feedback |
| `ARKStateLine` | Icon plus words for online, reconnecting, offline, waiting, working and failed |
| `ARKEmptyState` | One image, one sentence and at most one action |
| `ARKComposer` | 16 point minimum text, attachment, Send or Stop and retained draft |
| `ARKApproval` | Request, scope, ordered choices, resolved state |
| `ARKResult` | Preview, file facts and system share action |

Rules:

- One primary action per screen or card.
- Every control is at least 44 by 44 points.
- Every action gives visible feedback in the same frame.
- Constraints prevent invalid actions before an error is needed.
- Content remains selectable. Control labels do not.
- Appearance follows System by default and supports Light and Dark.
- Reduced Motion removes movement but keeps useful opacity and state feedback.
- VoiceOver, Voice Control and Dynamic Type are release gates.
- Routine navigation is immediate. Occasional sheets use short ease-out transitions.

Target usability score is 10/10: discoverability, outcome visibility, recovery, mapping and
constraints each pass first-use observation.

## 6. External client protocol

ARK first extracts a generic protocol boundary before the app connects to it. ARK never depends
on or names iOS. Wording and display semantics stay computed in `ark-interact` and cross the wire.

### Compatibility

A version-independent, additive `/client/bootstrap` envelope returns the protocol majors served,
their latest minors, the runtime version and an update URL. After negotiation, `hello` returns the
selected major and minor, runtime-scoped capabilities, payload and upload limits, and the minimum
client protocol it accepts. A registered computer also returns the stable account route id.

A major change is incompatible. A minor version only adds optional fields, events or
capabilities. Clients ignore unknown object fields and unknown events, retain their sequence
cursor, and request a snapshot when an unknown event may affect the open screen. Stable enums use
an `unknown` representation in generated Swift. Timestamps are RFC 3339 UTC strings. Sequence and
mutation counters are unsigned 64-bit integers encoded as decimal strings so JavaScript, Swift and
Rust preserve the same range.

Protocol v1 stays additive through the first native release. A later runtime serves its current
and prior major together. The prior major cannot retire until a compatible iPhone build is
approved and publicly available for at least 12 months. Runtime release tests use the last
approved iPhone build, including a simulated delayed or rejected App Store review.

Capabilities are discovered at the runtime that owns the selected chat. A held chat uses a
chat-scoped handshake with its destination. Missing support says “Update this computer” and never
falls back to the origin runtime.

### Schema and client

ARK checks in generated JSON Schema and golden JSON fixtures for every phone-visible request,
response, event and error. CI fails when Rust types and checked-in schema drift. The Swift protocol
module is generated from that released schema, then pinned by the `ark-ios` release. Handwritten
transport code is allowed; handwritten duplicate wire models are not.

Every error has a stable code, a safe display sentence and optional retry information. Bounds are
part of the schema: string length, page size, event payload, upload part and full upload size.

### Event stream

Events have a stable id and sequence. The client reconnects with its last sequence. A retained
range resumes in order. A gap returns an explicit resync event and disables mutations until fresh
attention and open-chat snapshots arrive. The client consumes a bounded buffer, cancels the stream
when signed out, and backs off reconnects. iOS may suspend the app in background; the app makes no
claim that SSE continues there. Foreground always rechecks the session and resyncs before enabling
actions.

### Durable mutations

Every mutating request carries a server-issued epoch, a strictly increasing counter, operation,
target and input digest. One device sends one mutation at a time.

The runtime stores acceptance in one transaction before the effect: device, epoch, counter, input
digest, durable operation intent, high-water mark and accepted receipt. A restartable idempotent
consumer applies the intent and records completed, refused or no-longer-applicable. An exact retry
returns the first receipt. A counter reused with different input is refused. A lower counter whose
receipt aged out is reported as already applied and reconciled from a snapshot, never executed.
A counter may not skip a pending counter.

Send binds to the durable turn record. Approval binds to the kept question. Steer and cancel bind
to the named turn and become no-longer-applicable when it no longer exists. Attachment settlement
binds to the exact staged manifest and apply receipt. Epoch rotation is allowed only with no pending
intent. Revocation or expiry invalidates the epoch.

Failpoint tests restart before acceptance, after acceptance, during effect, after effect before
completion and after completion before response for send, approve, steer, cancel and attachment
settlement. No boundary may lose or repeat an effect.

### Conformance gate

- Rust server conformance tests and schema-drift CI.
- Golden fixtures decoded by Rust and Swift.
- Old-client/new-runtime and new-client/old-runtime matrix for supported minor versions.
- Web, Desktop, CLI and iPhone parity for chat, approval, result and error meaning.
- Simultaneous phone and Desktop navigation without shared selection.

ARK releases the protocol first. The app then pins that release.

## 7. Native authentication and local data

The pairing exchange issues one random `external_client` credential. Only its hash is stored by
the runtime, outside the legacy owner-token array so older binaries fail closed. It is bound to an
immutable device id, expiry and mutation epoch. Account pairing also records the installation id
for revocation and diagnostics. Re-pairing that installation replaces its earlier credential. The
editable device name is not identity.

The app stores the credential only in Keychain with a ThisDeviceOnly accessibility class. It
never enters UserDefaults, a file, URL, universal link, custom scheme, notification, crash report
or log. App Transport Security allows valid HTTPS only, with no broad exception.

The credential is accepted only by `/client/v1/*` on a client-only listener. It cannot authenticate
the runtime page, generic RPC, browser carrier, generic event stream or screen control. The typed
contract still includes bounded project terminal actions, so hiding a control is never treated as
authorization. Another signed-in device can revoke it immediately. A 401, explicit sign-out or
revoke cancels SSE, deletes the Keychain item, purges protected local data and returns to pairing.

Google sign-in gives the app an ARK Account access token with `runtime.pair`. The app sends that
token only to the ARK Cloud API, which returns a 60-second ES256 assertion addressed to the exact
runtime route. The app sends that assertion as the bearer credential to
`POST /client/v1/account-pair`; the account token never reaches the computer. `ark-account`
verifies the API's
separate pairing JWKS, issuer, route audience, type, `azp=ark-ios`, scope, owner `sub`, route id,
`iat`, `exp` and one-use `jti`. Credential issuance and the persisted `jti` hash share one locked
write, so a process restart cannot replay an assertion that has not expired. The account registry
returns no runtime credential.

First release stores no chat or result body persistently. It may keep a bounded, Data Protection
protected index of ids, titles and restoration hints. It clears on sign-out or revoke. Drafts stay
in memory. Downloaded previews use protected temporary files and are removed after the share or
preview session. A cold offline launch shows connection state and no work content.

A restored backup or app install on another device has no usable credential. Reinstall requires
pairing again.

## 8. APNs and links

Apple credentials never ship in a self-hosted runtime. Native push uses an optional ARK-operated
blind relay, recorded in its own cross-repo ADR before implementation.

1. The attested app registers its APNs token directly with the relay and receives a route id plus
   route-bound send and manage capabilities. No ARK Account is required.
2. The app keeps manage in Keychain and hands only route id and send to its paired runtime.
3. The runtime can request only a fixed generic payload for that route. Capabilities are hashed,
   expire, rotate, revoke and cannot address another device.
4. The relay separates sandbox and production, deduplicates request ids, applies per-route quotas
   and retains no work content.
5. A tap routes by validated ids inside the app, reconnects to the bound runtime and fetches fresh
   state before showing or enabling an action.

The app and self-hosted runtime remain fully usable in foreground without the relay. Relay outage
delays notifications but does not delay work or foreground refresh. Native push for arbitrary
self-hosting is not claimed until the relay path passes production APNs tests.

Universal Links may use an ARK-owned HTTPS domain with its associated-domain file. Arbitrary
self-hosted domains cannot be entitlements and therefore enter only through the in-app QR scanner
or manual address field. A custom scheme never carries credentials. No incoming link executes a
mutation.

## 9. iOS implementation and release gates

The app target uses Swift 6 strict concurrency, Approachable Concurrency and MainActor default
isolation. Protocol models remain nonisolated value types. Use URLSession, Codable, structured
cancellation and an AsyncSequence SSE adapter. Prefer no third-party SDK in v1. Use Swift Testing
for logic and XCTest only for UI automation and performance.

Before release, record:

| Gate | Evidence |
|---|---|
| Project | Bundle id, minimum iOS, version/build policy and ARK protocol pin |
| Signing | Owner-selected team, automatic development signing, distribution signing and archive validation |
| Entitlements | Push Notifications and only the ARK-owned Associated Domain, where used |
| Configurations | Debug, TestFlight and App Store use correct APNs and runtime environments |
| Privacy | `PrivacyInfo.xcprivacy`, privacy nutrition answers, support URL, privacy-policy URL and data retention |
| Store record | Name, subtitle, description, keywords, age rating, export compliance, accessibility declarations, icon and screenshots |
| Review | Stable review runtime, review access, notes and no expiring pairing step |
| Quality | Crash and hang diagnostics without secrets, network logs with private values redacted |
| Distribution | Clean archive, export validation, TestFlight install, upgrade, reinstall and revoke tests |

Only the owner submits or releases. Building, signing for development, archiving and preparing
App Store artifacts are implementation work; public TestFlight or App Store submission needs the
owner's explicit request and App Store Connect access.

V1 pairs with a runtime and creates no account in the app. App Store account-deletion rules do not
apply. If account creation is added later, in-app deletion becomes a release gate.

## 10. Test matrix

Release automation must cover all protocol and model states, fixture decoding, mutation failpoints,
offline transitions, restoration, redaction and accessibility identifiers. UI tests must cover pairing,
Home, Chats, Conversation, Review, Result, Me, notification routing and sign-out.

Simulator matrix:

- smallest supported iPhone;
- current standard iPhone;
- light and dark;
- Dynamic Type through accessibility sizes;
- VoiceOver labels and ordered focus;
- Reduce Motion;
- portrait, landscape and software keyboard;
- online, reconnecting, offline, revoked, old runtime and unknown event.

Physical evidence requires one older supported iPhone and one current iPhone. It covers QR scan,
pairing, Keychain persistence, APNs token and tap, safe areas, keyboard, PhotosPicker, document
picker, share sheet, background and foreground, device lock, poor network, TestFlight upgrade,
reinstall and revoke. Simulator output is not evidence for touch feel, APNs production, camera,
device lock or TestFlight.

Five first-use participants start with an unlocked phone and the Devices screen on a signed-in
Desktop or Cloud client. All five must pair and answer without help. At least four must complete
every other core job without help. Nobody may cause an unintended mutation.

## 11. Delivery order

| Phase | Repo | Deliverable | Gate |
|---|---|---|---|
| 0. Decisions | ARK and workspace | External protocol ADR, native auth ADR, APNs relay ADR, minimum iOS and owner-confirmed repo boundary | Independent design QA says GO |
| 1. Protocol | ARK | Types, schema, compatibility, chat-local calls, SSE resume and durable mutations | Conformance, failpoint and cross-version tests pass |
| 2. App foundation | `ark-ios` | Generated models, transport, pairing, Keychain, restoration shell and UI kit | Unit, fixture, accessibility and snapshot checks pass |
| 3. Core app | `ark-ios` | Home, Chats, Conversation, Review, Result and Me | Core UI tests and parity tests pass |
| 4. Native services | ARK, relay and `ark-ios` | APNs, notification routing, pickers and share | Sandbox and production APNs plus physical-device checks |
| 5. Distribution | `ark-ios` | Signed archive, TestFlight, metadata and review runtime | Release QA says GO; owner approves submission |

## 12. Acceptance criteria

1. A new person scans an address-only QR code, pairs once and reaches Home without written help.
2. Phone and Desktop show different chats without moving each other.
3. A phone message appears once on Desktop. Response loss and retry never repeat it.
4. One approval closes on every client. An already-resolved notification explains that state.
5. Offline state never sends, approves, stops or uploads silently.
6. Every remote chat names its computer and never falls back to another runtime.
7. Unknown protocol additions do not crash the app. Incompatible major versions give one clear
   update path and disable mutations.
8. Revocation closes the stream, invalidates the epoch, removes local secrets and prevents push.
9. Attachments reach only the selected chat's runtime and never leave partial project files.
10. Native screens, notifications, pickers, share, restoration and accessibility demonstrate
    utility beyond a repackaged website.
11. Swift tests, Rust conformance, simulator matrix, physical iPhone matrix, TestFlight upgrade and
    clean archive all pass.
12. App Store metadata, privacy disclosures, review notes and stable review access are complete.

## 13. First-release cuts

- Desktop's Agents canvas
- Rich teammate configuration
- Full file tree and editors
- Browser and Computer control
- Billing and Cloud machine lifecycle
- Custom swipe gestures
- Voice input beyond the system keyboard
- Persistent offline chats or drafts
- Arbitrary self-hosted Universal Links
- Android native app
- Interactive Terminal artifacts, until the generic artifact path owns their presentation


Protocol1.5 adds optional `ChatSummary.participants`, the sorted names of agents assigned work for the chat through durable reporting and delegation links. Missing/null means unknown; an empty list is known empty. It does not prove an agent contributed a message or grant authority. Clients may show up to three faces with an overflow count, and expose all assigned names to accessibility. Legacy runtimes retain the neutral identity. The minimum supported minor version stays unchanged.
