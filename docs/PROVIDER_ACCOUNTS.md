# Multiple accounts per model provider

Status: Core and Desktop implementation candidate. Qualification is recorded in the workspace board. No release is included.
Requested: 2026-10-30.

A person can connect two or more accounts to the same model provider, choose which account
a chat uses, and see each account's reported usage limits in Settings > Models.

## Current implementation

| Surface | Candidate behavior |
|---|---|
| Core records | `ark-connect::ModelAccounts` stores added accounts and consumed setup IDs in `model-accounts/accounts.json`. Provider-owned profiles are private sibling folders. Saved API keys use the existing secret store. Removed tombstones retain no name or email. |
| Model selection | Existing `provider:model` choices keep their meaning. Added accounts use internal `provider~UUID:model` references through the same chat, agent and schedule records. Ordinary UI uses names. |
| Lifecycle | Active, Reconnecting and Removing are distinct states. Subscriptions activate only after account-bound successful CLI status. Known identity must match on reconnect. New login waits for old requests and idle processes to stop. |
| Providers | Claude and Codex subscriptions, Anthropic/OpenAI API keys, OpenCode Go keys and registered OpenAI-compatible endpoints have separate account bindings. The external default CLI sign-in remains available and untouched. |
| Desktop | One provider group, Add account, rename/reconnect/remove, per-account usage, and explicit account selection in the existing picker. Failed saving preserves the actual active selection and offers Retry. |
| Usage | Claude reports limits during work. Codex also has a bounded metadata refresh. Distinct buckets remain distinct. Unknown utilization and expired reset times never create full allowance. APIs without account-limit telemetry omit it. |
| Recovery | Startup opens history and Settings even when a saved account was removed or reconnect was interrupted. Calls fail closed. Cached agents re-resolve changed account revisions, including API keys. |
| Existing sign-ins | No credential copy or migration of the external CLI login. Legacy API key removal keeps its existing behavior. The default external CLI account has no new Remove action. |
| Native clients | Existing account-qualified catalog references use the shared protocol. Native account-management presentation and physical-phone qualification are separate consumer work. |

[ADR 0086](adr/0086-model-accounts-have-stable-execution-identities.md) records the selection,
credential and process boundary. One account store per runtime process remains required by
the existing process-global KeySource seam. Separate runtimes use separate processes.
Provider status exposes display identities where available. It does not consistently expose
verified subject, organization and credential scope together, so automatic duplicate-account
merging is not claimed. The tables below retain the broader design and acceptance contract. Candidate differences:

- Cancel stops sign-in and retains a pending account for retry or explicit removal.
- An unused setup can be removed without the vendor app. Once sign-in starts, the vendor
  app is required to clean its credential store; failure keeps a retryable Removing row.
- Account actions are visible row buttons.
- Missing usage reports omit the gauge. No invented allowance is shown.
- Duplicate identity merging and hiding the external default CLI sign-in are deferred.

## Product behavior

| Place | Behavior |
|---|---|
| Settings > Models | One group per provider. Each connected account has its own identity, state and limits. Add account stays in that group. |
| One connected account | Keep the current short path to choosing a model. Show its identity; no account-selection step. |
| Several connected accounts | The model picker shows the selected account above the model list. Its account control opens the other accounts with their remaining limits. Changing it is an explicit choice. |
| Composer | Keep the model name. When several accounts exist, include the selected account's short label so the choice is visible before sending. |
| Account row | Identity first. Optional short name, such as Personal or Work, can replace a long identity in the composer. Full identity remains accessible. Rename, reconnect and remove belong in the row menu. |
| Usage | Each account shows its own provider-reported windows, remaining share and reset time. Never sum percentages across accounts or models. |
| Exhausted account | Show the limit and reset time. Other accounts remain selectable. No automatic switch. |
| ARK Account | Keep the person's ARK sign-in separate from model-provider accounts. No new top-level Accounts destination. |

