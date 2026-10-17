# Event automations

Plan for work that starts when something happens. Scheduled work already starts at a time
(ADR 0053). This adds external events without building a second task, permission or connection
system.

## Product contract

A person asks in words:

> When Lara emails us, reply in the same thread using `budget.csv`.

> When a customer sends our WhatsApp Business number a message, answer from the current price
> list.

ARK shows one approval with the condition, work, data and allowed effects. After approval the
automation stays beside the chat that made it. It can be paused, resumed, changed or stopped.
Every matched event and run is visible there.

Time and event starts share one person-visible model without rewriting the shipped schedule
record:

```text
AutomationView
  Time(ScheduleRecord) | Event(EventAutomationRecord)
```

`ScheduleRecord`, its stored JSON, `s-` ids and public APIs do not migrate. A schedule is shown as
an automation that starts at a time. `/schedules` and the existing host methods stay unchanged;
`/automations` and `AutomationView` add an umbrella over both kinds. Shared task submission, run
policy and view helpers are extracted only where the two implementations use them.

## Clean primitives

| Primitive | Owner | Contract |
|---|---|---|
| `ExternalEvent` | `ark-tools` | A transport-authenticated, normalized event with an opaque stable `EventKey`, source, account, kind, time, direction, actor claim and small scalar fields. Provider content is a bounded payload or a provider reference, never a tool schema |
| `EventPattern` | `ark-orchestrate` | Deterministic exact matching over trusted fields. First release supports source, account, kind, direction and exact actor id. No model decides whether an event matches |
| `EventSink` | contract in `ark-tools`, implemented by `ark-orchestrate` | `ingest(event)` saves before it answers. Repeating the same `(source, account, EventKey)` is harmless |
| `EventAutomationRecord` | `ark-orchestrate` | The pattern, task, run policy, authority, owner chat, activation watermark, revision and visible state. It names no provider |
| `AutomationView` | `ark-interact` | One person-visible union over an unchanged `ScheduleRecord` and an `EventAutomationRecord` |
| `AutomationRun` | existing `TaskRecord` | An ordinary task. Its stable run key is `(automation_id, event_key)` or `(automation_id, time_slot)` |
| `EventSource` | contract in `ark-tools`, implemented at the provider edge | Gets transport-authenticated provider events by polling or pull subscription and hands normalized events to `EventSink`. Cursors and secrets stay with the connection. A push provider exposes a verifier and decoder to the host endpoint, then uses the same sink |
| `AutomationAuthority` | trusted field on `TaskRecord` | A mandatory run-scoped allow-list for every action and input, including reads, exact project paths, event-bound values, mutation mode and durable quotas |

Do not add a rule language. `EventPattern` starts with equality over canonical fields. New
operators need a real second use case and deterministic tests. Message text is task input, not
an instruction that can change the automation, its tools or its grant.

The dependency direction stays one way: `ark-tools` defines provider-neutral values and source,
sink and webhook contracts; `ark-connect` implements account sources and webhook verification;
`ark-orchestrate` implements the sink, matching and records; `ark-interact::App`, which already
owns the SDK and project orchestrators, wires account sources to each project's sink and computes
labels and views; `ark-ui` owns one generic hardened webhook route. Neither `ark-connect` nor
`ark-orchestrate` depends on the other.

### Schedule compatibility

| Existing contract | Event automation work |
|---|---|
| Stored kind and JSON | `orch_schedule` and `ScheduleRecord` remain unchanged; no migration or write-back |
| IDs | Existing and new time schedules keep `s-`; event automations use `a-` |
| Rust API | `schedule`, `schedules`, `schedule_record`, pause, resume, change, run now and stop keep their signatures and meaning |
| Client API | `/schedules` and schedule host methods stay; `/automations` and event methods are additive |
| Startup | `scheduled_projects` stays; `projects_with_work` additionally reads active `event_automation` records |
| Runs and retention | Time runs keep their slot dedupe, retries, missed-run policy, overlap and newest 50 records |
| Permissions | Time schedules keep ADR 0053 behavior. Only event runs require `AutomationAuthority` |

