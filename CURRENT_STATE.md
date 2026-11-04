# Current state

## Contextual app connection requests (2026-10-26)

Isolated work in `codex/app-connection-requests` retains the original chat and person message
while an opted-in Observe metadata acquisition waits for scoped access. Durable request and
turn admission, exact claim receipts, Stop and replacement fences, and same-turn continuation
are implemented. Existing external mutation approval and invocation handling remain separate.

All required Core CI stages are now green through split execution. The required debug prefix
passed format, generated protocol, version, all-target workspace clippy with warnings denied,
and workspace tests: 1,169 passed and 11 existing ignored across 115 completed groups. Optional
page-enabled checks were not enabled. The workspace release build passed in 7m 35s. The
simulated benchmark passed all 22 workloads; non-timing aggregate metrics match the committed
baseline exactly. Simulated timing is not production performance evidence, and simulated cost
accounting is not paid model usage. This is not a claim of one uninterrupted `scripts/ci.sh` run.
Evidence: `/tmp/ark-core-required-debug-browser-fixed-20261026.log`,
`/tmp/ark-core-verification-81XoBW/workspace-release-receipt.json`, and
`/tmp/ark-core-sim-benchmark-rbM6sM/verification-receipt.json`. Their logs, resource samples,
candidate and comparison are retained. No baseline was changed.

The current stopped-boundary fixture also passed the actual exact-R3 old reader and current
SDK returned-reader round trip. The returned-reader check passed 1/1 without provider, broker,
adapter, message admission or dispatch. It verified exact closed/cancelled ownership, marker-only
dead-owner cleanup, retained input, metrics, citations, credentials and request/header JSON, and
idempotent reopen. SQL bookkeeping timestamps may change. The old-reader comparison permitted
exactly one genuine R3 `runtime/self` creation if absent, not arbitrary row changes. Evidence:
`/tmp/ark-contextual-stopped-qw7l_z2t/compiled-fixtures-manifest.json`,
`/tmp/ark-contextual-stopped-qw7l_z2t/current-reader-return-stopped/result.json`, and the retained
`rollback-stopped-new-reader-proof.json` under that returned store. Independent artifact QA
approved this proof. All 1,022 source files stayed frozen through verification and the parent
Linux snapshot seal. The exact SDK/UI proof binaries remain preserved outside the target.
The fixture was created by the current SDK, not R3. It has no turn trace; R3 returned null.
The old HTTP invocation comparison excludes only the additive `connection_request` field.
The full stored invocation row stayed exact. This does not prove a populated-trace upgrade.

Root cleaned only the owned inactive debug and release profiles after their readers released
them. Required debug libraries were retained until the consumer check passed. Logs, receipts,
benchmark output and sealed proof binaries were preserved. Actual private activation and the
same-chat iPhone GitHub OAuth and counter checks remain pending. The feature is not complete;
vstack remains at VERIFY, step 4/6. No commit, push or public release is implied.

### Implementation and verification history

The entries below record earlier checkpoints and failures. Their pending statements describe
those checkpoints, not the current gate status above.

Scoped source reviews approved the bounded fixes. Focused checks pass: SDK 13/13, connection
HTTP 7/7 and waiting-event serialization 1/1. The unchanged native-client HTTP case then exposed
two inherited active-turn lock waits. Reviewed corrections preserve pending send receipts and
read stored chat metadata for idempotent create retries. That case now passes 1/1. The seven
connection HTTP cases have not been rerun after those two Host corrections.

A later Sonnet review found that metadata adapter failures could make chat reads fail. The
reviewed correction adds persisted retry deadlines with capped backoff, typed temporary
metadata failure, and a readable uncertain snapshot. The latest HTTP suite passes 9/9, including
metadata recovery, two devices and expiry. A separate concurrent rename case reproduced a
header-CAS Conflict that can discard the running answer. The approved correction refuses
active-turn rename atomically: runtime classifies the latest turn and Store fences the exact
raw header and latest row, without parsing runtime fields or merging stale data. The full SDK
suite now passes 19/19 with no exclusions. Store race cases pass 2/2, HTTP passes 9/9, and the
failed-start Host case and unchanged broader serve case each pass 1/1. The original rename
failure log is kept; the replacement test asserts the approved Busy policy and preserves the
answer. The latest serializer check was stopped by the fail-closed resource guard when `du`
returned an error during compilation. It has no test result and needs rerun. Actual R3 read-back
of the new retry payload remains pending. The focused slot closed at 17:19:04 UTC with no
compiler left running.

The retry fixture was subsequently exported from a fresh exact-R3 seed copy, with actual source
and SDK/UI executable hashes. The focused serializer retry passed. A further source review found
listener head-of-line blocking and missing continuation model readiness checks. Phase A source
now makes GET stored-only, runs bounded listener-owned maintenance with four concurrent chats,
uses local cooldown diagnostics, bounds broker and metadata calls to ten seconds, closes typed
broker refusal, and checks the selected model before admission. Awaited client-listener shutdown
drains maintenance. Phase A focused checks passed SDK 19/19, HTTP 16/16, Host 1/1, serve 1/1,
serializer 1/1 and Store 2/2. Two initial fixture failures are retained. Reviewed test arrangements
settle initial connection state before seeding a large revision and await background receipt
reconciliation without restoring blocking startup.

Phase B retains exact writer evidence for owned finalization, preserves the
closed cancellation fence, and cancels the captured local control even when the durable attempt
fails. Dead-owner recovery clears only the retained marker through exact CAS. Normal preadmitted
setup failures use an opaque in-memory admission proof to keep one failed input without touching
newer work or acknowledging unconsumed posts. A stopped late prerequisite is neither persisted
nor returned as waiting. The small approval projection separates a supplied multiline action
title from its detail. Focused checks passed SDK 23/23, admission setup units 3/3, HTTP 17/17,
Host 2/2, client projection 3/3, CLI Stop 1/1, serializer 1/1 and Store 2/2. Two test-only API
compile corrections and their failed logs are retained. The stopped unfinalized rollback
boundary passed locally, but has not been exported for an exact-R3 reader proof. No complete
current-source gate is claimed.

The current required debug prefix passed format, generated protocol, version and workspace
all-target clippy with warnings denied. Its resource guard then stopped workspace test
compilation because non-target volume growth exceeded the allocated 2 GiB envelope. The owned
process group was drained. This was not a workspace test failure, and no workspace pass is
claimed. The stopped log and resource samples are retained under
`/tmp/ark-core-required-debug-current-20261026.log`. No release, benchmark or external stopped
rollback export ran in that allocation.

