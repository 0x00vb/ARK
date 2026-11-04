# Contextual app connection requests

Status: implementation candidate under verification. Review corrections and the full required
gate are in progress. No release or deployment approval is implied. Code is authoritative for
exact Rust signatures; the sketches below retain the design and its safety boundaries.

## Scope and guarantee

Keep the original user request in its chat while it needs an app connection. After the server
confirms access, complete one saved metadata-acquisition call with the exact claimed binding,
then continue the original waiting turn in place. No synthetic user message or
`chat.send` is used. Prior completed mutations use the existing ADR 0048 deduplication guard.

The first consumer offers `integration.list` and explicit `integration.connect { offer }`.
Only the latter opts into contextual acquisition. It is Observe and returns binding metadata.
It performs no external provider operation. `integration.run` remains a separate operation with
its existing fresh approval, turn nonce, invocation ledger and unknown-effect handling.
Core does not name these tools or any integration vendor.

Arbitrary blocked external mutations, resumable model cursors and provider operation replay
are out of scope. Unconfirmed effects cannot become contextual acquisition requests.

## Existing seam evidence

Paths below are relative to `crates/`.

| Source | Finding |
|---|---|
| `ark-runtime/src/acquire.rs:77` | AcquisitionProvider owns missing capabilities. Its free-text methods lack exact action/input context and typed pending results. Extend this seam. |
| `ark-interact/src/capabilities.rs:253` | Connector recognizes local Connections catalog and awaits OAuth. Reuse acquisition meanings, not local OAuth for managed offers. |
| `ark-interact/src/approval.rs:130` | Chat approval transport is an in-memory oneshot. |
| `ark-orchestrate/src/record.rs:160` | Durable Question belongs to TaskRecord. Do not synthesize a task for a chat prerequisite. |
| `ark-runtime/src/engine.rs:591` | Ordinary execution creates an invocation and records Running before dispatch. Save the prerequisite before dispatch. |
| `ark-runtime/src/harness.rs:1078` | Prerequisite discovery must precede ordinary invocation approval here. |
| `ark-runtime/src/harness.rs:1347` | Existing checkpoint holds final project metrics, not an execution cursor. |
| `ark-sdk/src/lib.rs:489` | Existing unfinished-work continuation assembles bounded carry-over, mutation dedup, cut-short guards and hand-off take-up. |
| `ark-runtime/src/session.rs:235` | Turn owns goal, execution mode and work identity. New persisted fields default for old records. |
| `ark-interact/src/interaction.rs:1465` | Shared recording, journal and activity belong here. |
| `ark-ui/src/host.rs:1411,1695` | Ordered epoch/counter admission and accepted-mutation restart reconciliation already exist. |
| `ark-protocol/src/lib.rs:388` | Optional conversation fields are the authoritative native projection. |

## Compared designs

| Design | Result |
|---|---|
| Only `acquire_capability { need }` | Reuses the live meta-tool but loses explicit offer selection and exact invocation context. Requires model translation and changes free-text acquisition into an exact operation API. Local Connector still cannot serve broker scopes or restart. Do not force this mapping. |
| Optional contextual acquisition on AcquisitionProvider | Keeps free-text acquisition unchanged, lets the consumer recognize its metadata action, and reuses acquisition concepts and existing turn continuation. Choose this bounded extension. |

A persisted full harness cursor was considered and rejected. The requirement is the same user
request, not the same internal turn id or bitwise model execution. Continuation reconstructs
bounded stored progress under ADR 0048.

## Exact proposed interfaces