### v0.19.1 protocol compatibility

The implementation starts from the released external-client protocol v1.1 in ADR 0057. It does
not add event triggers to that release and does not make the native protocol a dependency of the
provider or orchestration layers.

| Released surface | Compatibility rule |
|---|---|
| `ScheduleRecord` and `orch_schedule` JSON | Keep byte-compatible. Do not migrate or write back records merely because the runtime opened them |
| Project resource calls | Keep `schedules`, `schedule`, `unschedule`, `schedule_pause`, `schedule_resume` and `schedule_run` with their existing parameters and meanings |
| Protocol bootstrap and `Hello` | Keep protocol major 1 and existing required fields, limits and capability names. Later schedule or automation support is advertised by new capability names only |
| Protocol v1 DTOs | Do not change the meaning or required shape of the released home, chat, attention, result, project, computer, terminal, mutation or event types. New fields are optional |
| Event stream | Keep stable decimal sequences, replay, `resync`, `affects` and unknown-event behavior. A later automation event adds an `affects` value and snapshot capability rather than changing an old event |
| Native mutations | A later pause, resume, run-now, change or stop operation uses the released epoch, counter, digest and receipt rules. It must be durable and ordered before the effect, with stable error codes |
| Generated clients | Any native-visible addition updates Rust types, checked-in schema, fixtures, generated Swift models and old-client/new-runtime compatibility tests together |

An old v1.1 client sees no automation capability and continues to use every existing screen. The
bundled page and terminal keep their current schedule controls. A later typed client first gains
read-only automation snapshots and resumable events, then mutations behind separate advertised
capabilities. No existing capability is repurposed.

## Durable flow

```text
provider -> verify and normalize -> save event -> acknowledge provider
                                  -> match active automations
                                  -> submit task once by stable run key
                                  -> record result in the owner chat
```

Each step can repeat after a crash:

1. The feed advances its provider cursor only after every event in the batch is saved.
2. An `EventKey` is deterministic. A source must give repeated delivery of the same semantic event
   the same key and distinct transitions different keys. Re-delivery replaces nothing and creates
   no second event.
3. Matching can scan saved events whose evaluation is incomplete. A stable run key makes task
   submission idempotent without a transaction across several records.
4. A run uses the existing task retry, question, attention, graceful-stop and report-to-chat
   paths. It does not have a second executor.
5. Provider actions use the external-effect rules from ADR 0056. A timeout after a send is
   uncertain, not success and not an automatic retry. A provider idempotency key is used where
   the provider supports one.

### Records and one-writer rule

| Record kind | Stable id | State kept |
|---|---|---|
| `event_feed` | source + account | provider cursor, last successful poll, backoff, health and next poll |
| `external_event` | opaque `EventKey` from source + account + provider identity + semantic variant | bounded normalized fields, provider reference, arrival sequence and payload |
| `event_automation` | `a-` plus random id | revision, activation sequence, pattern, task, authority, policy and owner chat |
| `event_evaluation` | hash of automation id + revision + event key | matched or ignored, task id when matched, and terminal reason |
| `orch_task` | deterministic `t-` id from the run key | the existing task plus automation id, revision, event key and authority |
| `automation_quota` | automation + action + window | attempts consumed, including uncertain effects |

A0 first enforces one writer process per store with an exclusive runtime lock. A second runtime
writer refuses to open it. Read-only clients still use the running process's protocol, and all
same-process components share its store handle. Under that invariant, the evaluator lock plus
deterministic record ids make a claim safe. Multi-process workers are not supported or claimed.
Before adding them, `ark-store` must gain an atomic insert-if-absent claim or an indexed event
table with leases.