A reviewed remaining-target admission formula then allowed a warm retry. Format, protocol,
version and clippy passed again. Workspace tests stopped at the unchanged Browser crash test:
the second explicit open received a local connection refusal. Its fixture ended after three
TCP connections. The precise Chromium request mix was not recorded. A test-only correction
now gives that fixture an owned, bounded lifetime with explicit shutdown and drain. A separate
stopped-request return-reader test verifies dead-owner cleanup without execution components.
The corrected Browser unit suite passed 6/6. Its crash/reopen fixture recorded four connections
in that run, and the idle/extra-connection case recorded six. The original failing run's exact
request mix remains unknown. The full focused SDK suite passed 25/25, including the stopped
return-reader's controlled local branch and missing/changed-marker rejection. No external
stopped rollback proof has been produced yet.

The latest required debug prefix then passed format, generated protocol, version, workspace
all-target clippy with warnings denied, and workspace tests. The log reports 1,169 passed and
11 ignored across 115 completed groups. Optional page-enabled checks were not enabled. The
owned process group was drained at 19:17:23 UTC. Evidence:
`/tmp/ark-core-required-debug-browser-fixed-20261026.log` and its resource samples. Release
build, benchmark and the external stopped-boundary rollback proof remain pending. This is a
split verification of the required debug prefix, not a completed `scripts/ci.sh` run.

The exact R3-created seed passed both new Interaction fixture phases. Exact R3 then read the
real live-Journal, paused-final and claimed databases, including typed history paths. New Core
recovery passed on all three returned disposable roots without a provider, broker, adapter,
new message or dispatch. See `docs/APP_CONNECTION_REQUESTS_ROLLBACK.md` for retained proofs
and limits. These proofs precede the latest source fixes. The earlier full gate passed format,
protocol, version and clippy, then stopped at the missing-chat regression, now fixed in source.
The current full gate, release build and benchmark remain pending. Further compilation needs
the parent session's allocation. No commit, push or deployment occurred.

## Private native notification relay (2026-10-24)

The isolated `codex/native-push` extension is integrated into this candidate without replacing the model and provider authorization changes. Typed external-client registration, presence and tap resolution use protocol 1.8. The runtime uses an operator-configured HTTPS origin and finite pilot expiry. Only a send capability is retained in the exact shared connection secret-store instance. Existing attention notices and chat completion feed a durable, content-free per-device outbox. The first live private binary exposed a missing headless surface: its original gate admitted only dedicated client listeners. The correction requires exact external-client scope on dedicated or account-bound headless listeners. Actual TCP tests prove all four push routes work while owner push and external generic RPC stay refused. Headless startup also resumes its prior chat; three restarts retain one session, while native window startup remains unchanged. Independent source review approved both fixes. Eleven focused push/startup cases and both model/provider HTTP regressions pass. Prior protocol checks passed six cases, and DTOs are unchanged. The corrected bounded Linux release build passes, binary SHA256 `82c890533c21a0f20b55feb7db335b98b7a5fc08cd08a1834fcd6556cd99d76e`. Physical APNs delivery and private activation remain with the parent session. Evidence: `../board/evidence/mobile-native-push-20261024/headless-followup-result.json`. No public release is implied.

## Combined release verification (2026-10-23)

The combined candidate passed full page-enabled CI: formatting, generated protocol, version
checks, clippy, workspace tests including all five Browser cases, all 66 WebKit cases, the
optimized release build and simulated benchmark. Root independently rehashed every source
file and verified the owned process group empty and shared lock absent. The owner explicitly
approved ADRs0078 and0079 for release. The release tag, both remotes, fresh-clone verification
and Desktop consumer checks remain next. Historical failures below are retained.
Evidence: ../board/evidence/coordinated-release-2026-10-21/core-fixture-full-0255-root-result.json.

## Browser fixture correction (2026-10-23)

The last full gate stopped at an intermittent Browser screenshot timeout. The prior
full run passed all four Browser cases and 65 of 66 WebKit cases. The remaining
chat case then passed its focused check after waiting for rendered detail text.
The Browser test server also had a separate defect: an idle connection could block
all later requests. It now serves bounded concurrent requests and owns shutdown.
An idle-first-connection regression joins the four existing Browser cases. Independent
source review passed. The separate five-case check and subsequent full CI both passed.
The fixture defect has not been established as the cause of the prior screenshot timeout.

## Combined gate follow-up (2026-10-23)

The fresh full gate passed the workspace tests and all four Browser integration tests.
WebKit passed 63 of 66 cases. Two assertions still described the old sidebar and approval
status. A browser pane header also missed the compact pointer layout rule. The bounded
correction updates those assertions and aligns the pane header while retaining shared
30-point chat and artifact tab controls. The next full gate passed 64 of 66 WebKit cases.
The remaining composer assertion still expected the old control order. The chart overflow
assertion measured immediately after resize. It now waits for explicit compact layout and
settled pane geometry, retains the no-document-overflow check, and checks internal chart
scrolling with failure dimensions. Scoped source review and actual verification remain open.
The retained-executable focused run then passed the files case. The composer case reached
a later target-size assertion that counted an absent pseudo-element. Its correction measures
the actual 32-point fine-pointer controls and verifies that pseudo-elements remain absent.
Independent source review passed. The corrected composer case and full gate remain open.
The composer then passed its retained-executable check. The next full gate passed 65 of
66 WebKit cases, including every corrected case. App conversation call details returned
empty rendered text; disclosure timing is the suspected cause. The test now waits boundedly for expansion and
the same rendered detail text before retaining its original assertions. Source review and
actual verification of this correction remain open. Release build and benchmarks were not reached.
No commit, tag or release follows from this failed run. Evidence: the workspace board's
`core-page-ci-run-root-core-full-resume-20261023-0105` and
`core-page-ci-run-root-core-webkit-fix-20261023-0120` receipts and logs.

## Coordinated release source (2026-10-21)

The owner requested completion, release and installation across clients. Current main
workingtree includes the participant/UX source, Office discovery and Windows lock diagnostics,
UTF-8 CSV guidance, and destination-side Browser correction alongside committed memory.
The latest attachment-control icon change is reconciled without replacing newer main behavior.
These source integrations are uncommitted. Focused source checks pass 22/22. Targeted
independent Luna and Opus reviews closed the Unicode initials and reconnect Retry findings.
The final combined page-enabled repository gate, final candidate review, immutable tag and
fresh-clone verification remain open.
ADRs0078 and0079 were subsequently accepted by the owner on 2026-10-23. The frozen memory CI does not certify this combined state.
The inference CPU and sanitizer gates have completed. The separate retained-binary Metal
test-only gate passed after configuration regeneration invalidated the earlier attempt.
Historical failed receipts remain preserved. Core full CI needs its own fresh gate window.
Source receipts: workspace board/evidence/coordinated-release-2026-10-21 and team-integration-2026-10-21.