```rust
// ark-core::action. Additive field defaults false and is omitted when false.
ActionDescriptor {
    /* existing fields */
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    contextual_acquisition: bool,
}
ActionDescriptor::contextual_acquisition(self) -> Self

// ark-runtime::acquire.
struct ContextualAcquisition { offer_id: String, display: String }

// Optional method on existing AcquisitionProvider. Default Ok(None).
async fn contextual_acquisition(
    &self, action: &ActionDescriptor, input: &Value,
) -> Result<Option<ContextualAcquisition>, ContextualAcquisitionError>;

struct ConnectionScopeRequest {
    request_id: String, invocation_id: String, offer_id: String,
}
struct PreparedConnectionScope {
    scope_ref: String, broker_revision: u64, expires_at: OffsetDateTime,
}
struct ResolveConnectionScope {
    scope_ref: String, request_id: String, invocation_id: String, route_id: String,
}
struct ResolvedConnectionScope {
    scope_ref: String, request_id: String, invocation_id: String, route_id: String,
    offer_id: String, fingerprint: String, broker_revision: u64,
    state: ConnectionScopeState, claim_receipt: Option<ClaimedConnectionBinding>,
}
enum ConnectionScopeState { Pending, Ready, Claimed, Cancelled, Expired, Unavailable }
struct ClaimConnectionScope {
    scope_ref: String, request_id: String, invocation_id: String,
    expected_broker_revision: u64,
}

// ark-tools, so adapters need no upward dependency.
struct ClaimedConnectionBinding {
    receipt_id: String, scope_ref: String, request_id: String, invocation_id: String,
    route_id: String, offer_id: String, binding_id: String, fingerprint: String,
    broker_revision: u64,
}
RunCtx { /* existing fields */ connection_binding: Option<ClaimedConnectionBinding> }

// ark-runtime::acquire. All methods take &self.
trait ConnectionRequestBroker: Send + Sync {
    async fn prepare(ConnectionScopeRequest) -> Result<PreparedConnectionScope>;
    async fn resolve(ResolveConnectionScope) -> Result<ResolvedConnectionScope>;
    async fn claim(ClaimConnectionScope) -> Result<ClaimedConnectionBinding>;
}
ArkBuilder::connection_requests(Arc<dyn ConnectionRequestBroker>) -> Self
```

The trait sketch elides receivers on broker methods only. Errors are typed refused, stale,
unavailable and uncertain variants with safe display text. Strings cannot declare access
success. Broker prepare/claim changes only metadata DB. OAuth starts only through the person's
existing Account app flow. Sources forwards contextual queries; multiple matches fail closed.

Runtime checks BOTH descriptor marker and `risk == Observe` before consulting this method.
An opted-in Mutate or Read action is invalid, never downgraded. Unmarked actions cannot pause.
This explicitly narrows the trust contract to reviewed side-effect-free acquisition actions.
Consumer connect is the only opted-in first action. Arbitrary operation errors cannot pause.

The optional broker is injected once in Ark/Engine and shared by Host. It binds authenticated
workspace and fixed runtime route itself. Runtime cannot self-declare an Account workspace.
No raw Account bearer enters protocol, chat records, events or RunCtx.

These fields match `contextual-broker-design.json` in the parent evidence directory. The consumer
implementation lives in the separate API/consumer task; Core ships only this trait and fixtures.
The hosted implementation uses finite RuntimeAppGrant for prepare and claim. The desktop
implementation uses Account at its fixed origin and current exact registered route ownership.
Native uses Account resolve/selection/OAuth routes directly, never sends its bearer to runtime.
Desktop account equality compares ADR 0069 provider boundary identity, not labels or URLs.

Preparation retries use the exact persisted request/invocation ids. Claim uses the independent
last resolved brokerRevision. A repeated exact claim tuple returns its immutable receipt even
if that claim itself advanced brokerRevision; it never chooses another binding. Validate every
returned identity plus offer fingerprint before persisting a trusted receipt. Broker performs
selection: one eligible connection may be atomically pinned under its sole-connection policy;
multiple connections require explicit Account selection. Contextual OAuth binds the exact attempt
before issuing a ticket. Old callbacks cannot replace the selected attempt or claimed binding.
Runtime does not inspect safeConnectionChoices or pick an account. Broker's returned offer and
safe choices remain on Account's resolve surface for native presentation.

## Durable data

```rust
struct PendingConnectionRequest {
    id: String, project: ProjectId, session: SessionId, work: String,
    invocation: InvocationId, action_digest: String,
    requirement: ContextualAcquisition,
    resolution: Option<ResolvedConnectionScope>,
    expires_at: Option<OffsetDateTime>,
    local_revision: u64,
    phase: ConnectionRequestPhase,
}
enum ConnectionRequestPhase {
    Preparing,
    Waiting {
        scope_ref: String, broker_revision: u64, offer_fingerprint: String,
    },
    Claimed { binding: ClaimedConnectionBinding },
    Acquired { binding: ClaimedConnectionBinding },
    Admitted { run_id: String },
    Started { run_id: String, pid: u32, pid_start: u64 },
    Completed,
    Interrupted { run_id: String },
    Closed { reason: ConnectionRequestCloseReason },
}

// ark-runtime::session. Runtime owns all variants.
enum TurnConnection {
    Waiting { request_id: String },
    Admitted { request_id: String, run_id: String },
    Started { request_id: String, run_id: String, pid: u32, pid_start: u64 },
    Interrupted { request_id: String, run_id: String },
}
Turn {
    /* existing fields */
    #[serde(default, skip_serializing_if = "Option::is_none")]
    connection: Option<TurnConnection>,
}
```