Visible actions are Add account, Sign in, Cancel, Retry, Reconnect, Rename and Remove as
needed. Do not add onboarding paragraphs, explanatory captions or success toasts when the
new account row itself confirms success. Existing brand tokens and Settings controls apply.

## Data before behavior

These are proposed shapes, not existing public types or example runtime output.

| Record | Fields and invariant |
|---|---|
| ProviderAccount | Opaque stable account ID, provider ID, display identity, optional short name, connection kind, credential reference, lifecycle revision. ID never derives from an email or label. Credentials stay outside this record. |
| ConnectionKind | Existing external CLI sign-in, isolated subscription sign-in, or saved API credential. Local models have no account. A future provider supplies an adapter through the same model-account boundary. |
| AccountState | Connecting(attempt ID), Connected, ReconnectRequired(reason), Unavailable(reason), Removing. Setup failure remains attached to its attempt and can be retried or cancelled. |
| ModelSelection | Provider ID, account ID when required, model ID, effort. A stored account reference never falls back to another account if unresolved. |
| UsageReport | Account ID, provider ID, source, reported time, plan when known, limit buckets and windows when reported. An absent measurement has no percentage. |
| UsageWindow | Provider bucket ID/name, duration when supplied, used share, reset time when supplied. Distinct provider buckets remain distinct. |
| SetupAttempt | Opaque attempt ID, provider ID, isolated credential scope, state, verified account identity when supplied. A retry reuses this attempt until committed or cancelled. |

Account ownership belongs to the runtime on the selected computer. Front-ends receive only
the public account view. A client cannot supply a config directory, keychain name or command.
Secrets, sign-in URLs containing credentials and raw CLI configuration never enter ordinary
records, telemetry, model prompts or screenshots.

One immutable execution identity includes provider, account, model and effort. Session reuse,
restart recovery, process reuse, model catalogs, capacity, cooldowns and usage updates all
use the appropriate account identity. Labels are presentation only. A late report carries
its original account ID and lifecycle revision; it cannot update a reconnected or removed
account.

Reconnecting advances the lifecycle revision and invalidates account-bound processes,
resumable vendor sessions and authentication/catalog caches, even when the verified identity
and account ID stay the same. No new request reuses execution state from the old revision.

## Connection and recovery

| Event | Result |
|---|---|
| First launch after upgrade | Preserve the existing external CLI sign-in and API keys as the default account references. Existing chats keep their account. Migration is idempotent. |
| Add account | Start an isolated sign-in or key-entry attempt inside the provider group. The existing account keeps working. No name is required before sign-in. |
| Successful sign-in | Verify the account-bound status and identity when available before committing the row. Process exit alone does not establish connection. |
| Same verified identity | Deduplicate by provider, verified subject, organization and credential scope. An exact known match focuses the existing account. Different scopes remain distinct. An unknown component never establishes a match by email or label alone. |
| Browser dismissed, refused, timed out or offline | Preserve existing accounts. Keep Retry and Cancel next to the failed attempt. Restart does not turn an unfinished attempt into Connected. |
| Cancel | Stop the attempt's process tree and discard only its temporary credential scope. Retrying an old callback cannot commit it. |
| Reconnect | Bind setup to the original account ID. An identity change requires an explicit new-account choice; it cannot silently replace another identity. |
| Remove | Disable new calls on that account, cancel or drain its processes, then remove its ARK-owned credentials and caches. A failure remains visible and retryable. Other accounts stay connected. |
| Removed account selected by a chat, agent or schedule | Retain its unresolved selection and work history. Require a person to choose an account before another call. Do not rewrite it to the default. |
| Existing external CLI account removed from ARK | Stop offering it in ARK. Do not sign out the external CLI or delete its files. |
| Active request while selection changes | Let that request retain its original account. The next request uses the new selection and opens a fresh vendor session. |

