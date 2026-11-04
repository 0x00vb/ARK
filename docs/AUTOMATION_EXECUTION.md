# Automation execution design

Implementation design for ADR 0094. Independent design gate GO on 2026-11-01. Implementation gates remain open.

## Data

| Record | Identity and contents |
|---|---|
| Definition head | Project + automation ID. Current revision, enabled state, schedule reference and activation digest. ScheduleRecord owns the next occurrence. |
| Revision | Automation ID + revision. Immutable name, trigger, inputs, sequence of 1 to 8 steps and authority. Canvas positions are stored separately. |
| Step | Stable ID, Agent / Approval / Result variant. Inputs bind literals or earlier text/file outputs. No forward references or branches. |
| Run | Deterministic project + automation + trigger origin + request key. Frozen revision and inputs, WorkItem and conversation, per-step phase/receipt/output, execution lease. |
| Event admission | Project + automation + stable endpoint + authenticated source/account + external ID. The v1 endpoint addresses one automation, not a fan-out subscription. Payload digest, frozen matching revision, run identity. Same ID and digest recovers the original admission across publication/pause; a changed digest conflicts. |
| Approval | Run + step + revision digest. Pending/allowed/denied, existing ApprovalDecision vocabulary. Stored before it becomes visible. |
| Effect accounting | Run + actual invocation ID, action and canonical input digest, plus account/window quota. Prepared/running/confirmed/uncertain states. Counter reservation and prepared effect commit together before execution; reusing that invocation never charges twice. Descendants share the same run ledger. Read failure or an uncertain/prepared effect blocks a new automatic mutation. |
| Canvas layout | Automation ID + layout version + positions keyed by stable step ID. Separate from execution revision and authority digest. Moving a node changes no authorization or execution semantics. |

A run is Pending, Running, WaitingApproval, Completed, Failed, Cancelled or Interrupted. Step phases never infer execution from a UI click. A receipt can exist before the step starts.

## Caller usage

Save a draft revision with an expected revision precondition. Saving keeps an existing active revision running. Prepare activation resolves exact model/account selections and freezes the reachable agent specifications without making model calls. The person reviews and activates the digest covering revision, resolved choices and hard grant. Scheduled/webhook admissions copy those choices from the active grant, so queued runs never resolve a later default. A manual run without activation performs this preflight before its durable run admission and requires live approval for mutations. Run now with a stable request key. Inspect the returned run through Work and the automation's history. Retry admission with that same key after a lost response.

The coordinator acquires one durable run lease. It creates the run's WorkItem and root SessionRecord in one transaction. WorkItem has an optional run binding. The coordinator owns that run's final state. Step completion never closes the WorkItem; work_follows defers finalization for a bound sequence. Work projects the run phase before individual completed steps, so an approval after step one cannot appear Completed.

Each Agent step uses the same WorkItem and a stable run/step request key, but gets an independent execution session and store scope. The scope shares only the root workspace mapping and explicitly passed artifacts, not another step's research state or conversation. It does not carry the root conversation digest or another step's session into its model. A trusted admission context selects the step session; a browser cannot supply a foreign conversation or override frozen step inputs. No conversation is copied into the definition. Each step receives only explicit inputs and bound outputs from the immediately preceding step.

The root conversation is the visible execution history. After a step finishes, the coordinator journals its public input, result and source task into that conversation. This mirror is idempotent and is never used as another step's model context. Execution sessions reference their root and are inspected as steps rather than listed as separate root chats. Delegated children inherit the run and authority; their briefs do not inherit a transcript. Run and authority references are stamped from the trusted parent on the server, never from model arguments. An opaque external program without an enforceable per-call or containment seam fails explicitly before execution. Provider and SessionBackend expose host_controls_tools with false by default. Wrappers preserve the contract. Preflight checks both router tiers and the Harness checks the exact captured provider before each call. HTTP providers and the tools-disabled Claude path implement it. Qualified Codex uses the app-server contract in ADR 0098. It requires the audited official native build, current account metadata, an explicit model and effort, and a private tool-free model catalog. Stateless calls use ephemeral app-server threads. Exec fallback and legacy or foreign conversations are refused for qualified work. Unsupported binaries retain ordinary chat compatibility, without protected automation eligibility.

An Approval step persists a question and returns. Answering its exact question ID commits the decision before the next step can run. A Result step deterministically retains its bound text and files. It makes no extra model call.