Preparing, the unperformed invocation, the materialized waiting turn and its counted session
header commit atomically. The transaction checks header.turn_count equals the placeholder's
original index, sets turn_count to index + 1 once, and preserves the original input/title/goal.
A later Input can therefore only allocate index + 1, never overwrite the original waiting row.
The same transaction accounts the paused attempt's metrics once. Engine prepares an in-memory
typed pause candidate without dispatch; Session commits it with the final paused-attempt metrics
when it receives AwaitingConnection. Broker prepare starts only after this durable transaction.
No partially stamped waiting placeholder is made externally actionable. Local revision increments
on each successful state transition under CAS. Broker revision is separate and used only for
broker claims. Request and invocation IDs are opaque UUIDs; chat IDs are opaque strings.
action_digest binds exact action/input and reviewed schema
identity. No provider session or browser attempt becomes chat state.

Pending invocations keep the existing PendingApproval status plus an optional connection_request
marker. Waiting authority requires the matching durable request, original work, current turn
marker and expected status. PendingApproval alone grants nothing; Ok or Failed cannot stand for
a waiting requirement. This encoding lets old R3 read the history without a new enum tag.
Missing or mismatched markers retire the orphan rather than reconstructing authority.

Refresh is single-flight per request across an Engine family, including its scoped clones.
The request is re-read after acquiring the async guard. Concurrent polls do not repeat metadata
adapter dispatch. The guard ends before model continuation and never waits for human OAuth.
Lost broker responses consume the authoritative immutable claim receipt, not a fresh claim at
the post-commit revision. There is no external provider operation in acquisition completion.

## Host and SDK methods

```text
Engine::refresh_connection_request(request_id) -> PendingConnectionRequest
Engine::admit_connection(request_id, revision) -> AdmittedConnectionRun
Engine::start_connection(admitted_run) -> Turn
Engine::interrupt_connection(admitted_run) -> ()
Ark::resume_connection(session, AdmittedConnectionRun, options, on_event) -> HarnessOutcome
Interaction::resume_connection(AdmittedConnectionRun, control, on_event) -> TurnEnd
Host::client_connection_requests(chat) -> projected list
Host::apply_client_mutation(device, mutation) -> receipt
Host::resume_client_mutations() -> startup reconciliation
```

The SDK accepts an opaque admitted run token, never user text or a caller-created binding. The
token is constructed only by Engine admission and checked against the durable marker again when
execution starts. Resume loads the original waiting Turn by exact index, not the next index.
It does not call begin_turn, goal_for or goal.then. It restores the original goal, steering,
execution mode and unfinished-work chain. Same-work successful mutations feed done_before;
same-work ambiguous mutations feed cut_short. Include acquired metadata in bounded carry-over.
The user-visible guarantee is no duplicate person message, new user input or chat.send. Existing
Goal records may repeat the goal internally under unchanged storage semantics.

The waiting turn has already been counted once. New `Session::resume_waiting` reloads its exact
stored row and header after admission. `Session::replace_resumed_turn` replaces that row and
updates the matching recent entry. It does not call push_turn, increment turn_count, regenerate
title or slide gists again. Extract SessionUsage metric accumulation from its turns increment:
each resumed attempt contributes only its new metrics; the existing stored metrics are not added
again. The combined Turn metrics/citations/steering preserve earlier attempts. A CAS verifies
header turn_count and exact current index/work before replacement so a stale Session cannot
overwrite a newer turn or header. A newer admitted instruction makes replacement stale. A waiting
row folded out of the recent window also makes its request stale; never mutate a folded gist to
resume it. The current waiting turn normally cannot fold without an intervening newer input.

Interaction seeds Recorder from the original stored trace. Journal targets the original index
and appends new trace events after the existing prefix. Never call Recorder::start to erase that
prefix on resume. Existing trace retention applies. A test compares original trace events before
and after pause, restart, claim and completed resume byte-for-byte as a retained prefix.