The first limits are part of the contract: 1 MiB raw webhook body, 256 KiB normalized event
payload, 64 scalar fields of at most 4 KiB each, 1,000 unevaluated events per account, 100 queued
runs per automation and 256 MiB of retained event payloads per account. A pull feed stops before
advancing its cursor when full. A webhook returns a retryable failure before acknowledgement.
One attention item explains the limit. Pending, queued, open or uncertain input is never pruned.
Evaluated unmatched payloads are removed after 24 hours; matched payloads follow the newest 50
runs and 30-day maximum, while their ids and audit state remain. `EventLog::prune` does this
hourly and before a save is refused for storage. A feed that cannot save for either limit shows
Full (not Degraded), keeps its cursor and tries again at the poll interval.

### Activation and changes

Every saved event gets a store-local arrival sequence. Creating an automation records the current
sequence and revision 1. Only later events can match. Changing or resuming increments the revision
and records a new current sequence. Events from before that cutover never match the new revision.

Manual Pause skips events received while paused, matching the existing schedule rule that pause
creates no backlog. It cancels event runs that have not started and marks their evaluations
`cancelled_by_pause`, including matched pending evaluations that have no task yet; a run already
executing continues under the old immutable authority. Change marks the same pending and queued
work `cancelled_by_change`, then starts a new revision. Stop marks it
`cancelled_by_stop`, removes the record and leaves a running task and history intact. Evaluation
state is keyed by `(automation, revision, event)`, so recovery cannot reinterpret an old event
under new words or permission.

Capacity Full is not Pause. It freezes the current revision and preserves its queued runs and
pending evaluations. A running task continues. When the queue falls below its cap, the same
revision admits pending events and returns to Listening. Other automations on the same account
keep ingesting and matching until the account inbox cap is reached. At that cap the shared pull
feed stops before advancing its cursor, or the webhook returns a retryable failure. “Discard
pending and start from now” is a separate destructive action: it asks, marks the preserved
evaluations and queued tasks `discarded`, and creates a new revision at the current sequence.

## Safety and unattended effects

Creating, changing or resuming an automation always asks. The approval shows:

| Shows | Example |
|---|---|
| Starts when | Inbound Mail from `lara@example.com` |
| Does | Reply in the same thread using `budget.csv` |
| Can read | The matched message and `budget.csv` |
| Can change | Send one reply to the matched sender in the matched thread |
| Limit | One open run, at most 10 replies per day |

The automation approval creates an `AutomationAuthority` from trusted orchestration state. It is
copied immutably to each event task and checked before the ordinary `ApprovalGate`, for every
RiskTier. It is default-deny and cannot be widened by a project or session grant, `--yes`, the
model, the event body or a changed tool call. Its constraints are data, not provider code:

- exact action name;
- literal equality for fixed inputs;
- equality to a trusted event field, such as recipient equals the matched actor;
- equality to the matched subject, such as the same mail thread;
- named project paths that may be read;
- a count and time window. Windows are fixed and start at midnight UTC for a day, so up to
  twice the count can run across midnight.

Literal constraints compare canonical values. Event-bound constraints are substituted from the
trusted stored envelope, never from model JSON. Project paths are resolved beneath the project,
with symlinks and aliases checked before the call. Known limit: the tool gets the path the model wrote after that check,
so a link swapped in between could redirect a read inside the run's project. Run code, processes, network and any unnamed
action are absent unless the approval names them. A call outside the authority is refused. A
mutation inside it asks unless its mutation mode is `unattended`; that mode atomically consumes
its durable quota before dispatch. Event text cannot create, resume or widen an automation,
connect an account or grant an action. Only the owner chat gets the automation tools.

Existing schedules keep ADR 0053 permissions, including `allow_mutate`, project grants and their
current `--yes` behavior. Changing them is a separate security decision, not hidden in this work.