Removing provider access uses the existing destructive-action treatment, names the account
and explains effects only when work is affected. Drafts and chat history are preserved.
Setup on another computer uses that computer's authority and setup surface. Credentials are
not copied between computers as part of this feature.

## Honest usage

| State | Presentation |
|---|---|
| Loading first measurement | A small static placeholder in the account's usage position. Account identity and actions remain available. |
| Current measurement | Show only reported buckets/windows. Use the provider's reset time and local display formatting. |
| No report yet | Usage unavailable. No empty progress bar or 0% value. |
| Provider has no limit telemetry | Omit usage controls. Account connection still works. Do not infer subscription allowance from ARK token counts or a successful key save. |
| Refresh fails | Preserve the previous measurement, label its age, and offer retry locally. Do not report zero allowance or disconnected authentication from a telemetry failure. |
| Reset time passed | Mark the report as needing refresh. Never manufacture a fresh 100% remaining value. |
| Limit reached without windows | Show Limit reached. Do not invent a reset time or choose a random bucket. |
| Multiple buckets | Show independently. A compact picker indicator names its bucket; it does not imply one comparable total across different limits. |

Usage refresh is a bounded metadata operation. Opening Settings or the picker must not send
a paid prompt, start inference or force sign-in. Coalesce concurrent requests per account,
time out and cancel work when its scope closes, and back off after failure. A provider that
only reports limits during actual work retains a last-reported view with its age.

## UI states and interaction

| State | Required behavior |
|---|---|
| Empty | Existing provider Connect action. After the first connection, the provider group exposes Add account. |
| Adding | One setup row in that group, immediate busy feedback and Cancel. Repeated activation focuses the current attempt. |
| Error or sign-in required | State on the affected account row with Retry or Reconnect. No full-page error replacing other accounts. |
| Long identity | Truncate visually within the row, preserve its accessible name and full identity on focus. Never push actions or limits outside the viewport. |
| Keyboard | Native buttons, visible focus, named row menus, Escape returns focus to the trigger. Account/model selection by keyboard is immediate. Rerender preserves focus. |
| Narrow or dark | Reuse adaptive rows and theme tokens. Limits may wrap below identity. Account actions remain reachable. No account dashboard or nested cards. |
| Touch | Reachable targets, no hover-only actions, safe-area and zoom checks on physical hardware before claiming phone support. |
| Motion | Busy and selection feedback are immediate. No decorative staggering or animated limit meters. Any occasional popover transition stays short and respects reduced motion. |

## Existing seams and implementation ownership

| Layer | Current evidence | Required change |
|---|---|---|
| `ark-connect` | `src/api_keys.rs` indexes saved keys by provider | Own account records, credential references and setup lifecycle through the existing connection boundary. Preserve the legacy key source. |
| `ark-model` | `src/cli.rs` uses ambient CLI sign-in in status, catalog, inference, search and cleanup | One account-bound process configuration applied to every execution path, including fallback. No process-global environment mutation. |
| `ark-model` | `src/usage.rs` records fixed claude/codex keys | Carry account identity from the originating process. Scope catalog and effort caches by account too. |
| `ark-model` | `src/session.rs` reuses and restores a lane by model identity | Include account in reuse, ledger recovery and release. Preserve old serialized records with explicit legacy resolution. |
| `ark-interact`, `ark-sdk`, `ark-ui` | Model selection, managed sessions and window methods are shared | Expose the account view and explicit selection once. Update protocol schema and compatibility fixtures for any changed public method. Held-chat calls target the owning computer. |
| `ark-desktop` | `src/setup.rs`, `src/install.rs`, `ui/models.js` | Account-bound setup jobs and grouped rows in the existing Models setting and picker. No Desktop-owned account database. |
| `ark-ios` | Native consumer of the runtime protocol | Consume the shared account view and choice after Core support. Keep native presentation; verify on device. |

## Alternatives