Admitted is durable but not started, so startup may start that one queued run. Immediately before
the first continuation model request, a CAS changes Admitted to Started with writer identity.
Started whose writer dies becomes Interrupted, never automatically starts again. It requires
the existing explicit ADR 0048 continuation policy, because Observe actions may write records.
This deliberately chooses a safe lost-start boundary over replay. Completed receipts mean
acquisition plus bounded resume admission, not successful completion of the whole user task.
Any fallible setup or continuation error after a successful start interrupts only that exact
owned run and reloads the Session cache. A losing start cannot interrupt another caller's winner.
Stop or replacement already closing the request is preserved. A live process does not leave a
failed setup stuck in Started. Concurrent projection losers re-read state on typed Stale, Busy
or CAS conflict. A typed transient broker Unavailable or Uncertain error logs an explicit
diagnostic and keeps the stored conversation readable. Its requirement projects as uncertain
with no scope or auto-admission. All other broker and storage errors still fail the read.

Existing integration.run approval, nonce, external ledger and ambiguity guards remain unchanged.
Connect admission is not operation approval. Hand claim's exact connection_ref only to the saved
connect action, which returns metadata. Consumer never lists and auto-picks an account after claim.

All chat control enters the Engine admission primitive detailed below, then compares request
revision and current work. No mutex is held during broker/network/model waits. A new instruction
or Stop closes a waiting request before later auth can resume it. If resume admission wins first,
the later instruction follows durable running-turn steering and Stop follows cancellation.
Opaque broker scopes may outlive local closure until expiry but cannot admit local work again.

## Four concrete lifecycle mechanisms

### 1. A typed pause, not a successful answer or crash

Add `HarnessOutcome::awaiting_connection: Option<AwaitingConnection>` and matching typed event.
`AwaitingConnection` identifies request/invocation/work. The normal completion value is absent.
Before a marked acquisition the harness flushes all earlier queued reads. It never puts the
acquisition in a parallel tool batch. Engine prepares the pending request and returns this
typed stop outcome candidate; persistence happens in Session's initial pause transaction below.
The harness stops processing the remaining calls in that model response,
never dispatches them, and skips final-answer generation, critic, finalization and finding-note
creation. No success/failure tool_result is fed to the model because no further model request
occurs before access is obtained. Earlier trace and tool results remain saved. On resume the
model sees the completed metadata result through bounded progress, not a fictional past success.

The initial pending transaction materializes the original full waiting turn, including its
input/goal/progress and `incomplete = true`, `error = None`, no cancelled/interrupted flags,
`pid = None`, and `Turn::connection = Some(Waiting { request_id })`. It also advances the actual
session header count once, in that transaction. `Session::record_turn` branches on the typed
outcome and performs that initial atomic pause transaction; exact replay only reloads/CAS-replaces
the already-counted row. It MUST NOT call push_turn
or increment header count again. Final journal flush may enrich the row/trace only if the same
request/work/revision still owns it; otherwise it cannot overwrite a newer input or closure.
Recovery preserves this materialized counted waiting row without marking it interrupted.
This closes the pause-commit/crash/new-input overwrite window. Later resumed attempts replace
the counted row through replace_resumed_turn. The run guard drops and Host clears conv.running;
no LLM capacity or Interaction mutex remains held while the person signs in.

Host recognizes waiting before generic turn-success handling: no successful completion push,
no generic crash Continue, no failed result. Shared activity says waiting for the named app.
The waiting question retains acquisition title/Connect semantics. The new requirement event
causes addressed snapshots to refresh independently of message count.

Waiting presentation comes from the owned request projection and typed live event, not an
invocation status alone. Legacy invocation rendering may show pending approval or not run.
PendingApproval remains excluded from cut-short recovery and automatic dispatch. Every durable
Timed event serializer normalizes AwaitingConnection to the existing StateUpdate tag, including
live Journal writes. Exact R3 rollback and returned-root checks are documented separately in
APP_CONNECTION_REQUESTS_ROLLBACK.md. Direct non-chat Agent execution refuses contextual pause.

### 2. One durable admission transaction below every chat entry point

Add Engine-owned `Arc<ChatAdmission>`, shared unchanged in Engine::new/scoped/for_work and every
other derived Engine constructor. Its short per-session mutex
protects in-process ordering; durable CAS is authoritative across stale handles. It is never
held across await/network/model calls. `Engine::admit_chat` accepts Input, Steer, Stop or
ResumeConnection. Each intent has an opaque id for response-loss reconciliation.

