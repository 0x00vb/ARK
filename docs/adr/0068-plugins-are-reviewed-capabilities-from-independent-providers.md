# 0068. Plugins are reviewed capabilities from independent providers

Status: Proposed
Date: 2026-10-14

## Context

ARK can start local MCP servers today and owns first-party account connections under ADR 0011.
It has no public catalog, portable package format or remote MCP transport. A catalog must add
useful integrations without making one integration vendor part of ARK's runtime or authority.

Agent Plugins 1.0 packages skills and optional MCP servers. The official MCP Registry provides
server metadata, not code review. Activepieces Community Edition provides a large MIT-licensed
action catalog and an OAuth-protected MCP endpoint. The endpoint operates in a project context,
but multiple Projects and their administrative isolation are a paid Activepieces feature, not a
Community Edition boundary. Its managed OAuth path uses `secrets.activepieces.com`. These are
different source, tenancy and trust models and must not be presented as one kind of installed file.

## Decision

ARK has a provider-neutral plugin catalog. A catalog entry says what becomes available and which
provider supplies it. The runtime knows portable packages, local MCP servers and remote MCP
servers. It does not name Activepieces or another catalog provider.

Only a person can add, update, remove, connect or enable a plugin. An agent can ask for a missing
capability through the existing approval path, but cannot perform those operations itself.

### Kinds and state

| Kind | What Add persists | What runs |
|---|---|---|
| Agent Plugin | A pinned package source, version and digest in the computer's plugin store | Reviewed skills only in the first release; server execution waits for the matching remote or sandbox contract |
| MCP server | A pinned endpoint or local command and its declared metadata | That server |
| Provider integration | A provider binding and integration id for one ARK Account | The provider's already deployed runtime |
| Personal | The same kind-specific record from a person's private source | The selected package or server |

An Activepieces Gmail row is a provider integration. Add does not claim to download Gmail code.
Its detail says that Activepieces supplies it and states the endpoint, auth path, provider catalog
or server revision, schema digest and update policy. Activepieces Cloud is marked
provider-managed. A self-hosted deployment shows an image digest only when ARK verified that pin.

Four states stay separate:

| State | Meaning |
|---|---|
| Added | The computer knows the package or provider binding |
| Connected | An account credential exists for the integration |
| Enabled | One project may offer its tools on its next turn |
| Approved | One invocation passed ARK's existing approval gate |

Adding, connecting or enabling never approves an invocation. Tools are fixed when a turn starts.
The next turn sees an enable, disable, update or removal.

Package skills follow the same account-level enablement. They are absent from the model's skill
catalog until enabled for that account. Disable or removal removes them from the next turn's
catalog.

### Catalog and installation

Browse may fetch bounded signed metadata and cache it with its fetched time. The application pins
ARK's catalog trust root. A signed snapshot binds a monotonic revision, creation and expiry times,
the next key when rotating and an emergency revocation list. An unknown, expired, revoked or older
snapshot fails closed without changing installed plugins. A root-key change needs a native release.
Browse does not fetch package contents, start a process, connect to MCP, create a provider resource
or begin OAuth. Offline Browse shows the newest valid cached data as stale. An explicit confirmation
in a detail view is the only Add action.

Every package record pins its source, version and digest. Validation rejects unsupported manifests,
path or symlink escapes, lifecycle scripts, missing executables, incompatible systems and namespace
collisions with built-ins, packages, project skills or personal skills. Installation stages into a
new directory, validates there and swaps atomically. A failed update leaves the prior version and
`PLUGIN_DATA` in place. Partial packages report valid skills separately from a failed MCP server.

The first marketplace release does not execute a downloaded local MCP server. It can add a reviewed
skill-only package or a remote provider integration. A package with a stdio server is shown as
incompatible until ARK has a subprocess sandbox with a clean environment, read-only package root,
writable `PLUGIN_DATA`, no home or project access without a separate grant, network denied or
host-allowlisted, contained children and explicit secret injection.

Manually configured local servers remain in Settings > Advanced. They inherit ARK's process
environment plus the values the person enters, and have the person's filesystem, network and
child-process rights. The screen states this full-machine-access warning before it starts the
server. Manual servers are not Public marketplace installations.

Removal asks separately whether to keep plugin data and whether to disconnect and revoke accounts.
Those defaults are keep data and keep accounts. A failed removal leaves a visible recoverable state.

### Provider selection

The launch default is a Composio adapter. Its Free plan currently provides 100,000 tool calls each
month, with up to 20,000 through Composio-managed applications, 1,000 managed connections and a
hard stop instead of overage. These are observed provider limits, not ARK entitlements. The UI does
not promise those numbers, and the owner rechecks them before release.