Restart observes a live owner. A dead owner resumes only from durable state. An already admitted step recovers its receipt. A completed step is never executed again. An unfinished invocation with uncertain effects requires a human decision; it is not retried automatically.

The scheduler uses When normalization and next_after. ScheduleRecord remains the authority for schedule identity, next_ms, skipped/missed occurrences and history. Add an optional target with a serde default: an ordinary agent task remains the legacy target; an automation target dispatches a sequence. Opening or listing does not migrate a schedule. Its library projection is a one-Agent sequence with a Result. Explicitly saving edits can adopt that schedule into a definition while retaining the same ScheduleRecord ID, next_ms and history.

Manual execution does not move next_ms. Every dispatch, including the old tick and run-now paths, revalidates the durable target and occurrence slot before admitting work. Due admission and cursor advancement commit together. A cached timer cannot execute a target that has changed. Existing in-flight work is observed and does not cause another run.

Webhook ingress verifies an independent secret/signature, bounded timestamp, stable event ID and body size before admission. The route supplies project, endpoint and automation identity; payload fields cannot choose another account or definition. New admission captures the active revision at the transaction boundary. A verified replay finds the prior admission before considering a newer revision or paused head. Production addresses use HTTPS. Loopback HTTP is allowed for a local runtime. Quota admission is store-wide, transactional and fail closed. A queued admission remains recoverable if dispatch stops.

## Authority and A0/A1 reuse

Compare two implementations:

| Option | Assessment |
|---|---|
| Merge the event-automations branch | Rejected. It includes unrelated mail/messaging providers, a separate event-only product model, and known quota/delegation issues. |
| Reuse independent seams in the current runtime | Chosen. Store writer already integrated. Bring provider-neutral event normalization and per-call guard, then bind them to sequence revisions and Work receipts. |

The guard runs before reuse, approval shortcuts and every invocation, including meta calls. It narrows the person's standing grants. Descendants inherit the same run authority and counters. Delegating does not mint another allowance. Admission freezes the exact activation grant and revision in the run. Publication and pause affect future admission, not a run already admitted. A separate explicit revocation invalidates the frozen grant for further effects and pauses affected runs. Unattended mutations require a supported, bounded action within the run's frozen grant. Authority has two levels. Hard bounds on actions, inputs, paths, processes, network and quotas always refuse a call outside the frozen run grant. No standing grant, Auto mode, --yes or live approval can widen them. Within those hard bounds, the unattended permission is separate: a mutation without that permission asks for a live approval. That approval satisfies only the per-call permission. An action without verifiable containment cannot prove a required hard path/process/network bound and is refused before effects. Person-only operations never become unattended.

Internal action discovery and evidence reads consume the same durable run call budget as adapter calls, across steps and delegation. Evidence stays scoped to the executing project. Read arguments allow at most 100 results and bounded query strings. Model-requested record_note and record_state writes have no guarded effect contract and are refused. The coordinator still records execution results and status.

A genuine authority refusal retains the first run fault, stops later actions and model requests,
and fails the run even if a model offers a final answer. This includes unapproved actions or
inputs, changed grants and exhausted quotas. Malformed permitted internal read queries and
limits remain correctable without spending quota. Ordinary adapter validation is unchanged.
This does not infer semantic success for every text-only answer. See ADR0101.

The model sees only the intersection of its existing actions and the frozen grant.
That registry is fixed after all adapters are registered, including for named and delegated
work. Derived tool hints and skill catalogs do not advertise removed actions. Protected
work cannot acquire new capabilities. Execution guards still enforce every input bound.

Do not import donor fail-open quota reads, per-orchestrator quota mutexes, blanket delegation refusal or unverified path-prefix permission checks. Canonical input must be the input actually invoked. The harness guard governs meta calls and approval shortcuts; the engine reserves effect accounting against the actual invocation identity before invoking the adapter. A prepared/running record that lacks a confirmed result after a crash is quarantined as uncertain, rather than silently reserving again. Path-changing actions without a sufficient lower-level containment contract are refused when the run requires a hard path bound. A live approval cannot substitute for containment.

## Verification

Test revision conflicts, eight-step limit, typed/backward bindings, duplicate triggers, quota read failure, simultaneous orchestrators, delegated authority, process death before/after admission, approval restart, cancellation and retained results. Use real app manual, time and authenticated webhook runs with a real provider. Then run complete repository gates and independent implementation review.

## Editor gate amendment, 1 November 2026