The store adds one used transaction primitive, `Store::transition_chat`, with exact preconditions
on session header JSON, current turn index/work/connection marker and request local revision/phase.
It writes the replacement request, actual session_turn row, session header, optional invocation
row, intent record and optional client mutation receipt in one SQLite transaction. It uses
conditional updates and checks affected rows; any failed predicate rolls back everything.
It does not merely write a pointer into the generic records table. Runtime constructs the typed
rows; Store remains unaware of runtime DTOs. No network happens inside this transaction.
Use SQLite IMMEDIATE transactions and a bounded five-second busy_timeout for cross-process WAL
contention. Each conditional update must affect exactly one row or roll back. Preserve
session_turns.text_len exactly as put_session_turn computes it. Timeout returns typed retryable
busy and no accepted control or receipt; never turn contention into stale success.

Existing Session::save and push_turn must also stop blindly replacing headers. Add
Store::put_session_if(expected_loaded_header_bytes, replacement_header), with equality against
the exact loaded persisted JSON bytes, never only a count inequality or loose timestamp test.
Session retains that opaque loaded version and updates it only after successful conditional
write/reload. Route every existing header update through this helper. Other Store header writers
must either create a previously absent session or use the same exact predicate. A stale save
returns typed conflict and cannot reduce count/title/usage after contextual admission.

Input while waiting closes the request as replaced and atomically inserts the new real input's
placeholder at current header.turn_count plus an intent record. Stop while waiting atomically
closes the request, clears the awaiting marker and marks the original turn cancelled. Steer
while waiting follows Input replacement policy. There is no missing-input crash window.
Input/Steer during Admitted but not Started returns typed Busy before mutation admission, counter
consumption or success acknowledgment; the client retains its draft. No durable control inbox or general chat queue is
introduced. Stop in this window atomically cancels admission and prevents its start CAS.
After Started, Input/Steer uses the existing running-turn control semantics and undelivered
message handling. Stop durably cancels the contextual marker before waking the attempt token.
This change adds contextual fences and no broader guarantee for existing attempt-local queues.

ResumeConnection CASes current header/current original work, Turn.connection::Waiting, exact
request revision and Acquired phase. It creates a random admitted run id, writes that id and
Admitted phase into the original turn and request, and finishes the originating mutation receipt
in the same transaction. SDK receives an unforgeable private-constructor AdmittedConnectionRun.
`Engine::start_admitted_connection` CASes the same row to Started with writer identity and returns
the exact existing Turn. Started commits only after matching run-local steering/cancel handles
are installed and addressable under the run id. SDK consumes this row and MUST NOT call
begin_turn. A failed start CAS
runs no model or adapter. This is an existing-row transition; no new turn/index/header count is
invented. SDK validates and reloads instead of trusting an in-memory Session clone.

All paths converge here: Host legacy `send`/`steer`/`cancel`, addressed `chat_send`/`chat_steer`/
`chat_cancel`, typed client mutations, local CLI control, Interaction::run_turn and SDK
ask_session/resume_connection. Existing Session begin_turn delegates to this admission helper;
an already admitted Input token consumes the persisted placeholder instead of overwriting it.
SDK gains fallible `control_session` for contextual Steer/Stop fencing. Host/CLI use it before
waking existing run-local Steering/CancelToken. Bare Steering/CancelToken remain attempt-local handles
and cannot revive or replace a persisted waiting request after the attempt returns. Do not bind
fallible storage writes into their infallible push/cancel methods or swallow storage failures.

Recovery rules: Admitted and not Started can be dispatched once. Started with dead writer is
Interrupted and needs existing explicit recovery. Any closed/replaced request or changed work
prevents pending completion and admission even after broker auth succeeds. Pending original
input is not lost when closure or claim races a newer input from any entry point.
Engine::recover_interrupted and Session::load explicitly scan Turn.connection::Started rows,
including index == turn_count - 1, and test writer_alive(pid,pid_start). A dedicated transition
CAS clears that row's connection marker, sets incomplete/interrupted state and request
Interrupted in one transaction. It preserves original index/count and never calls the existing
begun-turn push path. Preserve invocation recovery/cut_short semantics separately.

### 3. Engine enforces the acquisition boundary and consumes only trusted persisted claims

Marked descriptors must satisfy Observe AND Reuse::Volatile. Validate this both in harness
admission and Engine's direct execution entry. Do not trust a consumer to set volatile correctly.
Marked acquisitions bypass generic evidence reuse and per-digest repeated-failure suppression;
their request lifecycle and broker state govern retries. No broker/source/current chat work means
typed unavailable, with no adapter dispatch. Plan mode does not offer or start acquisition.