Composio requires its project API key to remain on a server. ARK Desktop never receives that key or
a generated MCP configuration containing it. The existing ARK Cloud API is a small authenticated
broker for managed integrations. Managed plugins therefore require an ARK Account. The broker maps
an immutable ARK account to a stable opaque provider tenant id, creates a session
with an explicit toolkit and tool allowlist, disables Composio connection meta-tools and sandbox,
starts hosted authorization, executes one reviewed operation and revokes a connection. It never
lets Composio choose ARK's tools, approvals or agent behavior.

The broker holds only its Composio project key, provider resource ids and bounded audit metadata.
Composio holds connected account credentials. Action inputs and outputs transit the broker and
Composio, so the connection detail states both processors and Composio's current retention policy
before sign-in. ARK logs ids, status, timing and byte counts, never action payloads or credentials.

Every signed provider offer carries generic delivery, prerequisites, credential-custodian and
payload-processor disclosures. The UI renders those fields without provider-specific branches.
The selected offer alone supplies installed authentication, update and disclosure state.

The broker creates a short-lived authorization session for Connect and a separate exact-operation,
exact-connected-account session for each execution. It deletes each on every terminal outcome and
reconciles abandoned sessions. No provider session is durable chat state. A durable ARK invocation
id and request digest prevent replay for the lifetime of the originating turn. After a dispatch
whose effect cannot be confirmed, the broker returns `effect_unknown` and never automatically
repeats a mutation. The broker accepts an allowlisted return-destination id, not a client URL, and
binds the single-use callback state to the account, offer, destination, nonce and expiry.
Each invocation also carries an opaque, tenant-bound turn reference. The runtime persists a random
per-turn nonce and obtains that reference through an authenticated idempotent broker binding. Raw
chat and turn ids never leave the device. Permanent turn deletion commits a durable local outbox
item, then sends an authenticated idempotent release to remove that turn's tombstones. Project or
account deletion releases any that remain.

The self-hosted escape hatch remains a user-owned remote MCP or Activepieces endpoint. Its
credential stays on the selected runtime and calls the endpoint directly. It does not require an
ARK Account. Activepieces Community Edition is not installed on ARK's 1 vCPU, 2 GB control-plane
host.

Every provider operation defaults to `Mutate`. Provider risk hints may only make risk stricter.
Narrowing needs an ARK-reviewed assertion in signed catalog metadata bound to the provider, adapter
revision, operation id and schema digest. A changed schema, new operation or missing assertion
returns to `Mutate`.

Each ARK release bundles a reviewed catalog snapshot, so first use and offline use need no catalog
host. A later refresh may fetch one bounded signed JSON snapshot from the existing ARK Cloud API.
That optional route performs no provider discovery, contains no credentials and cannot execute
actions. The bundle is the initial offline baseline only. After ARK accepts a newer valid signed
revision, refresh failure retains that newest accepted snapshot and never restores the older bundle.
ARK remains usable without ARK Cloud.

A provider boundary belongs to one ARK Account. `ark-connect` keeps each provider binding behind an
opaque connection reference and derives a stable non-secret boundary identity. For the managed
adapter it includes the provider id and ARK's opaque provider tenant id. For remote MCP it includes
the provider id, canonical OAuth issuer or instance origin, stable instance id when available and
authenticated provider project id. The plugin manager compares this identity before binding a
connection. Duplicate credentials or a rotated URL cannot bypass the comparison. The connection
can be used from every project that belongs to that account. A remote provider without a stable
authenticated identity cannot be reused across accounts.

There are three valid paths for a later ARK-operated provider:

| Path | Isolation | Cost and tradeoff |
|---|---|---|
| Licensed Activepieces Projects | One product or tenant per Activepieces project and scoped MCP credential | Activepieces Team is currently $200 per month. Recheck before purchase |
| Separate Community Edition instances | Physical database, encryption key and endpoint per product or tenant | No license fee, but memory and operational cost scale per instance |
| ARK-owned pieces runner | ARK uses pinned MIT piece packages while owning credentials, tenancy and execution | Largest engineering cost; no dependency on Activepieces paid project features |

One Community Edition instance on a separate suitable host remains acceptable for an owner-only
interoperability proof. It is not required for the operational first release and is not accepted as
the eventual public marketplace backend merely because the MCP tools call themselves project-scoped.

Public multi-user use requires a product-neutral tenant mapping before deployment:

`consumer id + consumer tenant id -> isolated execution boundary + scoped credential`

Each deployment path binds that invariant to its own isolation mechanism:

| Path | Required binding |
|---|---|
| Licensed Activepieces Projects | Activepieces identity, isolated project and project-scoped MCP credential |
| Separate Community Edition instances | Distinct instance endpoint, personal project and instance-scoped MCP credential |
| ARK-owned pieces runner | Isolated runner tenant and tenant-scoped credential |

For any managed provider, the consumer tenant is one ARK Account. Provisioning, authorization,
deletion and retention use that same key. Two accounts and provider accounts must be unable to
list, select or execute with each other's connections. Until
that proof passes, the managed adapter is unavailable and only user-owned endpoints can connect.

### Remote MCP contract