Independent design review: conditional GO. Steps and Canvas expose only consecutive bindings. An Approval step passes exactly its reviewed typed text and file inputs after approval. Refusal passes nothing and starts no downstream step. The approval digest includes the frozen values shown to the person.

Saving a draft does not activate it. Show saved revision and active revision separately. Activation confirms “Activate revision N”, with the frozen models, hard bounds, rate limits and exact unattended permission. Per-execution questions offer only one-use approval or refusal. Pausing stops future admission. Revocation invalidates the admitted grant for further actions and step starts.

The schedule editor includes an IANA time zone, Core-validated recurrence and three next occurrences calculated by Core. Reading a legacy schedule does not adopt it. Adoption is explicit and retains its ID, next occurrence and history. Trigger changes commit atomically so only one ingress remains active.

Webhook setup shows independent credentials, endpoint and actual reachability state. Authenticated preview does not execute. URL syntax alone does not establish a publicly reachable HTTPS endpoint. Loopback development stays visibly local. Verified replays retain the originally admitted revision through pause or trigger changes.

Webhook transport details and the active/draft credential boundary are specified in [AUTOMATION_WEBHOOKS.md](AUTOMATION_WEBHOOKS.md). Schedule activation stores a cutover watermark. An unadmitted occurrence at or before it is skipped under the new target; an already admitted run retains its original revision. Keyed manual schedule starts retain a durable receipt and never advance next_ms, including legacy schedules.

## Recovery and legacy schedule admission, 1 November 2026

A guarded mutation stores its canonical PendingApproval invocation before asking the person. The question binds its invocation ID, private scope, work identity and descriptor fingerprint. Recovery validates and reuses the retained question, including its original approval class and cut_short metadata. A compare-and-set claims the invocation once. A losing contender does not fault the winner. A prepared reservation retains its original quota windows; recovery cannot obtain a fresh allowance. Running effects without a confirmed result remain blocked. A completed invocation reconciles its ledger before another model decision. Failed or unconfirmed mutations do not resume automatically.

Step sessions set execution_managed. Generic startup recovery does not advance their begun placeholders. Their coordinator repairs the result journal or resumes the exact pending action before making another model decision. Hard process-exit tests cover General and an existing agent at the approval boundary, plus exit after an Ok invocation is durable but before ledger confirmation. These tests use synthetic providers and are not real-app acceptance.

Legacy dispatch now performs asynchronous model preflight and admits its Work receipt, task, private conversation, item binding and schedule cursor in one transaction. TaskSpec.item keeps the existing item. Each occurrence gets a different conversation and execution scope. Explicit followup receipts select a conversation only within that item and target. TaskSpec context, budgets, dependencies, rules, artifacts, work and report_to remain part of admission. Result mirrors use an idempotent post receipt and require an existing destination conversation. A preparation failure preserves the occurrence with a visible error and durable retry time; three such failures pause the schedule. Resume clears this preparation backoff.

## Recovering a manual start

Before admission, the editor retains the exact request key, revision and inputs in a scoped
runtime record. Opening the editor restores that pending request before enabling actions.
This survives a runtime restart and a different browser origin or serve port. A concurrent
client receives the existing pending request and must recover it before starting another.
Retry uses that exact request. Recover start atomically returns its existing Run or fences
that request against late admission. A successful receipt acknowledgement clears the pending
runtime record. Neither preparing nor cancelling an unadmitted start creates Work or calls a
model. The runtime remains the authority for Run and Work data.

Acknowledgement retains the resolved request, Run and Work receipt durably. After a lost
acknowledgement response, reopening offers Open last run and Run again. A new start must name
the last Run the client observed; a stale client is refused. An already admitted request still
returns its original Run. Rust callers use admit_manual_automation_run with ManualStart and
acknowledge its receipt, or prepare an intent explicitly before low-level manual admission.
A manual admission without a registered intent is refused before creating Run or Work.

## Incomplete definitions

Saving validates bounded storage shape, stable step identities and payload limits. Names,
prompts, ports and references can remain incomplete. Validation returns structured issues for
the definition or a step. Preparing activation and manual execution validate the complete
revision and current project references before resolving a model. Invalid drafts never create
a Run or Work. Save recovery uses the same storage validation and canonical digest as Save.

## Activation-specific input constraints

A schedule cannot activate with required execution inputs because its timer supplies no
input payload. Validation exposes this beside Start before activation preparation. Saving
the draft and an explicit manual Run now with supplied inputs remain available. The
activation transaction repeats the constraint. Webhook input preview validates declared
names and bindings without admitting work.