Generic Engine::run_action cannot bypass this path: marked actions return typed
contextual-acquisition-required refusal. Only harness preparation produces a typed pause
candidate; only Session commits it with the actual waiting row/header and final attempt metrics.
It never dispatches the adapter with connection_binding=None. A marked
Mutate/Read descriptor is always invalid, even for direct callers and allow_mutate=true.

`complete_connection_acquisition(request_id)` is crate-private. It takes NO public binding argument.
It loads a receipt saved only from the injected broker claim result. Under request/work CAS it
verifies scopeRef, requestId, invocationId, routeId, offerId, fingerprint, current action/input
digest and descriptor schema/marker/risk/reuse. Offer/fingerprint replacement closes as replaced.
It dispatches the exact saved metadata call with the original invocation id and that typed binding.
Only this private path populates RunCtx.connection_binding. Generic run_action always uses None
for unmarked operations, including integration.run. Claim for offer A cannot complete offer B.
Adapter completion is metadata-only and may be repeated after a crash; success atomically stores
the original invocation as ordinary Ok, its evidence result and request Acquired phase. Unknown
external effects returned from this metadata action are rejected and cannot enter auto-resume.

### 4. Broker refresh, local revision, recovery and deletion

Prepare returns scopeRef/brokerRevision/expiresAt from the exact persisted identity. Resolve and
claim fields follow the separate broker JSON above. On every successful fresh resolution compare
the validated tuple (scope identity, broker revision/state, fingerprint, selected binding/receipt)
with the last stored tuple. A change CASes the snapshot and increments local_revision exactly once.
Unchanged reads do not advance local revision. The public expectedRevision compares local revision;
claim uses stored brokerRevision. Exact receipt replay is reconciled independently of old expected
broker revision. Fingerprint change closes local request as replaced. Terminal scope response
closes it with matching cancelled/expired/unavailable reason. Transport unavailable retains waiting
and a safe retry state; uncertain never completes metadata or starts a continuation. Broker stale
re-resolves and emits current state; no authority is inferred from the phone's browser return.

Authorized chat reads and `chat.connection.continue` perform bounded fresh resolve. A Ready result
queues at most one metadata completion/resume admission through the shared CAS; native refresh on
Safari dismissal therefore needs no synthetic send. Startup reconciles Preparing/Waiting/Claimed/
Acquired/Admitted records and applies the same bounded path. It does not auto-start Interrupted.
No poller with unbounded retries is introduced. Native continues to refresh the pending snapshot
while its scoped Account flow is active. A broker notification, if later added, must use this
same admission path rather than creating a second scheduler.

Keep closed/completed request tombstones for the originating chat lifetime. Remove scope/binding
details from terminal public projections; retain only IDs, final local revision and reason locally
as needed for stale rejection. Permanent chat deletion atomically closes/removes its pending
records with the existing deletion path; later scope callbacks have no matching work and refuse.
Broker retains its own finite-expiry/tombstone policy; local deletion does not resurrect its UUID.
Account/route revocation makes injected resolve/claim fail closed. No new cloud cleanup authority
is inferred from the Core interface.

## Wire contract

```text
ChatConnectionRequest {
  id, offerId, display, state, revision: DecimalString, scopeRef?: String
}
ConversationSnapshot.connectionRequests?: [ChatConnectionRequest]
ChatConnectionContinueInput { requestId, expectedRevision: DecimalString }
MutationRequest.operation = "chat.connection.continue"
MutationRequest.target = exact chat id
```

Wire revision is local_revision. States are preparing, waiting, connecting, ready, continuing,
completed and closed. Ready requires fresh successful broker resolution. Only waiting/connecting/
ready may expose scopeRef. Unknown state disables actions. Missing or empty arrays preserve old
behavior. Advertise `chat.connections` only on the addressed route with both optional services
configured. Scope checks never fall back to origin. Existing epoch/counter ordering and terminal
receipts handle response loss. Stale revision creates no new invocation or continuation.
Generate schema, fixtures and Swift through `scripts/protocol.sh`; no handwritten DTO copies.
Old descriptor JSON decodes the marker as false. Old Turn JSON decodes connection as None,
including through readable_turn fallback. Old-client snapshots ignore the optional array.
New-client/old-runtime capability absence suppresses the CTA and unsupported mutations remain
refused. Persisted turn_count includes the original waiting turn once and no artificial resume
turn; trace indexing, title and usage tests assert those exact semantics. Generated Swift is
byte-compared against fresh authoritative schema generation.
Every requirement transition emits the existing conversation-affecting change event, even when
message count is unchanged. A native client refreshes the addressed snapshot. Missing capability
on an older runtime yields no Connect CTA. Native scope resolution goes only to the Account
client's fixed origin, never a URL supplied by the runtime.