## Combined source integration (2026-10-20)

The frozen functional project-memory candidate is committed and fast-forwarded to local main
as `f0d10f1`. Its full CI and independent reviews apply to that exact candidate. The working
tree retains the pre-existing participant and UX changes; combined compiler, full repository
and native gates remain queued. Memory performance improvement is not established.
ADRs0078 and0079 were subsequently accepted by the owner on 2026-10-23. No push, tag or release occurred.
Evidence: `../board/evidence/team-integration-2026-10-20/core-functional-memory-local-merge.json`.

## Work clarity pass (2026-10-20)

In `codex/ux-clarity`: the sidebar names Working and Needs you, shows pending decisions
on folded projects, and preserves keyboard focus during background updates. File cards have
44px targets and explicit accessible open actions, including after Restore. Reconnect and
detached-window state use the current approval snapshot. Real WebKit regression tests cover
these paths. Consumer native integration and the final repository gate remain separate.
Board `ark-ui-clarity-20261020`. No release or Desktop repin.

## Project memory in development (2026-10-20)

Isolated `codex/project-memory` in `../ark-project-memory`. The existing records table holds
project-scoped reviewed corrections. Shared commands remember, accept, reject, replace and
forget; agent suggestions remain pending behind existing mutation approval. Replacement is
transactional. Harness reasoning and final-answer requests refresh bounded active memory,
including after provider retry delays. `/context next message` previews selection.

Independent plan/design/source QA approved functional behavior. Aggregate efficacy remains
unproven. Store tests pass 9/9, runtime
memory tests 7/7, shared interaction tests 18/18 and proof-grader checks 2/2. The SDK test
covers explicit first use, project opt-in, empty-project index parity and real session requests.
Full `scripts/ci.sh` passes, including workspace/doc tests, release build and all 22 simulated
workloads with exact committed-baseline parity. Actual terminal and page dogfood passed.
The SDK offers suggestions for an explicit request or when active/pending records exist. Safe cleanup recovered 8.490 GiB from eight unused ignored Next
build directories; source status was unchanged and active output was preserved.

Eight real Claude Haiku file tasks completed. Baseline: 4/4 correct, 24 calls, 79.3 seconds.
Memory: 3/4 correct, 24 calls, 82.6 seconds. Both document tasks saved correct reports in
four calls versus five. The replacement coding task recalled the right rule but exhausted its
step budget before saving. Scope, retired-memory exclusion and current-instruction override
checks passed. This proves a narrow document benefit, not overall reliability improvement.
The failed coding attempt needs a separate execution-budget investigation before claiming
broader workflow improvement. Evidence: workspace `board/evidence/project-memory-2026-10-20/`,
including `full-ci-verified.log` and `verification-summary.json`. No commit, main
integration, release or consumer repin. Board `ark-project-memory-20261020`.


## Mobile continuity listener

In development on `codex/mobile-client-listener`: a stoppable client-only listener attaches to
the same live Host as Desktop. Credentials distinguish owner from external client. Legacy access
entries default to owner, while external-client credentials and grants live in separate fields so
an older binary cannot reinterpret them as owner authority. QR and account pairing issue
external-client credentials. Account
pairing binds the verified account subject and requested route id to the listener route. Desktop
still needs to construct `ark-account`'s production route-assertion verifier, register its route and
retain the listener handle across sign-in state. The verifier requires exact ES256 claims, refreshes
unknown signing keys and consumes assertion ids in memory; credential issuance also persists the
assertion-id hash atomically so replay stays refused across restarts. Source review added a
loopback-only cleartext boundary, bounded connections and bodies, signing-key cache expiry and
completion-based JWKS refresh throttling, including empty, invalid, slow and stale-key cases. A
real HTTP integration test signs through a served pairing JWKS, pairs once and refuses replay with
the typed error envelope. The full repository gate passes. Desktop still must wire the listener
into its signed-in lifecycle before account pairing is reachable in production.

## Mobile result files

Protocol v1.3 advertises `results.download`. A paired native client can list up to 50 files
produced by that conversation and fetch the exact bytes. Inputs and rollback copies stay hidden.
Downloads require the paired credential, an exact chat-project-scoped content address, a real
blob no larger than 50 MiB and one of four bounded read slots. The full Core gate passes,
including the release build and benchmark. The iOS consumer and its signed device run are next.

## Hosted process Browser correction (2026-10-21)

HTTP-served Browser panels use the destination Chromium surface even when the page and selected
chat share a runtime ID. Loopback hostnames do not identify where the process runs. Missing
Chromium stays visible and Try again reopens the requested address. The native app's local
Browser view remains unchanged. The isolated branch's 19 focused Node cases passed; they have not
been rerun on this combined main source. The process-page fixture enables Browser explicitly.
Current-main page CI, real same-port viewer/destination proof and native dogfood remain open.
Board `948e2397`.

## Native model-program authorization (2026-10-23)

In development on `codex/mobile-provider-login`: protocol v1.6 adds a typed, account-route-only
Claude connection and authorization surface for paired native clients. The runtime drives the
official Claude Code login, status and logout commands without a shell. The CLI keeps its own
credentials. ARK holds one active attempt and at most 32 bounded in-memory records, validates the official authorization
address, accepts the one-time code once and restricts the attempt to its paired device. Status
requires the exact `claude.ai` authentication method. Independent source review passed after
fixing device scope, listener ownership, closed-channel timer starvation and fail-closed child
cleanup. Authorization tests pass 13/13; serialized serve tests pass 16/16 and the previously
intermittent mutation case also passed alone. A prior parallel run counted a duplicate mutation;
it remains a historical observation, not a claimed fix. Full `scripts/ci.sh` passed formatting,
protocol/version checks, clippy, serialized workspace tests, release build and the simulated
benchmark with no baseline delta. The isolated Linux hosted build passed in
4m56s. Packaged-image smoke passed official Claude authorization start/cancel, process exit,
pairing and scope isolation. The exact image is now healthy on VPS loopback. iOS full checks
passed and its signed artifact is ready but not installed. Real provider login/model work is
unproven and needs the owner's physical iPhone.

## Upload completion drafts

On `main`, an upload keeps its completion notice in the originating chat draft.
Thirteen real WebKit frontend cases pass, including project switches, mounted, closed and
promoted split panes, and held chats with a different resource runtime. Existing draft keys,
failure and Stop preserve their behavior. The real-host `upload-drafts` page scenario and
full page-enabled Core CI pass on `3f14696`, including 66 WebKit scenarios, release build and
benchmark comparison. Independent final QA is GO. Consumer native and release gates remain separate.
Board `adb47848`.

## Dictation follow-through