Inbound direction is required by default. Outbound status updates never match an inbound-message
automation. Per-actor and per-subject cooldowns prevent reply loops. One run is open by default;
later events queue in arrival order. A bounded queue that fills moves to Full, preserves the
current revision and creates one attention item. Three failed runs in a row still Pause it,
matching schedules.

## Provider proofs

### Mail

`ark-connect` adds read and same-thread reply actions plus one poller shared by every automation
on the same Gmail account. On a home NAS the first transport is
outbound polling of `users.history.list`, so no public port or cloud service is required. The
cursor is the Gmail `historyId`.

Activation first reads a current history id and saves it as the baseline. It does not run old
mail. Each poll pages `history.list` completely, keeps only `messageAdded` events that are inbound
and in INBOX, fetches each message needed by a pattern, and saves an event keyed by Gmail message
id. Only then does it advance the history cursor. Sender addresses are parsed and canonicalized,
but the UI says the exact address, never that the human identity is verified.

Gmail says an invalid or expired history id requires full sync. ARK does not silently jump to a
new watermark. It pauses the feed and its automations with the uncertain interval visible, then
offers two explicit recoveries: reconcile the mailbox from the last successful time, or start
from now and acknowledge the gap. Reconciliation completes before the new cursor is committed.
An automatic reply is bound to the literal approved address and Gmail thread/message headers. A
different `Reply-To` requires approval. Gmail send has no assumed idempotency key; a timeout is a
durable uncertain effect and is not sent again automatically.