| Design | Tradeoff |
|---|---|
| Duplicate a provider entry for every account | Small initial UI change, but repeats models and setup, hides grouping, and encourages provider-name aliases to leak into ordinary UI. Rejected. |
| One provider with account identities and explicit selection | Adds one account boundary to shared state. Keeps provider discovery/model choices grouped and gives each limit a clear owner. Chosen proposal. |

Do not create a second account system in Desktop, overload the ARK Account, or introduce
automatic quota routing. Final API/serialization changes require an ADR before implementation.

## Provider isolation evidence and limits

The installed programs report Claude Code 2.1.314 and Codex CLI 0.153.4 on 2026-10-30.
Only version commands and source inspection were run. No second sign-in, credential read,
logout, API validation or paid model call was performed.

| Provider | Primary documentation | Qualification still required |
|---|---|---|
| Claude subscriptions | [Authentication](https://code.claude.com/docs/en/authentication) documents separate `CLAUDE_CONFIG_DIR` directories for multiple accounts. Console sign-ins without API keys are an exception. | Confirm installed-version keychain isolation, token refresh, inherited auth-variable removal, identity and all process paths on each supported OS. Do not generalize directory isolation to the Console exception. |
| Codex subscriptions | [Authentication](https://developers.openai.com/codex/auth/) documents file/keyring/auto/ephemeral credential stores and `CODEX_HOME` for file credentials. | Verify separate account storage, keyring behavior, ambient auth overrides, status, catalog and fallback on installed versions. Follow enforced administrator policies. |
| Saved API keys and named endpoints | Existing `KeySource`, `ApiKeys` and endpoint seams | Scope secret references and validation to accounts; preserve endpoint host/TLS rules. Advertise usage only where real account telemetry exists. |

Do not enable an adapter's Add account action until its credential isolation is qualified.
Use deterministic stand-ins first. Real sign-in qualification needs the person to complete
each provider's authentication. Never copy or inspect their tokens to simulate a second user.

## Acceptance evidence required before shipping

| Check | Proof |
|---|---|
| Two accounts on one provider | Add B while A is active. Verify distinct identity, sessions and credentials. Restart and use each without signing out the other. |
| Scope every execution path | Process fixtures cover sign-in, status, catalog, ordinary/streamed calls, web search, app-server, exec fallback and cleanup. A's environment and files never appear in B. |
| Same-model account switch | A to B on the same model opens a new vendor session. Repeat after restart. Agents that follow the chat follow its account; explicitly assigned agents retain theirs. |
| Concurrent accounts | Interleave usage reports, failures, cooldowns and model/effort listings. No cross-account cache entry or limiter changes. |
| Setup lifecycle | Double activation, callback retry, cancellation, timeout, rejected login, duplicate identity and restart converge without duplicate accounts or leaked processes. |
| Removal | A remains usable after removing B. B stops taking new requests. Selected chats and schedules ask for account choice. The external CLI's login stays intact. |
| Migration | Existing keys, model strings, chats, agents and schedules resolve to their prior default account. Repeated migration changes nothing. |
| Honest limits | Missing, stale, failed, exhausted and multiple-bucket responses preserve meaning. Passed reset times never create a new measurement. No inference to refresh. |
| Real UI | Actual runtime-backed Settings/picker proof for one/two/three accounts, long names, busy/error recovery, dark/narrow, keyboard and focus restoration. Screenshots accompany review. |
| Consumer parity | Shared CLI/window selection has the same meaning; protocol and native clients handle older servers and unresolved accounts. Physical-phone proof remains separate. |
| Required gates | Independent plan/design/implementation QA, Core `scripts/ci.sh`, Desktop `scripts/check.sh`, native checks for changed clients, and account-isolation proof. Gates run one at a time. |

Norman diagnostic: 8/10 for the observed implementation. Automated fixtures cover visible
outcomes, recovery, account/control mapping and constraints. Discoverability with a person
using this flow for the first time remains unmeasured. This is not a user-study result or a
physical-phone verdict.