On `main`: each chat keeps its own draft, dictation uploads stop at exactly eight minutes,
Whisper and GGML native logs are silenced, Devices skips `/session` in native windows, and
notification page setup runs only in the app's own main window (ADR 0029). Open: a native check
of detached and remote windows at the screen. Board `26025811`.

## Bounded code runs

On `main` (ADR 0072): two active code runs, eight waiting, one per project, owned cleanup on
cancel, background children stopped, bounded capture. Left: native Windows and Linux runs and
the owner's call on the 2/8 limits (board `17de65d1`).

## Advanced artifact surfaces in development

`ark-advanced-artifacts` implements ADR 0071: map/model classification, route-bound immutable
snapshots, cancellation/progress, large-file metadata probes, bounded Focus and `ark.fileViewer`.
A0 and A1 to A3 independent code QA are GO. Focused tests pass: 49 file, 36 UI unit and nine HTTP.
Real WebKit covers Source recovery, a 30 MB model and the shipped Desktop frame through ARK's
snapshot transport, including selection and Source switching. Desktop has offline map and
worker-decoded GLB/STL surfaces, with real WebKit interaction and hostile-input proofs. Evidence:
`docs/ADVANCED_ARTIFACTS_A0.md`. The complete v0.24.0 CI passes, including all 55 WebKit
scenarios, the release build and benchmark gate. Desktop's full checks pass: 22 Rust and 64
JavaScript tests. Native local rendering, renderer-kill Retry and Linux ARM64-to-Mac map/model
dogfood pass. The consumer must pin the immutable v0.24.0 tag, not this development tree.
The owner deferred five-person observation and physical
trackpad testing on 2026-10-15; do not claim observed 10/10 usability. ARK Desktop implements A4
(CAD) as its own capability. A5 (IFC) is on `main`; the owner
deferred architect/BIM observation on 2026-10-15, so no professional validation is claimed.

What is happening in ARK right now. Read this at the start of a session. Update it at the end
of a session that changed what is in progress, blocked or next. Keep it under a page: history
goes in git and `CHANGELOG.md`, direction in `ROADMAP.md`, decisions in `docs/adr/`.

Last updated: 2026-10-21 (combined release source, checks and remaining gates)

## Release

