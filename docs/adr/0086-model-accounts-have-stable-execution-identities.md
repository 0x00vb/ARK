# 0086. Model accounts have stable execution identities

Status: Accepted for implementation
Date: 2026-10-30

## Context

The owner requested several subscriptions on the same provider, their individual usage
limits, minimal copy and consistent account setup. Model specs and usage records previously
named only a provider. A model-only session check could also reuse a session after an account
change. The detailed state and acceptance table is in `docs/PROVIDER_ACCOUNTS.md`.

## Decision

`ark-connect::ModelAccounts` stores account metadata and secret references in the runtime's
store. The existing KeySource boundary supplies model adapters with account bindings.
Passwords and tokens never enter account metadata. Vendor CLIs own their credentials in a
private directory per added account. Every child receives the same scoped environment;
there is no process-wide environment mutation or copied external login.

Old `provider:model` references keep their meaning. An added account uses the internal
reference `provider~UUID:model`. UI labels show provider and account names. This keeps
existing chat, agent and schedule model records as the one selection source. Account syntax
remains recognizable after removal, so it cannot become a model on another provider.

The account ID scopes credentials, catalogs, capacity and usage. A revision separates old
execution state after reconnect. Sessions persist and compare account and revision as well
as model. Added subscriptions start Reconnecting. Only account-bound successful CLI status
can activate setup. A known identity requires a fresh matching identity on reconnect.

A request holds an account lease. Lifecycle changes first invalidate new work, then drain
active requests and stop idle vendor processes. Removal logs out only the added profile,
deletes its credentials and profile, and retains a consumed-attempt tombstone. Replaying an
old request cannot recreate a removed identity. API key replacement validates before changing
the credential, then invalidates stale execution handles.

Settings retains one provider group with account rows. The existing model picker has an
account control only when several accounts exist. There is no automatic quota routing.
Missing, stale or expired telemetry never becomes a fabricated full allowance.

## Alternatives

| Approach | Reason |
|---|---|
| Duplicate a provider and key slot for each account | Repeats models, loses account ownership and encourages setup paths that overwrite credentials. Rejected. |
| Replace every stored model string with a new selection record | Would migrate all clients and records before preserving existing behavior. No additional authority benefit in this change. |
| Stable account references through the current model seam | Existing records, routing and explicit model choices carry the account together. Chosen. |

## Limits

The KeySource installation remains process-global, as it was for saved model keys. A process
must serve one account store. Independent stores must run in independent runtime processes;
this change does not claim isolation between several Ark instances sharing one process.
Native clients can consume account-qualified catalog references. Native account management
and physical-phone qualification are separate consumer work.

Real second-account sign-in needs the person to complete the provider's own authentication.
Empty-profile checks and deterministic vendor fixtures do not prove paid entitlement.

## Checks

`crates/ark-connect/tests/model_accounts.rs` exercises separate scopes, telemetry, stored
records, retries, lifecycle invalidation, process shutdown, identity mismatch and removal.
`ark-model` tests cover same-model account changes across restart and independent limit
buckets. Desktop tests exercise the actual model views. Runtime-backed proof and required
repository gates are recorded in the workspace board evidence before completion.

Unused subscription profiles contain an ARK setup marker. The login command removes it
before spawning the vendor. Only a still-unused profile without verified identity can skip
logout when the vendor executable is absent. Started setups keep retryable cleanup, including
keychain-backed credentials. Usage cache entries retain their originating connection identity
and readers filter revisions after releasing the cache lock. Web search holds the same
execution lease as completion calls.