| Boundary | Rule |
|---|---|
| Address | HTTPS only. Plain HTTP is allowed only on an explicit loopback development address. User information and credentials in URLs are rejected. |
| Network | Public addresses are the default. Loopback or private targets require an explicit local-server choice. Resolve and check every connection so DNS cannot move a public name to a private address. |
| Redirects | Off by default. An explicitly allowed redirect stays on the original HTTPS origin. Authorization and cookies are stripped before any origin change. |
| OAuth | Authorization-server and protected-resource discovery must remain on allowed HTTPS origins. Authorization Code with PKCE S256 is required. State is single-use and bounded. |
| Limits | Connect is 10 seconds, handshake and tool listing are 20 seconds, response bodies are at most 16 MiB, and displayed errors are at most 32 KiB. Tool calls obey turn cancellation and time limits. |
| Sessions | Session identifiers are scoped to one endpoint and credential, never persisted in catalog data, and closed or discarded on disconnect, removal and restart. |
| Secrets | Credentials use the existing secret store. Headers and tokens are redacted from logs, errors, traces and returned content. |

### Interface

Settings uses ARK's own capability catalog: installed items, search, category sections and compact
rows. It uses ARK's Projects, Chats, Agents, Capabilities and approvals model, tokens and words.

Selecting a row opens details. The view shows source, provider, pinned version or revision, digest
where applicable, endpoint or executable, access, account scope and update policy. Add changes
nothing until its explicit confirmation. After every add, refusal, update or removal, the row,
Installed sidebar, focus and status agree. Loading, empty, stale, offline, refused, partial, failed,
updating and removal-failed states are visible in dark, small-window, keyboard and reduced-motion
modes.

## Consequences

- ARK can use Composio's catalog and managed authorization without naming it in runtime or UI
  state contracts.
- A provider integration and an installed package share one catalog but never claim the same
  storage or trust semantics.
- Managed integrations require an ARK Account and send bounded action payloads through ARK's API
  and Composio. Local ARK without an account retains Personal remote MCP and package sources.
- The existing API receives a small broker workload but no Activepieces workload. Composio holds
  connected account credentials; ARK holds only its provider key and opaque resource ids.
- Switching the default provider does not move credentials. A person reconnects each account.
  Provider-specific operations without a compatible replacement remain pinned or unavailable.
- A remote MCP client and installer add security-sensitive code before the catalog UI can become
  functional.
- The existing 1 vCPU, 2 GB ARK Cloud host is not an Activepieces deployment target. A later
  optional route may return the bounded signed catalog JSON from memory with strict size and cache
  headers. The first release does not require that route because its catalog is bundled.

## Check

The exact public contract, state transitions and interface mapping are in
[`../PLUGINS.md`](../PLUGINS.md).

1. Browse network tests prove metadata-only access and stale offline behavior.
2. Catalog tests reject valid but expired, revoked, unknown-key and rollback snapshots without
   disturbing installed plugins.
3. Package fixtures prove validation, deterministic collisions, atomic update and removal choices.
   A package with a local executable cannot run. A hostile manual-server fixture proves the warning
   and documents its inherited environment, filesystem, network and child access.
4. A running turn keeps its tools and skills while the next turn sees each state change.
5. Remote MCP tests cover address policy, redirects, rebinding, OAuth origins, limits,
   cancellation, cleanup and redaction.
6. Remote risk tests cover a lying `readOnlyHint`, a changed reviewed schema and an unreviewed new
   tool. All remain `Mutate`.
7. The Composio proof covers hosted authorization, catalog and schema revisions, a read, an
   approved mutation, refresh, revoke, hard quota exhaustion, provider failure and redaction.
8. Two-account, two-project and two-user tests prove that the broker cannot list, select or execute
   another tenant's connection. Two accounts for one toolkit in the same tenant prove each call
   pins the selected account and never falls back to the most recent account. Guessed ids, replayed
   callbacks and copied connect links fail.
9. A provider-swap test runs the same generic catalog and interaction fixtures against Composio and
   a fake second adapter. The core, approval gate and UI have no provider branch. Switching retains
   the old binding until a compatible new binding succeeds and always requires reconnection.
10. Broker tests enforce request, response, error, deadline, concurrency, rate, idempotency and
    retention limits. Restart and delayed-retry tests prove invocation tombstones outlive a process
    and do not expire while the durable turn can resume. Turn-binding tests cover issuance,
    same-tenant replay and cross-tenant separation. Turn-deletion tests cover outbox retry,
    duplicate release, cross-tenant refusal and cleanup after acknowledgement. Reconciliation removes expired
    authorization and execution sessions on success, failure, cancellation, timeout and crash.
    Callback tests reject arbitrary destinations, copied links, substituted destinations and replay.
11. Page tests and real WebKit dogfood cover every interface state and keyboard focus.
12. ARK ships its contract first with a tag. Cloud implements against that tag. The owner separately
    authorizes broker deployment. Desktop then updates its immutable pin before live dogfood.