## Crash and concurrency matrix

| Boundary | Recovery |
|---|---|
| Before initial pending transaction | No acquisition dispatched or external effect. Existing unfinished work remains recoverable. |
| Pending saved, prepare not called | Retry exact prepare using persisted ids. |
| Prepare committed, response or scope save lost | Exact prepare returns same scope. Conflicting ids refuse. |
| Waiting at restart | Same request id and chat. Resolve current broker state. |
| Resolve unavailable, scope revoked/expired, route/workspace mismatch or offer replaced | Fail closed before claim or connect completion. Keep recoverable failure or close with safe reason. |
| Claim committed, response lost | Same exact claim returns immutable receipt and binding. No provider operation exists here. |
| Binding saved, connect result not saved | Reserve a persisted metadata retry deadline before dispatch. Complete the same Observe call with the exact binding after cooldown and fresh resolve. Metadata-only adapter may repeat safely. |
| Result saved, continuation not admitted | Admit one deterministic continuation; create no second invocation. |
| Admission committed, start CAS not committed | Find the same Admitted marker and start it once. No new turn or message. |
| Started committed, process dies before or during model continuation | Mark Interrupted and require explicit ADR 0048 recovery. Never automatically replay Observe calls or other effects. |
| Two clients continue same request | Store CAS plus broker CAS yields one binding, invocation result and continuation. |
| Stale local revision or resolved/replaced requirement | Reject without new effects. Snapshot supplies current state. |
| New instruction or Stop races with auth/claim completion | Shared chat admission order and final request/work CAS choose replacement/cancellation or continuation admission. No stale original work resumes after replacement, and no user message is lost. |

## Verification and build plan

Use deterministic metadata adapter/broker fixtures and actual client-listener TCP requests.
Cover old defaults, unchanged local acquisition, marker/risk enforcement, same request at restart,
unavailable/mismatched/revoked scopes, independent local/broker revisions, replaced offers,
concurrent clients, prepare/claim response loss, every crash boundary, one user message, one saved
connect invocation and one continuation admission. Have the model fixture attempt a prior mutation
again and prove done-before returns the stored result without dispatch. Uncertain effects cannot
be contextual requests. integration.run stays a separate freshly gated operation. Existing model,
provider-auth and push listener regressions remain required.
Exercise new instructions and Stop on both sides of auth/claim completion. Assert no stale
continuation and no missing user message. Check requirement-only snapshot refresh events and
both old-client/new-runtime and new-client/old-runtime compatibility.
Also test Busy response loss/retry with an unchanged mutation counter and retained native draft,
Stop before start, controls immediately before/after Started, stale Session::save after atomic
pause admission, two Store handles contending for the same revision, and dead Started recovery
at turn_count - 1 with unchanged count/index. All broker resolve response identity fields must
match the persisted scope/request/invocation/route/offer tuple, including exact claim receipt.

Focused tests and clippy precede authoritative protocol generation and one stable full
`scripts/ci.sh` gate, including workspace tests, release build and simulated benchmark. No paid
model calls, deployment, phone activation or hosting changes. Focused checks have run; current
results and unverified source changes are recorded in `CURRENT_STATE.md`. Parent
allocates each guarded build window/cache first. `CONTRIBUTING.md:44` permits reduced debug info
and no incremental output. All inherited provider/model/push/headless deltas remain preserved.
Root and QA approved the initial interfaces and the bounded metadata retry design.

## Metadata retry after a confirmed claim

The opaque request record adds optional `metadata_retry: { attempts, next_at }`. The counter
saturates at `u32::MAX`. The timestamp is RFC 3339. Missing data preserves the original request
shape. There is no new persisted phase, invocation status, wire DTO or authority field.

Ownership, local scope expiry and descriptor replacement checks run before cooldown. A future
deadline returns the payload-free typed `MetadataUnavailable` error without broker resolve or
adapter dispatch. An eligible attempt first resolves current broker state and validates the
immutable claim. A request/turn/header CAS then reserves its attempt and deadline before
dispatch. Delay grows from one second to a thirty-second cap, with no terminal failure count.
Only the reservation winner proceeds. The saved invocation and claim receipt are unchanged.