Gmail push can be added later as a transport for the same feed. It uses Cloud Pub/Sub and still
requires `history.list` to obtain the changes. Google recommends polling for user-owned devices,
requires a watch renewal at least every seven days, and says notifications can be delayed or
dropped. Sources:
[push notifications](https://developers.google.com/workspace/gmail/api/guides/push),
[history](https://developers.google.com/workspace/gmail/api/reference/rest/v1/users.history/list),
[sync](https://developers.google.com/workspace/gmail/api/guides/sync).

Mail reading needs Google's restricted scope and its review. It does not ship hidden behind the
existing send-only consent.

### WhatsApp

The first supported source is the official WhatsApp Business Platform Cloud API, not a personal
WhatsApp account and not screen scraping. Meta delivers inbound messages and status changes to a
public HTTPS webhook. Events include a stable message id, sender and type. Sending uses the same
Business Platform connection. Sources: Meta's official
[Cloud API collection](https://www.postman.com/meta/whatsapp-business-platform/collection/wlk6lh4/whatsapp-cloud-api),
[webhook payload reference](https://www.postman.com/meta/whatsapp-business-platform/folder/tduohwq/webhook-payload-reference),
and [messages reference](https://www.postman.com/meta/whatsapp-business-platform/folder/13382743-ba8d099d-007e-4b52-b9f2-3cf3c60e4fbc).

A NAS needs a public HTTPS route supplied by the person, such as their reverse proxy or tunnel.
An optional ARK relay may forward authenticated envelopes later, but self-hosting never depends
on it. `ark-ui` exposes `GET/POST /hooks/<source>/<opaque-route>` only for a registered webhook
source. GET handles Meta's verification challenge. POST reads at most 1 MiB within 5 seconds,
verifies `X-Hub-Signature-256` over the raw bytes before JSON parsing, binds the WABA and phone
number to the connected account, saves every decoded event, then acknowledges. Inbound message
keys and status-transition keys are distinct because status changes reuse a message id. The
source derives each opaque key from the message id plus semantic variant, such as inbound message
or one named status transition and its provider occurrence. Secrets and system-user tokens live
in `Secrets`; rotation restarts the source without changing the automation.

Production setup requires a Meta app, Business portfolio, WhatsApp Business Account, registered
business phone number and phone-number id, app secret, webhook verification token, WABA webhook
subscription, `whatsapp_business_management` and `whatsapp_business_messaging`, a suitable system
user access token, and any required Meta app or business review. The connection is not Ready until
ARK validates the bound ids and subscription.

Free-form replies are allowed only inside Meta's rolling 24-hour customer support window. The
authority binds that deadline from the inbound event. Dispatch and every retry re-check it. After
expiry, ARK asks for an approved template or fails closed; it never turns free text into a
template. A send timeout is a durable uncertain effect and is not retried automatically. Source:
[Meta status and customer support window](https://www.postman.com/meta/whatsapp-business-platform/folder/fuaee8l/statuses-object).

An approved authority binds `whatsapp.reply`'s `to` to the event's `actor` and its
`reply_until_ms` to `fields.reply_until_ms`. The action also refuses a deadline more than 24
hours ahead, so a free-form reply outside the window fails closed even without that binding.

One Meta app sends every number's changes to its one callback URL, so connect one number per
Meta app. A body's changes for another number are left out; a body with only those is refused.
Known limits of the first route: a request's headers get the general 30-second read limit (the
5 seconds apply to the body), each request reads the connected numbers' secrets to find its
route, and concurrent hook connections are not capped. The app secret is proved only by the
first signed webhook.

Personal WhatsApp remains available only through the visible Computer workflow in ADR 0056. It
is not an unattended event source.

## Background ownership

`projects_with_work` includes projects with active event automations. Opening the app starts one
feed supervisor per connected provider account even when no chat or window is open. All
automations on that account share it. The supervisor uses a 60-second Gmail poll interval, jitter
and exponential backoff from 1 to 30 minutes, and records degraded state. Provider quota errors
respect their retry time. Disconnect stops the feed and marks dependent automations `needs
connection`; reconnect resumes from the durable cursor before accepting new work.

## One NAS and a larger runtime

| Concern | First implementation | Scale path without changing records |
|---|---|---|
| Process | One locked runtime process, one supervisor per connected account | A future atomic claim permits several workers; not part of A0 to A4 |
| Persistence | Existing SQLite WAL and typed records | Add indexed event tables only when profiles show record scans matter |
| Dispatch | One in-process matcher and existing orchestrator queue | Add insert-if-absent claims and expiring leases before multi-process dispatch |
| Ordering | Per feed cursor, event time retained, arrival order used | Partition claims by account and subject |
| Backpressure | One open run per automation, bounded pending events, Full on overflow | Per-account quotas and worker capacity |
| Delivery | Outbound polling when possible; verified webhook when required | Pull subscriptions or an optional relay use the same envelope |
| Tenancy | One runtime and store per workspace | Cloud keeps the same boundary, not a global automation service |

No Redis, Kafka or external scheduler is required. Do not build leases or a separate event table
until a measured workload needs more than the typed-record path. Stable ids, cursors and run keys
preserve that option.

## Person-visible states

The chat has one quiet line per automation. Opening it shows the condition, task, connection,
permission, last event, last run and history. Controls sit beside the record they change: Pause or
Resume, Change and Stop. There is no rule builder in the first release.

| State | What the person sees | Recovery |
|---|---|---|
| Listening | `When Lara emails · listening` | none |
| Running | the matched sender and current step | Stop the run; the automation remains |
| Waiting | what answer or approval is needed | Answer from attention or the automation |
| Needs connection | which account disconnected | Reconnect; events after the saved cursor are recovered when the provider permits |
| Starting | the account and that its baseline is being saved | Wait; no event can run before the cutover is durable |
| Degraded | polling backoff and the last successful check | It retries within the shown bound |
| Recovery gap | the exact interval ARK cannot prove complete | Reconcile or explicitly start from now |
| Paused | why it paused and the pending count | Inspect, then Resume |
| Full | the preserved queue or account storage limit and what stopped | Let it drain, raise the explicit limit, or Discard pending and start from now |
| Rate limited | the action and when its quota resets | Wait; Retry is unavailable |
| Reply window ended | free-form WhatsApp reply is no longer allowed | Choose an approved template or leave it unsent |
| Send uncertain | the destination and action that may have happened | Check the provider; automatic Retry is unavailable |
| Failed | the last failure and whether anything may have happened | Resolve or retry from that run; never hide uncertainty |

The first draft scored 6/10 on the `design-everyday-things` diagnostic because degraded delivery,
recovery gaps and unsafe retry states were invisible. The states above address those design
findings. The design gate must score the rendered flow again. No score is claimed before that
review and a first-use dogfood.

## Delivery order and gates

| Step | Work | Done means |
|---|---|---|
| A0 | Store lock, records and fake feed | A second writer runtime refuses the store while protocol clients still work; existing schedule JSON, ids, APIs, startup discovery, controls, retention and permissions pass unchanged; one fake event delivered twice makes one task and one chat result; event-before-create, manual pause, change, resume and stop follow their cutovers; full then drain keeps the revision and queue; full then Discard cancels it and cuts over; crash points before and after every durable step make no duplicate run |
| A1 | Authority, approval and interaction | The mandatory authority blocks Observe, Run code, process, network, symlink/path alias, broader grants, `--yes`, changed arguments and multiple calls beyond quota; create, inspect, pause, resume, change and stop work in words; `/automations` and every visible state pass in both front-ends |
| A2 | Mail proof | Exact-address inbox trigger on one shared NAS poller; pagination and duplicate pages; disconnect, revoke and cursor expiry; explicit recovery gap; same-thread reply bound to the approved literal address; malformed and oversized messages; send timeout uncertain |
| A3 | WhatsApp Business proof | Meta app, Business portfolio, WABA, phone id, app secret, verification token, webhook subscription, permissions, system-user token and required production review checked; secret rotation; GET challenge; raw-body signature before parse; account binding; malformed, oversized, duplicate and out-of-order webhooks; distinct status keys; 24-hour window at dispatch and retry; send timeout uncertain |
| A4 | Load and recovery | 1,000 mixed fake events, 100 queued runs, slow model, full inbox, full then drain, full then discard, another automation sharing the account, reply loop, quota race, disconnect, SIGKILL and disk full. No duplicate task or effect attempt; preserved capacity input is not pruned; manual pause and explicit discard have terminal audit reasons; ingest-to-task p95 under 2 seconds excluding model and provider poll; retained event storage stays within the configured 256 MiB cap |
| A5 | Optional transports | Gmail Pub/Sub or an ARK relay only if polling latency or public webhook setup blocks real use |

Status: A0, A1, A2 (Mail, against a fake Gmail) and A3 (WhatsApp Business, against a fake Graph
API) and A4 (limits, against fakes) are built on the `event-automations` branch. No real Google
or Meta account was used. Settings › WhatsApp connects a number with the checks of
`Connections::connect_whatsapp` and shows the callback path. A4 ran with tight limits on a Mac:
a run on NAS-class hardware (memory, SQLite growth, the 2-second p95 at 1,000 events) is an owner
card. Not tested: disk full (a store error becomes Degraded with backoff by code path only),
reply loop and quota race beyond the A1 tests. Its runs show a
closed reply window and an uncertain send as the run's failure and effect, not as their own
automation states.

Each gate gets plan, design and implementation review. A0 starts only after a workload has exact
acceptance fixtures for Mail and WhatsApp. A4 records memory, SQLite growth and model cost as well
as its pass/fail limits. No paid provider or Google review is started without the owner's
approval.

## Not in the first release

- Semantic conditions such as “when a message sounds urgent.”
- Personal WhatsApp monitoring or background Computer control.
- Chaining automations by letting one event create another automation.
- A visual flow builder, arbitrary expressions, branches or loops.
- A global ARK Cloud event bus.
- A promise of exactly-once external effects. ARK promises one durable run per event and reports
  an uncertain effect truthfully.