| | |
|---|---|
| Latest tag | v0.25.0 release metadata prepared for 2026-10-21. Publication and fresh-clone verification are tracked above and remain open. |
| `main` | v0.25.0 release tree. |
| v0.14.0 | ADR 0048 (chat turns survive a crash and continue), ADR 0049 (typed Codex tool arguments, retry after shown text), ADR 0050 (an agent never loses its task), and `ark-os` from `ci/windows` (ADR 0022, merged; Windows not yet run on a machine). `ARK_UI_PAGE_TESTS=1 scripts/ci.sh` passed on the release commit |
| v0.15.0 | ADR 0051 (a turn ends on its answer, the project's `AGENTS.md` in each turn, code coloured in the editor), ADR 0052 (a chat runs on the computer the person picks, over SSH; new crate `ark-reach`; rustls), ADR 0053 (scheduled work; renumbered from the branch's 0051). `ARK_UI_PAGE_TESTS=1 scripts/ci.sh` passed on the release commit. No live Claude run before the tag (the owner's choice, 2026-10-08; ROADMAP › Deferred) |
| v0.16.0 | ADR 0052 resource-location amendment: every resource is bound to its runtime, project replica and opening chat; missing routes fail closed; remote Browser rendering, terminal replay and reconnect; exact transfer plans, epochs and durable receipts. `ARK_UI_PAGE_TESTS=1 scripts/ci.sh` passed on the release commit. A real Linux x86_64 runtime covered Files, `.xlsx`, `.docx`, Terminal, processes, artifacts and Browser with no matching local workspace |
| Next tag | not set. |
| Step 2 | Done in v0.9.0 (ADR 0012, ADR 0014). Deferred: start the CLI before the first call, deeper provider telemetry |
| Windows | `ci/windows` merged into v0.14.0 (`ark-os`, ADR 0022): every OS difference ARK had on 2026-10-05 goes through it; code written on `main` after that (processes, notifications, durable turns) mostly does not yet, and newer shell-script tests are not marked unix-only. Never run on Windows. Next: the machine run (`scripts/windows-check.ps1`), Office fixes in `ark-files`, one runtime per store. Analysis and plan: `ark-desktop/docs/WINDOWS.md` |
| Accounts | In v0.10.0: `ark-account` (ADR 0026, 0031), an optional ARK Account for ARK Cloud. Tested against a fake service and dogfooded in ARK Desktop against the real account service (`ark-cloud/account`, local) |

## In progress

| What | State |
|---|---|
| Project section hook (ADR 0077) | On `main`, card e11c3c73: generic app rows inside the current unfolded project, with owned cleanup. WebKit passes nine component cases and one actual frontend boot case against intercepted RPCs. The real Rust-host scenario and full page-enabled Core CI pass on `3f14696`; independent final QA is GO. Desktop-kept source/browser migration is prepared separately; its full checks, release and repin remain open. |
| An app's conversation is the chat (ADR 0076) | Branch `agent-chat-parity`: `ark.chatView` draws ARK Desktop's agent pane with the chat's own turns and composer. Page tests `an_apps_conversation_draws_exactly_as_the_chat` and `an_apps_conversation_behaves_as_the_chat` pass (streaming, approvals in place, tool rows through `task_call`, Stop). Desktop needs a tag with it before it can ship. |
| Completed branch audit | CI's unoptimized test profile, historical completed branches, voice dictation and the reviewed efficiency and catalog-startup patches are integrated on local `main`. Exact combined page-enabled CI and focused remote-provider, dictation and approval checks pass. No push, tag or release. |
| IFC model files (A5) | On `main`: `.ifc` is a read-only model (STEP header check, 2 MiB limit before any copy), Source opens read-only, `api.confirm(label)` for viewers, and `ArkUi::wasm_worker_asset` (HTTP serve path only). ARK Desktop's consumer is on `codex/ifc-integration` and needs an ARK tag with this before it can pin. |
| Efficiency and Codex startup | Host-model tools stay in ARK's approval path; scoped vendor overrides remove unused native tools without changing user settings. Default instructions avoid redundant bookkeeping and route permitted proposals to the existing approval gate. Catalog discovery skips optional usage telemetry while retaining its six-second deadline and strict advertised model/effort checks. Separate owner gates, 99 model tests, safety checks and synthetic writing-app checks pass. The 30/30 efficiency result is configuration-confounded, not a causal savings claim. Source chain, retained failures and remaining device/release gates: `bench/integration-2026-10-15/README.md`. |
| Models and commands follow a chat's computer | Released in v0.24.0: choosing a linked computer immediately uses its subscriptions, API keys and Ollama catalog; the selected model starts the chat there; setup stays chat-bound; local-only slash commands are omitted from completion and the command palette, and typed slash words go to the remote chat as text. Focused two-runtime Rust and WebKit tests, independent QA and the full `scripts/ci.sh` gate pass. |
| Voice dictation (ADR 0070) | Integrated on `main`: explicit computer-bound uploads, independent split controls, permission and inference leases, immediate capture cleanup and stale-result cancellation. Authenticated two-runtime and WebKit tests cover remote routing and capture races. Full page-enabled CI passes: lint, workspace tests, 56 WebKit scenarios, release build and simulated benchmark gate. Independent source and screenshot QA is GO. Optional `ark-transcribe` installs the pinned multilingual Whisper base model only after a microphone action. Native microphone and multi-minute quality validation remain release follow-ups on macOS, Windows and Linux. |
| Signed page updates (ADR 0067) | Released in v0.23.4. An opted-in native consumer verifies a separately signed manifest, exact native versions, a monotonic sequence and a bounded exact file set. Apply reloads only the page. A seven-second readiness failure restores the previous page while the runtime keeps running. ARK Desktop provides the first consumer and release path. |
| Plugin marketplace (ADR 0069) | Released in v0.24.0: Plugins occupies the fixed global rail, Connections is removed from Settings and every reviewed provider integration is offered without a separate Add toggle. The Desktop bridge opens server-controlled managed connections and exposes generic, approval-gated managed actions only when an ARK Account is signed in. Connections are account-wide across that person's projects. The broker holds the provider key; Desktop does not. Composio Free is the default launch adapter. Activepieces or remote MCP remains the account-optional self-hosted path. A signed catalog refresh and second-provider swap proof remain. |
| Ambient Desktop | Released in v0.23.0. A native consumer can keep one page and runtime alive, switch it into a compact menu-bar presentation, project agent state and restore the exact chat. Computer keeps only a volatile foreground app name for this view. Window name and identifier use a separate approval each time. Accessibility content and capture remain the existing higher-permission actions. Held SSH and Cloud chats never fall back to this Mac. |
| Workspace UI polish | Released through v0.23.4. Open chat tabs and Chat, Agents and Board share one top rail; chat and side-pane tabs share the compact 30 px Terminal-tab control grammar; split panes do not repeat the selected chat title; the fixed destination rail reaches the window edge without a partial divider; WebKit side panes keep their intended width. Pixel inspection, the full ARK gate and 49 WebKit scenarios pass. |
| Plan, Manual and Auto execution modes (ADR 0064) | Released in v0.22.0. The chat persists a revisioned mode and each turn freezes it. Plan omits and rejects mutations. Auto bypasses approval only for explicitly eligible project file and contained code actions. Browser, Computer, connections, automations and unknown tools remain Manual. The composer exposes the choice and `Shift+Tab` cycles it. |
| Spatial chat layouts (ADR 0063, 0068) | Released in v0.23.4. Tabs reorder by drag and form nested horizontal or vertical layouts by dropping on a pane edge. Up to four visible chats keep their transcript, turn, queue, approvals, draft, scroll, model and mode. Dividers resize with pointer or keyboard, narrow windows show the active leaf, and valid layouts restore. A native drag beyond the window opens the chat in a full ARK window with window-scoped foreground, tab layout and Browser panel. The source changes only after native creation succeeds. |
| Native generated images (ADR 0066) | Released in v0.22.0: supported OpenAI reasoning models get an approval-gated `image.generate` action. Valid PNG output becomes a durable project file, renders as the image file card in chat and opens in the shared image view. A paid live API generation was not run. |
| Subscription agents keep their credentials on their computer (ADR 0061) | In v0.20.0: Grok 1.0 is an offered ACP worker with an isolated provider-owned login, fixed ask rules, no inherited app secrets, place-bound setup and a checked model selector. Claude Code and Codex explicit worker compatibility remains, but neither subscription is newly offered: Anthropic requires explicit permission for automated consumer access, and OpenAI's supported third-party route is app-specific Sign in with ChatGPT. ARK's full release gate passed. Left: ARK Desktop's permanent pin, native check and release. |
| Repeated failed tool calls | In v0.20.1, an identical failed read or observation runs once and gets one retry, then returns immediately. A successful change reopens the read. Focused runtime tests pass. The 22-workload simulated suite has no non-timing metric change. Live qwen3:4b passes `failure-recovery` and `web` 3/3, writer dogfood passes and implementation QA is GO. |
| Signed background updates for native consumers (ADR 0054) | In v0.17.0: opt-in `desktop-updater`, official Tauri updater in local and direct-remote window paths, an own-page-only state bridge, background download and install on macOS and Linux, no forced restart. Windows is explicitly unavailable until it has a non-disruptive installer design. Left: ARK Desktop's staged 0.5.2 to 0.5.3 proof |
| Project Terminal without a chat (ADR 0060) | Protocol support was released in v0.19.0 and stabilized in v0.19.1. The v0.20.0 correction makes the existing web Terminal pane use project authority, capture the current chat's computer and leave the chat unchanged. It removes the duplicate project view. Protocol v1.1 retains generated Terminal types and transport for future artifact clients. ARK Desktop 0.6.5 remains public on v0.19.1; a corrected Desktop release is still needed. |
| A chat runs on the computer the person picks (ADR 0052, accepted 2026-10-07): the place chip, held chats and `chat_*` calls, the project copy and Bring back, `ark-reach` (setup and a tunnel over the person's `ssh`), Settings › Computers, programs installed and signed in there, ARK Desktop's contract and builds, the writer as second app | In v0.15.0 (steps 0 to 6 and 8). The resource-location amendment is in v0.16.0: every surface carries a validated runtime, project and chat; transfer retry has exact plans, epochs and durable receipts; the remote Browser is rendered by Chromium there. Automated two-runtime tests and a real Linux x86_64 runtime cover Files, editors, spreadsheet, document, Terminal, process, artifacts and Browser, with no local replica. Left: a real-VPS run with a subscription (the owner), step 7 (OpenCode, Cursor: needs a signed-in provider and Cursor) |

## Open follow-ups (not started)

| What | Owner |
|---|---|
| Event automations (ADR 0062, proposed): schedules unchanged under one automation view; transport-authenticated external events with actor claims; deterministic matching; one durable run per event; mandatory run-scoped authority that broader grants cannot widen; Mail and WhatsApp Business proofs; one locked SQLite writer on a NAS, no broker. Plan and gates: `docs/AUTOMATIONS.md` | unassigned |
| A turn ends on its answer and follows the project's instructions (ADR 0051, in v0.15.0): found from the owner's ARK Desktop run of the highlighting task on `claude:sonnet` (10 calls, 551k input tokens, the answer shown 5 times, `rules: 0`). A restated answer beside a second check of the turn's own changes ends the turn; the project folder's `AGENTS.md` goes into each turn (1,500 tokens, sections chosen by the task); the editor colours code from muted theme tokens (BRAND.md › Product UI). Checks: `scripts/ci.sh` with page tests; `restated_answer.rs`, `rules::tests`, `a_turn_follows_the_project_folders_agents_file`, the workbench page test; QA plan and implementation gates. Left: a live run of the same task on `claude:sonnet` (the owner's OK), two instruction lines on checking and one answer measured live (+5.8% tokens in the simulated suite, held back), orchestrated agents reading `AGENTS.md`, the Claude vendor thread at 155k (a card) | the owner (live run) |
| Scheduled work (ADR 0053, in v0.15.0): dates, cron and the person's time zone; asked each time with all a run is handed; pause, resume, change, run now, stop in words, `/schedules` and the window (a line per schedule above its chat, the schedule in the side pane); one run per time across a crash; a failure that may pass tried again after 5 and 10 minutes, continuing; missed runs once or let go; overlap; auto-pause after 3 failures; 50 runs kept. Checks: `tests/schedules.rs`, a WebKit page test, a real-process run (`restart.rs`: approved, SIGKILL, ran at its minute in the next process), a live run on qwen3.5:27b (ADR 0053 › Check; the chat overwrote the person's CSVs with sample data before scheduling, asked). Cost: +7.9% tokens on the simulated `board` workload (larger schema), baseline regenerated. Left: ARK Desktop's Agents canvas (paused schedules, the schedule's zone in labels), a paid-model run of the stock example, waking the computer or starting ARK at a run's time (Desktop's login item) | the owner (paid run); ARK Desktop |
| An agent never loses its task (ADR 0050), with ADR 0049 (typed Codex tool arguments) and ADR 0048 (durable chat turns): in v0.14.0. Found from the owner's ARK Desktop store: the task left the working set after two reads, a Codex vendor session rotated without it, "cointinue" became the goal, and `cargo` was run inside Run code. Now: a pinned task frame and a ledger of dropped steps; a goal stored with each turn and continued by any follow-up (also after the step limit); a budget sized from the model's window (16k on a subscription lane, up to 48k on an API); a reply the model could not get right is sent back to it as feedback; `code.run` points a project's commands to `process.start`. Checks: `scripts/ci.sh`; `long_task.rs`, `lost_task.rs` (the owner's sequence with a SIGKILL), `login_path.rs`; live on `codex:default` (twice) and `claude:sonnet` (killed, "cointinue", highlighting implemented; ARK on Sonnet used 338k input tokens against Claude Code's 781k for the same task); live qwen3:4b against `main`: 12/14 both, +7.6% tokens (mostly ADR 0049's schemas), `bench/RESULTS.md`; 35 WebKit page tests; QA plan, design and implementation gates passed. Left: ARK Desktop's pin to v0.14.0 (its `main` has processes on since 6e52a25) and a native check | ARK Desktop |
| Durable chat turns (ADR 0048, on `main`, untagged): a chat turn keeps its progress through a crash, shows where it stopped with one Continue, and continues without repeating a change or a hand-off; a reused pid is not taken for a dead writer; 4 retries and 60 s for an unreachable model; durable blobs. `scripts/ci.sh` passes; real `kill -9` runs pass (`crash_recovery.rs`, `crash.rs`, `restart.rs`) and a page dogfood (writer, a fake model). Left: a tag (the owner's OK), ARK Desktop's pin and a native check after it, Windows process start times (`ark-os`), deduplicating Observe calls that write records (`board.add`) if a run shows it | the owner (tag); ARK Desktop (pin) |
| Project processes (ADR 0041, in v0.13.0): ARK Desktop turns them on (`.processes(true)`) with its v0.13.0 pin. The agent's browser reads the pages a project's processes serve themselves (ADR 0046, on `main`). Later: a runtime elsewhere forwards a process's port so the Browser view reaches it (ARK Cloud), Windows (a job object), a dogfood on a hosted model | ARK Desktop; ARK Cloud |
| The Browser panel as a view of its own in the window (ADR 0044, in v0.13.0): checked natively only on duckduckgo.com; the rest of ADR 0044's Check needs a person at the screen (a Next.js dev server, a sign-in, a Desktop dialog, resize, project switch, reload, This Mac \| Cloud, Dock reopen, a notification; and since "can't be reached": `localhost:3000` down then up, a slow dev server, a plain http site with ARK Desktop's Info.plist, a page without a background in light and dark). ⌘K and ⌘N are menu keys on main since cbc4073 (Esc stays a page key, ADR 0044); a native check from the Browser view waits for the next tag | the owner (the native check); ARK |
| Rename "Allow for this session" in approvals | the owner decides the words |
| Typed Codex replies (ADR 0049): a live `analysis` and `workbooks` bench on `codex:default` against `bench/RESULTS.md` (2/2 each, about 230k tokens a run, before); the input-token cost of the schema in a long session. Later, if a reason comes: strict function calling for OpenAI-compatible APIs from the same converter | the owner's OK for the live bench |
| The Computer (ADR 0042, 0045, 0056, in v0.18.0): off until the person turns it on; app-neutral native control; sequenced activity and targets outside ARK; Stop shares the dispatch lock; message draft, send and proof are separate; durable draft and unconfirmed-submission records survive restart and block unsafe mutations; trusted unresolved effects replace false success claims before storage or display. Checks cover wrong foreground apps, restart, status-only message changes, keyboard cancellation, stale overlays and trailing streamed whitespace. Left: grants per app and a controlled native self-chat run in ARK Desktop | the owner: grants per app; ARK Desktop: native self-chat run |
| Attention, workers, where work runs (ADR 0043, accepted 2026-10-07, after the One and herdr review): step 0 an ACP spike (an ARK task on Claude Code and Codex: permission, kept question, answer, continue, result and file back), 1 one attention list in a runtime, 2 one ACP worker (ADR 0039 H1 to H3), 3 notifications to a phone and My machine as a location, 4 attention across runtimes. Order and checks: ROADMAP › Work runtime | Steps 0 to 4 done, in v0.13.0 (ADR 0043 › Step 0 to Step 4 results, ADR 0047). Left for the owner: real phones (iPhone Home Screen, Android), ARK Desktop's My machine at the screen. Found: after a restart an ARK agent repeats its step, and a local model may write a different change, so the person is asked again (Claude Code loads its session and is not asked again) |
| Board: whether the chat uses the board unasked for project-sized work, and hosted models (the `board` workload passes 3/3 live on qwen3:4b when asked); board tools for code-defined team leads | before turning anything on or off by default (ADR 0023) |
| ARK Desktop moves its keys to ARK's store (`ark_connect::ApiKeys`, the `api_key_*` methods) and drops `setup::keychain` and `load_keys` | ARK Desktop |
| A paid-model run of scheduling: qwen3:4b claims schedules it never made | needs the owner's OK |
| ARK Desktop uses `ark_files::Files` instead of its own `files.*` adapter (same action names), and turns Files on | ARK Desktop, against `../ark` until v0.10.0 |
| Files: cell styles in the grid, images in a document, undo across content saves, `.xls`, `.doc`, chart creation and more formula functions | when a real task needs one (ADR 0019 › Consequences) |
| Files on qwen3:4b: `files-edit` (`bench/RESULTS.md`) is 0/5. It finds the file, and since the announced-work check it no longer stops after reading; it then puts the totals in a new column or sheet. qwen3.5:27b 3/3 | the layout is the model's; revisit with a hosted model or a larger local one |
| ARK Desktop ships `uv` in the app (Run code is on in its `main`): without `uv` a clean Mac shows Run code as unavailable. The Data Analysis page on aiark.dev waits for that release (owner, 2026-10-05) | ARK Desktop |
| Claimed files: the harness checks an answer's saved files against the turn's (`claimed-files`, `bench/RESULTS.md`). Claimed schedules: not reproduced on qwen3:4b or qwen3.5:27b in three shapes (`claimed-schedule`), so no check for claimed actions. Left: claims in other languages than English | a transcript of a claimed schedule, then a workload that reproduces it |
| Run code on qwen3:4b: 1/5 on `analysis`; it diagnoses its own syntax error but does not fix it. Identical changes now run at most twice (35% fewer tokens). qwen3.5:27b 2/2 | a fix needs a stronger model or a measured hint; nothing planned |
| Run code later: network as its own permission, packages per project, memory and CPU limits per run | when a workflow needs one (ADR 0020, 0032 › Consequences) |
| Run code on a real Windows machine (`cargo test -p ark-exec --test run_windows`, with `ARK_TEST_PYTHON=1`) and a real Linux machine (`ARK_TEST_PYTHON=1 cargo test -p ark-exec --test run`) | the owner's Windows machine; a Linux machine or a working Docker (ADR 0032) |
| Data work on smaller models: Haiku and qwen3.5:27b keep duplicate orders on `workbooks` (0/2 each); Sonnet and Codex 2/2. A hint naming the skill in `code.run` did not help Haiku | a measured change in the skill or the task's shape; nothing planned |
| Data UX: Windows saying Excel holds a workbook open (sharing violation on a save) | the owner's Windows machine run |
| Versions before (ADR 0034): not measured on ext4 or NTFS, where the copy before a run is a real copy; the store keeps every version (no clean-up of old artifacts yet); the terminal has no Restore | a cloud runtime's numbers; when the store grows too large |

## Waiting on this repo

| Who | Waits for |
|---|---|
| ark-desktop | nothing: it moves to v0.12.0 for `token link` and `ARK_ENTRY_URL` (ARK in the browser) |
| Mail and Calendar for anyone | ARK's Google OAuth client and Google's review: owner steps in [`docs/GOOGLE.md`](docs/GOOGLE.md). Until then they show as unavailable, or work with `ARK_GOOGLE_CLIENT_ID` for test users |

## Next

Prove ARK as a product, in this order ([`ROADMAP.md`](ROADMAP.md) › Next): v0.8.0 (done),
Claude Code and Codex first-class (done, v0.9.0), ARK Desktop public, hosted-model
evidence (time-boxed), persistent headless ARK (shipped), ARK Cloud on the same
runtime. ARK Cloud's plan is `../ark-cloud/docs/CLOUD.md` (2026-10-05); ARK's part of it is
A1 to A4 on the board. A1 (named OpenAI-compatible endpoints) and A2 (402 is out of credit) are
on `main` (ADR 0027), and A3 (`FileSecrets`: keys and tokens in an encrypted file for servers,
ADR 0028), and A6 (the window opens a runtime elsewhere and comes back, `remote::visit`, ADR
0029, for ARK Desktop's This Mac | Cloud), and a folder sent to a runtime elsewhere becomes a
project there (`remote::send_folder`, ADR 0030, for Copy to Cloud); and A4 (Run code on Linux,
bubblewrap, ADR 0032), for Cloud Phase 2. A4 ran on Linux on 2026-10-07 (Docker, Debian bookworm, bubblewrap, uv 0.12.23; `/proc`
unmasked as in a VM): all 10 `ark-exec` tests pass after the fix in v0.12.1.

W1 to W3, W5 and W6 of [`ROADMAP.md`](ROADMAP.md) › Work runtime are on `main` (ADR 0037,
0038). The runtime has no known blocker left for Cloud running work nobody watches; next is an
end-to-end check on a Cloud machine. ARK Desktop's W4 (its page reads them) ships on its pin of v0.11.0
(ark-desktop 05b7aa1).

W5 and W6 checks: `examples/writing-agent/tests/restart.rs` runs a real `writer --serve` against
a fake OpenAI-compatible model, stops it with SIGTERM (clean exit, about a second) or SIGKILL
while an agent works for a waiting chat, and starts it again: the task continues, the chapter is
not saved twice, the chat gets the result once. A change still running when the stop starts is
let finish: covered in process (`a_stopping_process_lets_a_change_under_way_finish…`), since the
writer has no slow change to stop a real process in. Dogfooded live (2026-10-05, qwen3.5:27b,
`writer --serve --tokens`): the chat handed a save to an agent and waited; after the save,
SIGTERM landed in the agent's last model call and the process exited cleanly in 0.1 s. The
restarted runtime continued the task. The model tried to save twice more and both calls were
answered from the first save (the file was not written again). The chat's turn shows cancelled
and the result arrived as one post. An earlier run where the chat's wait got the result live
posted nothing (`reported`, no second copy).

W1 to W3 dogfooded live (2026-10-05, qwen3.5:27b, `writer --serve --tokens`): an agent asked to
save a chapter, the process was killed with `kill -9`, the restarted runtime listed
the kept question in `hello` and `/agents` ("waiting (your answer)"), `writer --connect -p
"/task answer <task> once"` answered it, and the chapter was saved once. Not run on a hosted
model or a Cloud machine yet. The same through ARK Desktop's page (server build against `main`):
the canvas showed "needs you" and the waiting line before and after a `kill -9`, and "Allow once"
in the tray after the restart saved the file once.

Checks on the W1 to W3 commits: `scripts/ci.sh` passes (fmt, clippy, every test, release build,
bench gate); the 26 WebKit page tests passed on the first full run. `files-edit` reads 8 tokens
over the baseline both before and after this work: from v0.10.0's versions before (checked on
8bd8b96), within the gate. In v0.20.2, the page gate maps
`RUST_TEST_THREADS` to libtest's real setting. Its fake OAuth browser decodes complete form values
and retries transient loopback closes. The former connect-mid-task flake passes 25 of 25 focused
runs and the complete 43-scenario page gate.

## Working notes

- Several Claude sessions may edit this tree at once. Agree a file split before editing (see
  `../WORKSPACE.md`, Sessions).
- `.playwright-mcp/` is scratch output from browser dogfooding. It is untracked; delete it when
  done.

## October 20 local UX integration follow-up

The reviewed clarity source is now applied in this local tree, including the new work-state WebKit test and CI entry. Shared-page tests repeated at29/29. iOS incremental source is integrated with prior edits preserved. Desktop currently previews this page through explicit source overrides on its installed0.23.6 runtime; this is not a freshly linked Desktop release. Full Core/Desktop/native gates remain pending disk admission. No commit, dependency pin, tag, push or release. Evidence: `../board/evidence/ux-clarity-2026-10-20/README.md`.


## Desktop icon harmony trial, 20 October 2026

Static Phosphor regular icons now align with iPhone. Chat, Agents, Board, new chat and attachments use explicit action symbols. Primary header/sidebar/composer controls use actual40px targets. Shared WebKit30/30 passed. A separate locally signed ARK UX Trial runs the successful currentCore0.25 cached binary with explicit current source overrides; installed ARK stays intact. Final embedded rebuild and full compiler gates still need disk admission. Native screenshot and launch command: ../board/evidence/ux-clarity-2026-10-20/. Push delivery remains incomplete.


## Slim shell and agent conversations, 20 October2026

Owner feedback applied: fixed rail50px, header44px, glyphs17px with40px primary click targets. Fixed chrome and project drawer have subtly different semantic tones. Composer model follows upload and dictation in visual/DOM order; mode is right aligned, the sole local destination stays hidden, remote choice stays visible. Project new chat uses plain plus. Desktop current-project agent faces open existing per-agent conversations, with runtime/project stale guards, focus preservation and disposal. Native row opened the correct orchestrator conversation. Desktop page tests71/71 pass; shared page verification recorded in slim-core-final.log. Group avatars remain open because chat participant metadata is absent. Full compiler/final embedded release and APNs delivery stay open. Evidence: ../board/evidence/ux-clarity-2026-10-20/.


## Participant tracking and compact controls, 20 October 2026

The participant source now derives assigned agent names from durable task report_to and parent links. This corrects the earlier assumption that no chat association existed. A read-only check found three existing chats with recorded groups. Protocol 1.5 adds optional participants; unknown remains distinct from known empty. Desktop and iPhone render a neutral identity, one face, or a capped group stack with an overflow count. Source review approved after preserving base chats on malformed task records and populating addressed reads for remote forwarding.

Desktop pointer chrome is now 44px rail, 36px header, 15px navigation glyphs and 32px controls. Project drawer width is 236px; coarse input retains larger targets. The header tone continues above the project drawer. Native glyphs are 20/18/16pt with 44pt toolbar targets.

Web checks: Desktop 71/71; shared page 35/35 plus focused 13/13 including touch targets. Rust formatting passes. New Rust projection/integration tests, authoritative Rust schema generation, Swift protocol tests and native simulator/device checks have NOT run: guarded commands refused current disk space. Schema-to-Swift output reproduced the existing generator output before the additive field, but does not replace the Rust generation gate. The running Desktop trial loads current UI sources with its previous native binary, so it does not yet expose real participant metadata. The installed phone remains the prior verified build.

Push remains incomplete. Stored Apple keys were rejected; a valid APNs key path and ID were requested. Full trusted relay, runtime/native registration and live delivery still remain. A writable external build volume was requested; no build output was deleted. The vstack task remains in build, with guarded compiler gates configured.


## Stable avatar identity integration, 20 October 2026

Seven bundled faces now use the same frozen UTF-8 identity-v1 rule across Desktop contacts,
canvas, individual/group chat marks and iPhone chat/group/attention marks. Working status no
longer swaps the face. Exact-byte participant deduplication preserves distinct Unicode names.
New192px exports remove the candidate-scale edge artifacts at compact sizes. This is a one-time
transition from the old four-face hash; future catalog changes require an identity migration.

Desktop full page suite73/73 and shared suite37/37 pass; final late-resolver14/14 pass.
Production Swift identity code compiles under the guarded small phase and executes9cross-platform
vectors plus Unicode group deduplication. The new source faces are visible in the existing Desktop
trial via source overrides. This does not update its older native participant projection.
Full Rust/native simulator/device build and install remain blocked by phase disk admission.
Push remains unavailable: trusted relay/native registration and valid APNs credentials are still
required. No release, deployment or claim of complete mobile integration.

The later guarded checks also pass: durable projection4Rusttests, protocol4tests, generator
nullable-collection regression1test, authoritative protocol regeneration and native byte compare,
and Swiftpackage7tests. This supersedes earlier protocol/projection unrun statements. The
interaction/host integration and complete application builds remain pending storage.

## Route models and official provider authorization, 23 October 2026

Protocol v1.7 now has a route-owned ready model catalog, an optional model on idempotent chat
creation, optional current model state on conversation reads and the ordered `chat.model`
mutation. The session's first record stores an explicit model atomically. Exact retries compare
the durable project and model. Existing records and clients remain valid without either field.

The account-bound connection surface supports Claude Code and Codex through their pinned official
commands. Codex uses its device page and opaque user code. One authorization child runs across all
providers. Attempt ownership, bounded output, cleared environment, strict status, cancellation,
descendant cleanup and redacted Debug output fail closed.

Focused evidence passes: six protocol tests and authoritative generation, 17 authorization tests
and the atomic session model test. Generated Swift SHA-256 is
`54a74042c07db0268f4c335c0f10bb9c4a3030aec5dd153ca63d39c53452df98`.

On 24 October the current-source focused checks passed: the route model HTTP test, native
authorization ownership and recovery HTTP test, six protocol tests, the atomic session model
test and authoritative generation. The route test now counts retries against the post-restart
Host baseline, since opening that Host creates a separate default session. Its original
lost-response one-chat assertion remains. Root independently reviewed that test correction.
The bounded x86_64 Linux release build passed on the development Mac in 3m56s. Its 64,503,768-byte
binary requires GLIBC 2.29 and has SHA-256
`9d4b9e669a881ee42151593649bee2862d17aed43121d3e73b573f3233e1233c`.
The owner requested focused
critical checks instead of another full repository gate. No deploy, release or live provider
login was performed by this Core session. Evidence: workspace
`board/evidence/mobile-core-models-20261024/` and `/tmp/ark-core-model-codex-final-build-20261024.log`.