Adapter errors become `MetadataUnavailable`; their raw detail is not logged or returned.
Host keeps the original conversation readable with state `uncertain`, no scope action, and a
fixed `metadata_unavailable` diagnostic. The stored deadline survives restart. A successful
metadata result clears retry data atomically with `Acquired` and the existing invocation result.
Invalid results containing commands, effects, pictures or produced artifacts remain terminal.
No provider write is retried by this mechanism. A crash after reservation can delay the next
safe metadata attempt, but does not authorize replay of the later model continuation.

Old R3 ignores the request's opaque record kind and still sees the known invocation/event
shapes. Actual R3 read-back of a newly produced retry payload remains required. Earlier retained
rollback evidence does not cover this added payload. Focused metadata retry SDK and HTTP tests
pass. The separate concurrent-rename regression reproduced a header conflict. Its approved
atomic Busy correction passes the full focused SDK suite and Store race tests; the full current repository gate is not green. See
`CURRENT_STATE.md` for remaining checks.

## Listener-owned connection maintenance

Conversation GET projects stored requests and fixed Host-local diagnostic state. It does not
await broker calls or metadata adapters, and does not admit continuation itself. The listener's
startup and 250 ms tick schedule maintenance without waiting in the accept loop. A shared Host
try-lock permit admits one maintenance job across listeners without queued duplicates. A batch
refreshes at most four chats concurrently. Per-request Host-local polling starts immediately
after restart, then uses one-second pending polls and capped one-to-thirty-second failure backoff.
There is no new persisted polling field. Durable metadata retry authority is unchanged.

All prepare, resolve, claim, cancel and metadata futures have ten-second bounds. Fixed transient
diagnostics contain no transport or adapter payload. Typed broker refusal closes only an unclosed
request as unavailable. Existing closed cancellation reasons are not rewritten. Store corruption
and serialization errors remain errors, not successful refreshes. Before continuation admission,
the Host obtains the idle conversation and checks its selected model with a non-waiting lock.
An unavailable selected model leaves the request acquired without running a fallback.

ClientServer has additive awaited shutdown: signal the listener, abort its request and maintenance
jobs, then drain them before returning. Drop sends the signal but does not claim synchronous drain.
The generic served/JoinHandle tuple remains unchanged; aborting that raw handle retains its earlier
JoinSet cancellation behavior. Phase A passed its focused SDK and HTTP suites, selected Host/serve
checks, serialization and Store race cases. This is not a full repository gate.

## Owned Stop finalization and normal setup failure

Phase B source retains the existing Started turn marker only when its exact Started request is
closed as cancelled. The outer writer pid is cleared. This marker proves who may finalize; it
never permits admission or continuation. The same-turn finalizer requires the original marker
tuple, request/session/project/work, latest turn, exact header and row/request CAS. It keeps the
request closed, suppresses a new prerequisite, preserves the input and merges citations and
metrics. SDK normalizes the returned outcome from the persisted cancellation result.

Reopen clears this marker only for the exact dead writer and closed cancelled request, through
the same atomic CAS. Header JSON and request JSON stay unchanged; the Store transaction may
refresh its SQL session bookkeeping timestamp. It does not resume the request.

A normal admitted input has an opaque in-memory proof of its exact header, placeholder and
admission identity. A setup error can count that input as failed only while the same proof is
outstanding and the persisted header and row still match. Consumed proof clones cannot affect
a later admission. Cleanup conflicts retain newer work and return both errors. Unread posts
remain unacknowledged because no model consumed them. Resumed setup keeps its separate
interruption path. Phase B passed the full focused SDK and connection HTTP suites, admission
setup, Host, projection, CLI Stop, serialization and Store race cases. The latest required
debug prefix also passed format, protocol, version, workspace clippy and workspace tests.
Release build, benchmark and an exact-R3 read of the stopped unfinalized boundary remain pending.

Hosted runtime resolve transport is confirmed: `GET /v1/runtime-apps/connection-requests/:scopeRef`
with exact requestId and invocationId query parameters, authenticated only by RuntimeAppGrant.
It returns fresh state, offer fingerprint, brokerRevision and exact receipt. HostedApps exposes
only this restricted alias proxy. Account resolve stays at the fixed API origin with requestId
and routeId. No Account bearer is passed through runtime. Browser return never proves ready.
