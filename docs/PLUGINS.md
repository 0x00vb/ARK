# Plugins

The decision and security boundary are in
[ADR 0068](adr/0068-plugins-are-reviewed-capabilities-from-independent-providers.md). This file
specifies the provider-neutral contract and the Settings interface.

## Status, 2026-10-14

The first implementation slice is in this tree. It supplies the typed, vendor-neutral catalog
and provider boundary, persistent inert Add records, shared host calls, and the Settings › Plugins
catalog. Add is deliberately harmless. It does not start code, connect an account or let an agent
call a service.

The managed broker lives in `ark-cloud/api`. ARK Desktop bridges its signed-in Account to that
broker, where configured. Connections belong to the person's ARK Account and are available across
their projects, matching the account-wide model in Codex. The next slice still needed before any
integration can reach a turn is conversion of a selected operation into an ordinary ARK tool and
the existing approval path. No unsigned local fallback catalog is shown when the broker has no
reviewed offers.

## Ownership

| Layer | Owns |
|---|---|
| `ark-runtime::plugins` | Stable ids, catalog records, installed records, component state and persistence |
| `ark-tools::mcp` | Local and remote MCP transport, endpoint policy and safe risk defaults |
| `ark-connect` | Opaque account references, OAuth, secrets and stable non-secret provider-boundary identities |
| `ark-interact::plugins` | Person-only operations and the view model shared by terminal and window |
| `ark-ui` | The Settings screen, focus and feedback |

No operation below is an agent tool. A window host call or terminal command may invoke it only on
the person's behalf.

## Public contract

All persisted documents have `schema_version`. New optional fields use `#[serde(default)]` and
unknown catalog fields stay in `extra`. Stable ids are lower-case ASCII with letters, digits,
hyphens and dots. Display names are never ids.

```rust
pub struct PluginId(pub String);
pub struct CatalogId(pub String);
pub struct ProviderId(pub String);
pub struct ConnectionRef(pub String); // opaque outside ark-connect
pub struct ProviderBoundaryRef(pub String); // non-secret opaque identity outside ark-connect
pub struct ProviderOfferId(pub String);
pub struct ProviderBindingRef(pub String); // opaque outside ark-connect or the broker
pub struct ProviderTenantRef(pub String); // opaque account boundary
pub struct CapabilityOperationId(pub String);
pub struct ProviderInvocationId(pub String); // stable durable tool invocation id
pub struct ProviderTurnRef(pub String); // opaque broker-scoped identity for one durable ARK turn
pub struct ReturnDestinationId(pub String); // server allowlist key, never a URL
pub struct OperationId(pub String);
pub struct PluginRef { pub catalog: CatalogId, pub plugin: PluginId }

pub enum CatalogScope { Public, Personal }

pub struct CatalogKey {
    pub id: String,
    pub public_key: String,
    pub valid_from: OffsetDateTime,
}

pub enum PluginIcon {
    BuiltIn { name: String },
    Embedded { media_type: IconMediaType, digest: String, data_base64: String },
}

pub enum IconMediaType { Png, Webp }

pub struct CatalogSnapshot {
    pub schema_version: u16,
    pub catalog: CatalogId,
    pub scope: CatalogScope,
    pub revision: u64,
    pub issued_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
    pub key_id: String,
    pub entries: Vec<CatalogEntry>,
    pub revoked_entries: Vec<PluginId>,
    pub next_keys: Vec<CatalogKey>,
    pub extra: Map<String, Value>,
}

pub struct CatalogEntry {
    pub id: PluginId,
    pub name: String,
    pub summary: String,
    pub publisher: String,
    pub icon: Option<PluginIcon>,
    pub categories: Vec<String>,
    pub version: String,
    #[serde(flatten)]
    pub kind: EntryKind,
    pub access: Vec<String>,
    pub extra: Map<String, Value>,
}

#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EntryKind {
    Package {
        source: String,
        digest: String,
        components: Vec<PackageComponent>,
        auth: AuthRequirement,
        update: UpdatePolicy,
    },
    RemoteMcp {
        origin: String,
        server_revision: String,
        schema_digest: String,
        auth: AuthRequirement,
        update: UpdatePolicy,
    },
    ProviderIntegration {
        operations: Vec<CapabilityOperation>,
        offers: Vec<ProviderOffer>,
    },
}

pub struct CapabilityOperation {
    pub id: CapabilityOperationId, // stable ARK id
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub output_schema: Option<Value>,
    pub schema_digest: String,
    pub risk: Risk,
}

pub struct ProviderOffer {
    pub id: ProviderOfferId,
    pub provider: ProviderId,
    pub external_integration: String, // interpreted only by this provider adapter
    pub adapter_revision: String,
    pub operations: BTreeMap<CapabilityOperationId, ProviderOperationRef>,
    pub auth: AuthRequirement,
    pub update: UpdatePolicy,
    pub delivery: ProviderDelivery,
    pub prerequisites: Vec<OfferPrerequisite>,
    pub disclosure: ProviderDisclosure,
    pub opaque: Map<String, Value>, // interpreted only by this provider adapter
}

pub enum ProviderDelivery { Brokered, Direct }
pub enum OfferPrerequisite { ArkAccount, LocalSecretStore, ReachableEndpoint }
pub struct ProviderDisclosure {
    pub credential_custodian: String,
    pub payload_processors: Vec<ProcessorDisclosure>,
}
pub struct ProcessorDisclosure {
    pub name: String,
    pub purpose: String,
    pub privacy_url: Url,
    pub retention: String,
}

pub struct ProviderOperationRef {
    pub external_operation: String, // interpreted only by this provider adapter
    pub provider_schema_digest: String,
    pub mapping_digest: String,
    pub opaque: Map<String, Value>, // adapter-owned argument/result mapping
}

#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PackageComponent {
    Skill { name: String, digest: String },
    LocalServer { namespace: String }, // incompatible in the first release
    RemoteServer { namespace: String, origin: String },
}

pub enum AuthRequirement {
    None,
    Account { provider: ProviderId },
    Secret { fields: Vec<SecretField> },
}
pub struct SecretField { pub id: String, pub label: String, pub required: bool }
pub enum UpdatePolicy { Manual, Pinned }

pub struct InstalledPlugin {
    pub schema_version: u16,
    pub plugin: PluginRef,
    pub pinned_revision: u64,
    pub pinned_entry: CatalogEntry,
    pub added_at: OffsetDateTime,
    pub data_path: Option<PathBuf>,
    pub projects: BTreeMap<ProjectId, ProjectPlugin>,
    pub components: Vec<ComponentResult>,
    pub update: UpdateState,
    pub extra: Map<String, Value>,
}

pub struct ProjectPlugin {
    pub enabled: bool,
    pub selection: Option<ProviderSelection>,
    pub pending_switch: Option<ProviderSwitch>,
}

pub struct ProviderSelection {
    pub offer: ProviderOfferId,
    pub binding: ProviderBindingRef,
    pub operation_schema_digest: String,
}

pub struct ProviderSwitch {
    pub from: ProviderSelection,
    pub to_offer: ProviderOfferId,
    pub candidate: Option<ProviderBindingRef>,
    pub state: ProviderSwitchState,
}

pub enum ProviderSwitchState { Connecting, CheckingCompatibility, Ready, Failed { why: String } }

pub struct ComponentResult {
    pub component: String,
    pub state: ComponentState,
}

pub enum ComponentState { Ready, NeedsConnection, Incompatible { why: String }, Failed { why: String } }
pub enum UpdateState { Current, Available { version: String }, Updating { operation: OperationId }, RollbackRetained { why: String } }
pub enum ProviderHealth { Ready, Degraded { why: String }, Unavailable { why: String, retry_at: Option<OffsetDateTime> } }

pub enum CatalogFreshness {
    Fresh,
    CachedStale { fetched_at: OffsetDateTime, why: String },
    OfflineNoCache { why: String },
    InvalidUsingPrevious { why: String },
    InvalidNoCache { why: String },
    Expired { expired_at: OffsetDateTime },
    Revoked { why: String },
}

pub struct PluginOperation {
    pub id: OperationId,
    pub target: OperationTarget,
    pub kind: OperationKind,
    pub state: OperationState,
    pub cancellable: bool,
}

pub enum OperationTarget { Plugin(PluginRef), Catalog(CatalogId), Source }
pub enum OperationKind {
    RefreshCatalog, RegisterSource, Add, Connect, Enable, Disable, Update, Remove,
    Retry { original: RetryKind },
}
pub enum RetryKind { RefreshCatalog, RegisterSource, Add, Connect, Enable, Disable, Update, Remove }
pub enum OperationState { Pending, Running, Refused, Failed { why: String }, Completed, Cancelling, Cancelled }

pub struct PluginCatalogView {
    pub schema_version: u16,
    pub catalogs: Vec<CatalogView>,
    pub installed: Vec<InstalledRow>,
    pub source_operations: Vec<PluginOperation>,
}

pub struct CatalogView {
    pub id: CatalogId,
    pub scope: CatalogScope,
    pub revision: Option<u64>,
    pub freshness: CatalogFreshness,
    pub operation: Option<PluginOperation>,
    pub entries: Vec<PluginRow>,
}

pub struct PluginRow {
    pub plugin: PluginRef,
    pub name: String,
    pub summary: String,
    pub icon: Option<PluginIcon>,
    pub state: EntryState,
    pub provider: Option<ProviderHealth>,
    pub operation: Option<PluginOperation>,
    pub compatibility: EntryCompatibility,
    pub badges: Vec<RowBadge>,
    pub primary_action: Option<PluginAction>,
}

pub struct InstalledRow {
    pub plugin: PluginRef,
    pub name: String,
    pub icon: Option<PluginIcon>,
    pub state: EntryState,
    pub badges: Vec<RowBadge>,
}

pub struct PluginDetail {
    pub catalog_entry: CatalogEntry,
    pub plugin: PluginRef,
    pub catalog_scope: CatalogScope,
    pub catalog_freshness: CatalogFreshness,
    pub state: EntryState,
    pub installation: InstallationFacts,
    pub provider: Option<ProviderHealth>,
    pub components: Vec<ComponentResult>,
    pub operation: Option<PluginOperation>,
    pub compatibility: EntryCompatibility,
    pub actions: Vec<PluginAction>,
}

pub struct InstallationFacts {
    pub installed: Option<PinnedInstallation>,
    pub connection: Applicable<ConnectionState>,
    pub enabled: bool,
    pub update: UpdateState,
    pub candidate: Option<UpdateCandidate>,
}

pub struct PinnedInstallation {
    pub catalog_revision: u64,
    pub entry: CatalogEntry,
}

pub struct UpdateCandidate {
    pub catalog_revision: u64,
    pub entry: CatalogEntry,
}

pub enum Applicable<T> { NotApplicable, Value(T) }
pub enum EntryCompatibility {
    Compatible,
    PartiallyCompatible { why: Vec<String> },
    Incompatible { why: Vec<String> },
}
pub enum ConnectionState { Missing, Connecting, Connected { label: String }, Failed { why: String } }
pub enum EntryState {
    Available, Adding, Connecting, Enabling, Disabling, Refused, OperationFailed,
    NeedsConnection, ConnectedDisabled, Enabled, Partial, ProviderUnavailable,
    UpdateAvailable, Updating, RollbackRetained, Removing, RemovalFailed,
    CatalogBlocked, Revoked,
}
pub enum RowBadge { Cached, CatalogWarning, Revoked, Partial, ProviderDegraded, ProviderUnavailable, UpdateAvailable }
pub enum PluginAction { Inspect, Review, Add, Connect, TurnOn, TurnOff, Update, Retry, Remove, Cancel }

pub enum ConnectStart {
    Browser { operation: OperationId, url: String },
    SecretForm { operation: OperationId, fields: Vec<SecretField> },
}

pub enum RetryStart {
    Operation(OperationId),
    Connection(ConnectStart),
}

/// A person-provided value. It cannot be serialized or cloned, Debug prints `[redacted]`, and
/// Drop clears its allocation before release.
pub struct SecretInput(/* private bytes */);

#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceRegistration {
    PackageFile { path: PathBuf },
    CatalogUrl { url: String, key_id: String, public_key: String },
}
```

`PluginIcon::Embedded` is part of the signed snapshot, not a URL. It accepts only decoded PNG or
WebP bytes up to 24 KiB and 128 by 128 pixels. Animated images, trailing data and digest mismatches
are rejected. Browse never fetches an icon from another origin.

A catalog response is limited to 16 MiB before allocation and 12 MiB after decoding. Parsing keeps
Serde JSON's 128-level recursion limit. A snapshot has at most 2,000 entries, 32 categories, 16
categories per entry, 64 access statements per entry and 8 MiB of decoded icons in total. Ids,
versions and category names are at most 128 bytes, display names and publishers 256 bytes, summaries
1,024 bytes, access statements 512 bytes and technical strings 4,096 bytes. Exceeding any limit is
`InvalidUsingPrevious` or `InvalidNoCache` and does not partially accept the snapshot.

A Secret requirement has at most 16 fields. Field ids follow the stable-id rules and labels are at
most 128 bytes. Secret values never enter catalog, operation, event, log or error documents. The UI
uses password inputs, never restores a submitted value, and clears the form on Cancel. `submit_secret`
accepts only fields declared by the signed entry, validates required fields, gives the values directly
to `ark-connect`, and persists only the returned opaque `ConnectionRef`. Browser callback parameters
also terminate inside `ark-connect`; the plugin layer observes only operation state and the opaque
reference. A validation or provider failure records a redacted safe detail and permits Retry without
retaining values. `cancel(operation)` closes either flow and asks `ark-connect` to discard pending
state. Retrying Connect always creates a new operation. Browser retry returns a fresh URL with new
single-use PKCE and state. Secret retry returns a blank SecretForm. The terminal operation remains
queryable for history but is replaced on the row by the new operation. Retrying every other kind
returns `RetryStart::Operation` with its new operation id.

The public methods are asynchronous except `inspect` and `list`:

```rust
trait PluginManager {
    fn list(&self, project: &ProjectId) -> PluginCatalogView;
    fn inspect(&self, project: &ProjectId, plugin: &PluginRef) -> Result<PluginDetail, PluginError>;
    fn operation(&self, operation: &OperationId) -> Option<PluginOperation>;
    async fn refresh_catalog(&self, catalog: &CatalogId) -> Result<OperationId, PluginError>;
    async fn register_source(&self, source: SourceRegistration) -> Result<OperationId, PluginError>;
    async fn add(&self, project: &ProjectId, plugin: &PluginRef, revision: u64) -> Result<OperationId, PluginError>;
    async fn connect(&self, project: &ProjectId, plugin: &PluginRef) -> Result<ConnectStart, PluginError>;
    async fn submit_secret(&self, operation: &OperationId, values: BTreeMap<String, SecretInput>) -> Result<(), PluginError>;
    async fn enable(&self, project: &ProjectId, plugin: &PluginRef, on: bool) -> Result<OperationId, PluginError>;
    async fn update(&self, project: &ProjectId, plugin: &PluginRef, revision: u64) -> Result<OperationId, PluginError>;
    async fn remove(&self, project: &ProjectId, plugin: &PluginRef, choice: RemoveChoice) -> Result<OperationId, PluginError>;
    async fn retry(&self, operation: &OperationId) -> Result<RetryStart, PluginError>;
    async fn cancel(&self, operation: &OperationId) -> Result<(), PluginError>;
}

pub struct RemoveChoice { pub keep_data: bool, pub keep_account: bool }
pub enum PluginEvent {
    CatalogChanged { catalog: CatalogId, revision: Option<u64>, freshness: CatalogFreshness },
    PluginChanged { plugin: PluginRef },
    OperationChanged { operation: PluginOperation },
}
pub enum PluginError {
    NotFound, RevisionChanged, CatalogUnavailable, CatalogExpired, CatalogRevoked,
    EntryRevoked, InvalidSignature, Incompatible, Collision { name: String },
    NeedsConnection, ProviderUnavailable, Refused, OperationConflict, Cancelled,
    Transport { safe_detail: String }, Persistence { safe_detail: String },
}
```

Window host calls use the same verbs with a `plugin_catalog_` prefix. The terminal mirrors them
under `/plugin catalog`. Existing `/plugin add` continues to mean a manually configured local
server under Advanced.

## Catalog failure behavior

| Condition | API freshness | Browse | Installed entries |
|---|---|---|---|
| Fetch fails, valid cache | `CachedStale` | Show cache, timestamp and Retry. Add is allowed until expiry | Unchanged |
| Fetch fails, no cache | `OfflineNoCache` | Offline state and Retry. No empty-catalog message | Unchanged |
| New snapshot invalid, no cache | `InvalidNoCache` | Invalid-catalog state and Retry. No rows are accepted | Unchanged |
| New snapshot invalid, previous valid | `InvalidUsingPrevious` | Show previous snapshot with warning. Add is allowed until expiry | Unchanged |
| Cached snapshot expired | `Expired` | Rows remain inspectable. Add and Update are disabled. Retry is available | Unchanged and runnable |
| Catalog key revoked | `Revoked` | No new Add or Update. Explain the withdrawal | Installed rows remain visible and keep their persisted enabled state |
| Entry revoked later | entry status `revoked` | Entry cannot be newly added or updated | Show Review. Turn off and Remove remain available. Never silently uninstall or disable |

Search with zero matches says `No plugins match “…”`. A valid catalog with no entries says
`No plugins are available`. Offline without a cache never uses either message.

## State and transition model

`Connected` is not applicable when `auth` is `None`. `Approved` is invocation state and never
appears as installation state.

| State | Row status and primary action | Disabled | Persisted | Next turn |
|---|---|---|---|---|
| Loading | Skeleton rows, no action | All row actions | Nothing | Unchanged |
| Detail | Current state, action below access summary | None valid for state | Nothing | Unchanged |
| Confirming Add | `Add` in detail | Row Add | Nothing | Unchanged |
| Adding | `Adding...` | Add, Update, Remove | Operation only | Unchanged |
| Refused | Inline `Not added`, Add | None | Nothing | Unchanged |
| Added, needs connection | `Connect` | Turn on | Installed record | No new tools or skills |
| Connected, disabled | `Turn on` | Connect | Connection and disabled project state | No new tools or skills |
| Enabled | `On`, overflow has Turn off and Remove | Connect | Enabled project state | Tools and skills present |
| Partial | `Review` | Failed component only | Per-component results | Ready components only |
| Provider unavailable | `Retry` | Connect and Turn on | Installed state and health | Previously enabled tools remain but calls fail clearly |
| Update available | `Update` | None | Available revision | Existing version remains |
| Updating | `Updating...` | Add, Remove, another Update | Operation only | Existing version remains |
| Rollback retained | `Update failed`, Retry | None | Prior version plus failure | Prior version remains |
| Removal choices | `Remove` confirmation | Row Remove | Nothing yet | Unchanged |
| Removing | `Removing...` | All mutations | Operation only | Current turn unchanged |
| Removal failed | `Remove failed`, Retry | Add and Update | Installed record remains | Prior state remains |
| Removed | Public row returns to Add | None | Record removed per choice | Removed on next turn |
| Catalog stale | `Cached`, Retry in header | Update after expiry | Cache metadata | Unchanged |
| Catalog invalid or revoked | `Review` | Add and Update | Prior cache and installed records | Persisted enabled state remains |

Cancel is shown only while the operation can stop before its atomic swap. After the swap starts,
Cancel is disabled and the operation completes or rolls back.

`ark-interact` derives each row through one total precedence function. The first matching rule
supplies the primary state and action. Lower rules may add badges but cannot replace that state or
action. Invalid combinations are reconciled before view derivation: a persisted `Updating` must
name a live or terminal Update operation, and an enabled plugin whose required connection is lost
is treated as `NeedsConnection` until reconnection.

Operation target is constrained by kind. Refresh targets one Catalog, RegisterSource targets
Source, and Add, Connect, Enable, Disable, Update and Remove target one Plugin. Retry retains the
original non-Retry kind and the same target. Other combinations are rejected as `OperationConflict`.

For a Plugin target, operation state is applied first:

| Operation state | Add | Connect | Enable | Disable | Update | Remove |
|---|---|---|---|---|---|---|
| `Pending` or `Running` | `Adding` | `Connecting` | `Enabling` | `Disabling` | `Updating` | `Removing` |
| `Cancelling` | Same busy state, no action | Same | Same | Same | Same | Same |
| `Refused` | `Refused`, Inspect | `Refused`, Inspect | `Refused`, Inspect | `Refused`, Inspect | `Refused`, Inspect | `Refused`, Inspect |
| `Failed` | `OperationFailed`, Retry | `NeedsConnection`, Retry | `ConnectedDisabled`, Retry | `Enabled`, Retry | `RollbackRetained`, Retry | `RemovalFailed`, Retry |
| `Cancelled` or `Completed` | Recompute from persisted state after showing transient feedback | Same | Same | Same | Same | Same |

Retry in `Pending`, `Running` or `Cancelling` uses the busy state of its `original` kind. Retry in
`Refused`, `Failed`, `Cancelled` or `Completed` uses that row of the table for its original kind.
Cancel is the primary action only in `Pending` or `Running` while `cancellable` is true. When it is
false, or once cancellation starts, a busy row has no primary action. Terminal operations stay
queryable by id but leave the row after their feedback is acknowledged.

If no Plugin operation overrides it, base state uses this order:

| Priority | Condition | Primary state and action |
|---|---|---|
| 1 | The entry is revoked | `Revoked`, Review. Turn off and Remove remain available in detail |
| 2 | The catalog is expired or revoked and the entry is not installed | `CatalogBlocked`, Review |
| 3 | The entry is not installed and compatibility is `Incompatible` | `Partial`, Review |
| 4 | The entry is not installed and compatibility is `PartiallyCompatible` | `Partial`, Review; detail may add only the compatible components |
| 5 | The entry is not installed and compatibility is `Compatible` | `Available`, Add |
| 6 | The installed update is `RollbackRetained` | `RollbackRetained`, Retry |
| 7 | The provider is `Unavailable` | `ProviderUnavailable`, Retry |
| 8 | Any installed component is incompatible or failed | `Partial`, Review |
| 9 | Required connection is `Connecting` | `Connecting`, no action |
| 10 | Required connection is `Missing` | `NeedsConnection`, Connect |
| 11 | Required connection is `Failed` | `NeedsConnection`, Retry |
| 12 | Installed, connection is `Connected` or `NotApplicable`, and project state is disabled | `ConnectedDisabled`, Turn on |
| 13 | A valid newer candidate exists | `UpdateAvailable`, Update |
| 14 | The installed entry is enabled | `Enabled`, Inspect |

Connection, provider health and installed component rules run only after an installed record exists.
`AuthRequirement::None` always uses `Applicable::NotApplicable`. Account and Secret always use
`Applicable::Value`, so every installed connection applicability and state is covered.
`ProviderHealth::Ready` does not add a badge, `Degraded` adds `ProviderDegraded`, and `Unavailable`
produces the state above. Empty or all-Ready component lists pass priority 8.

RefreshCatalog is displayed in its Catalog header. Pending and Running show a busy refresh control;
Cancelling shows the same control without Cancel unless `cancellable`; Failed shows Retry; Refused
shows Review; Cancelled recomputes from cached freshness; Completed recomputes from the accepted
snapshot. RegisterSource appears in `source_operations` and its Add menu flow. Pending and Running
show validation in progress; Cancelling is busy; Failed preserves all inputs and shows Retry;
Refused preserves inputs and shows Review; Cancelled returns to the editable form; Completed opens
the registered Personal source. Retry uses the same mapping as its original kind.

Catalog freshness constrains this order without hiding persisted installation state. `Expired` or
catalog `Revoked` disables Add and Update. An uninstalled entry then becomes `CatalogBlocked`,
Review. An installed entry keeps the state derived above and adds `CatalogWarning`. A stale but
still valid cache adds `Cached` and permits Add until expiry. Entry revocation always takes priority
over catalog freshness. Badges accumulate in the fixed order `Revoked`, `CatalogWarning`, `Cached`,
`ProviderUnavailable`, `ProviderDegraded`, `Partial`, `UpdateAvailable`; duplicates are removed. A
row exposes only the primary action. Detail may expose other safe actions named by the transition
table.

## Screen specification

The wide screen follows the supplied Codex structure.

| Area | Content and behavior |
|---|---|
| Customize sidebar | Plugins and Skills first. Installed plugin rows follow under Installed. Partial, unavailable, updating and failed entries show a small status mark and accessible status text |
| Header | Plugins, search, refresh, settings and Add menu |
| Tabs | Public and Personal as a `tablist`. Switching by keyboard is instant and retains search |
| Sections | Category heading followed by a two-column list. Heading is text unless it opens a complete category view, when it is a button |
| Row | Icon, name, one-line outcome and a separate action. The name area opens detail. The action is `+` with `aria-label="Add NAME"`, or text for Connect, Turn on, Update, Retry and Review |
| Detail | Back, icon, name, publisher, summary, access, account requirement and primary action. Technical details disclose kind, source, pinned version or revision, digest, endpoint or executable and update policy |
| Remove sheet | Keep data on by default. Keep account on by default. Remove is destructive and separate from Cancel |

When an update is available, detail shows `Installed` and `Update` blocks side by side on wide
screens and in that order on small screens. Each block shows its version, catalog revision, source
and digest. Update confirmation names both versions.

The Add menu has three first-release choices:

| Choice | Behavior |
|---|---|
| From file | A native file picker returns a package path to `register_source(PackageFile)`. Only reviewed skill components can become compatible in this release |
| Catalog URL | Collect an HTTPS URL, key id and the full public key by paste or file import. Derive and show its fingerprint for confirmation, then call `register_source(CatalogUrl)`. Never obtain the trust key from that catalog URL |
| Tool server | Navigate to Settings > Advanced > Tool servers and show the full-machine-access warning. It does not register a marketplace source |

Registered packages and catalogs appear under Personal. Source validation finishes before an entry
can be added. A failed registration keeps the menu input and shows its error inline. Personal
catalog identity is its catalog id plus plugin id, so a private fork may use the same plugin id as
a Public entry without collision.

Add from a row opens detail and focuses the primary Add confirmation. It does not add immediately.
Escape returns from detail to the originating row. Escape closes a menu or removal sheet without
changing state. After Add, Connect, Turn on, Update, Retry or refusal, focus stays on that plugin's
new primary action. After removal, focus returns to its Public or Personal row. Inline errors appear
below the affected action, keep the detail and scroll position, say what happened and offer Retry
when retry is safe.

Keyboard order is Customize navigation, search, header actions, Public or Personal tabs, category
controls, then each row name and row action. Rows are lists, row names and actions are buttons,
tabs use `tablist`, menus use `menu`, and the removal choice uses `dialog`. Focus never enters a
hidden column or closed detail.

## Responsive and visual rules

| Rule | Mapping |
|---|---|
| Surface | Light is default. Canvas is paper, text is ink, and raised detail or dialog surfaces are white |
| Grouping | Hairlines and whitespace divide rows. No cards inside cards |
| Accent | Blue is only the active tab, focus ring and one selected state. It stays below 5 percent of the screen |
| Targets | Every pointer target is at least 40 by 40 px |
| Wide | Customize sidebar stays fixed. Catalog uses two columns |
| Below 860 px | Catalog becomes one column. Customize becomes the first Settings screen. Choosing Plugins opens the content with a Back control. Installed items remain below Plugins and Skills on the Customize screen |
| Small detail | Detail replaces the list rather than opening beside it. Back restores search, tab, scroll and focus |
| Motion | Keyboard tab changes and navigation are instant. Pointer-opened detail may fade and move at `--ark-dur-2` with `--ark-ease-out`. Press uses `--ark-dur-1` and scale 0.97 |
| Reduced motion | Only opacity and color feedback remain. No scale or movement |

## Design diagnostic

| Check | Result |
|---|---|
| Discoverability | 2: row action and detail action say the next valid step |
| Evaluation | 2: sidebar, row, detail and operation status change together |
| Recovery | 2: retry, rollback, removal choices and preserved state are explicit |
| Mapping | 2: action stays beside its plugin and focus returns there |
| Constraints | 2: invalid operations are absent or disabled and Add requires detail confirmation |

Target: 10/10 before implementation review.

## Provider boundary

The plugin manager depends on one provider interface. Vendor adapters implement it outside the
runtime catalog and interaction view:

```rust
pub trait IntegrationProvider {
    async fn catalog(&self, request: ProviderCatalogRequest) -> ProviderResult<ProviderCatalogPage>;
    async fn begin_connect(&self, request: ConnectRequest) -> ProviderResult<ConnectStart>;
    async fn connection(&self, request: ConnectionRequest) -> ProviderResult<ConnectionStatus>;
    async fn disconnect(&self, request: DisconnectRequest) -> ProviderResult<DisconnectResult>;
    async fn bind_turn(&self, request: BindTurnRequest) -> ProviderResult<BindTurnResult>;
    async fn execute(&self, request: ExecuteRequest) -> ProviderResult<ProviderExecutionOutcome>;
    async fn release_turn(&self, request: ReleaseTurnRequest) -> ProviderResult<ReleaseTurnResult>;
    async fn health(&self) -> ProviderResult<ProviderHealth>;
}

pub struct ProviderCatalogRequest { pub cursor: Option<String>, pub limit: u16 }
pub struct ProviderCatalogPage { pub offers: Vec<ProviderOffer>, pub next: Option<String> }

pub struct ConnectRequest {
    pub tenant: ProviderTenantRef,
    pub plugin: PluginRef,
    pub offer: ProviderOfferId,
    pub return_destination: ReturnDestinationId,
    pub idempotency_key: String,
}
pub struct ConnectStart {
    pub operation: OperationId,
    pub authorization_url: Url,
    pub expires_at: OffsetDateTime,
}

pub struct ConnectionRequest {
    pub tenant: ProviderTenantRef,
    pub binding: ProviderBindingRef,
}
pub struct ConnectionStatus {
    pub binding: ProviderBindingRef,
    pub boundary: ProviderBoundaryRef,
    pub account_label: String,
    pub state: ConnectionState,
}
pub struct DisconnectRequest {
    pub tenant: ProviderTenantRef,
    pub binding: ProviderBindingRef,
    pub revoke_upstream: bool,
    pub idempotency_key: String,
}
pub enum DisconnectResult { Revoked, DisabledRevocationPending }

pub struct BindTurnRequest {
    pub tenant: ProviderTenantRef,
    pub client_nonce: String, // random 256-bit value persisted with the local durable turn
}
pub struct BindTurnResult { pub turn: ProviderTurnRef }

pub struct ExecuteRequest {
    pub tenant: ProviderTenantRef,
    pub turn: ProviderTurnRef,
    pub plugin: PluginRef,
    pub offer: ProviderOfferId,
    pub binding: ProviderBindingRef,
    pub operation: CapabilityOperationId,
    pub arguments: Value,
    pub deadline: OffsetDateTime,
    pub cancellation: CancelToken,
    pub invocation: ProviderInvocationId,
}
pub struct ReleaseTurnRequest {
    pub tenant: ProviderTenantRef,
    pub turn: ProviderTurnRef,
}
pub enum ReleaseTurnResult { Released, AlreadyReleased }
pub struct ProviderExecution {
    pub provider_request: String,
    pub operation_schema_digest: String,
    pub result: Value,
}
pub enum ProviderExecutionOutcome {
    Completed(ProviderExecution),
    CompletedDuplicate {
        provider_request: String,
        operation_schema_digest: String,
    },
    EffectUnknown {
        provider_request: Option<String>,
        safe_detail: String,
    },
}
pub enum ProviderErrorKind {
    Invalid, Refused, NotConnected, Quota, RateLimited, Timeout, Cancelled,
    ProviderUnavailable, IncompatibleSchema, Internal,
}
pub struct ProviderError { pub kind: ProviderErrorKind, pub safe_detail: String, pub retry_at: Option<OffsetDateTime> }
pub type ProviderResult<T> = Result<T, ProviderError>;
```

The trait accepts ARK ids, opaque provider references, bounded JSON values, deadlines,
cancellation and a durable invocation id. It never exposes an SDK object, API key, MCP header or vendor
session type. A fake adapter drives every lifecycle and isolation test.

`ProviderTenantRef`, `ProviderBindingRef` and `ProviderBoundaryRef` are opaque outside
`ark-connect` or the authenticated broker. The provider adapter alone resolves external toolkit,
operation, connected-account and session ids. Before execution it verifies the signed offer maps
the requested ARK operation id to the current provider operation and that every schema and mapping
digest matches. The core and UI never inspect provider ids or mapping data.

`CatalogEntry.id` is the ARK-owned integration identity. A provider offer supplies its external
integration id, adapter revision, schema digest and opaque metadata. An installed plugin records
the selected offer separately from the plugin identity. The UI therefore shows one Gmail row, not
one row per provider. Switching providers stages a new binding, verifies operation compatibility,
then swaps atomically. The old binding remains usable until the swap succeeds. Credentials never
migrate and must be reconnected. A provider-only operation without a compatible replacement stays
pinned to its provider or becomes unavailable with an explanation.

Before provider selection, a catalog row is an aggregate of its signed offers. Add opens detail
when more than one usable offer exists. That detail presents each offer's generic delivery,
prerequisite and processor disclosure fields. It does not branch on provider names. An offer is
usable only when its signed prerequisites are satisfied. Once installed, authentication, update,
delivery and disclosure state come only from the offer pinned in `ProviderSelection`. A switch
shows the candidate offer's disclosures before reconnection and does not change the installed
selection until compatibility succeeds.

### Managed launch adapter

Composio is the default launch adapter. Its project key stays in the existing ARK Cloud API secret
store. Desktop and generated MCP configuration never receive it. Managed plugins require an ARK
Account. The broker derives a stable external tenant id from the immutable ARK account id and ARK
project id, never an email address. It does not retain one provider session per chat. Connect
creates one authorization session for the exact toolkit. Execution creates one session for the
exact toolkit, one provider operation and one connected account, with `manageConnections`
disabled and sandbox disabled.

The runtime sends one approved operation to the broker with its ARK access token, project id,
connection reference, operation id, arguments, deadline and durable invocation id. The broker repeats
tenant, allowlist and size checks before calling Composio. It returns bounded data and redacted
errors. It logs ids, provider log id, status, timing and byte counts. It never logs arguments,
results, authorization URLs, tokens or provider headers.

Composio stores and refreshes connected account credentials. ARK stores its Composio project key,
opaque connected-account ids, tenant mapping, connection status and audit metadata. A live request
may retain an encrypted provider session id only as an ephemeral cleanup lease. It is never durable
chat state.
Action payloads transit both ARK and Composio. The connection detail states this and Composio's
current retention policy before authorization. Free-plan exhaustion pauses managed calls and shows
the provider limit. It never falls through to a different provider or account automatically.

Every managed binding resolves to exactly one validated Composio connected-account id. Every
execution session passes that id in the `connectedAccounts` override for the selected toolkit.
The broker enables multi-account selection, requires an explicit account, and refuses any provider
fallback to the most recent or default account. A second account for the same toolkit creates a
second binding with a distinct label and reference.

### Broker limits and lifecycle

| Boundary | Limit or rule |
|---|---|
| Request | JSON only, 1 MiB body, 16 KiB headers, at most 64 object fields per level and 32 levels |
| Response | 16 MiB provider result; larger results fail closed and are not logged |
| Error | 32 KiB redacted safe detail |
| Time | 10 seconds to create an authorization link, 10 minute authorization expiry, 60 seconds for one execution including provider session create and delete |
| Concurrency | 2 executions per tenant and 8 across the broker; excess returns retryable busy without calling the provider |
| Rate | 30 execution starts per tenant per minute and a configurable global limit no higher than the provider plan allows |
| Idempotency | Scope is provider, tenant, opaque turn, binding, operation and durable invocation id. Keep request SHA-256, `started`, `completed` or `effect_unknown`, and provider request id for the lifetime of the originating turn. Same id and different body returns conflict. A completed duplicate returns `CompletedDuplicate`. A crash after dispatch returns `EffectUnknown`; ARK never automatically repeats an uncertain mutation |
| Execution audit | Provider, tenant, plugin, operation, provider request id, status, timing and byte counts for 30 days. No arguments, results, authorization URLs, credentials or headers |
| Provider sessions | One exact-tool session per execution. Delete it on success, failure, cancellation or timeout. Record an encrypted cleanup lease before dispatch. Reconciliation every 5 minutes deletes sessions older than 15 minutes, including crash orphans |
| Authorization sessions | Delete on success, cancellation or 10 minute expiry. Reconciliation removes abandoned sessions every 5 minutes |
| Disconnect | Disable the binding before provider revocation. Provider failure leaves `revocation_pending`, prevents execution and retries for 24 hours |
| Chat deletion | No provider session should survive a turn. Delete any referenced orphan immediately and let reconciliation catch missed cleanup |
| Permanent turn deletion | ARK sends an authenticated idempotent `release_turn` with the tenant and opaque turn reference after the local deletion commits. The broker deletes that turn's invocation tombstones and returns `AlreadyReleased` on replay. A durable deletion outbox retries until acknowledged. Project or account deletion subsumes every unreleased turn |
| Project or account deletion | Disable every binding, delete audit rows, sessions and live mappings, and request upstream revocation. On provider failure retain only an encrypted provider account id and random deletion receipt, retry for 30 days, make one final attempt, then delete the id and raise an unresolved provider-deletion incident for the owner |

The broker never accepts an external connected-account, toolkit, operation or session id from a
client. It resolves all of them from the authenticated tenant, signed offer and opaque binding.
When a durable turn first needs a managed operation, the runtime generates a random 256-bit nonce
and persists it with that local turn. Authenticated `bind_turn` sends the tenant and nonce. The
broker derives `ProviderTurnRef` from both with a keyed one-way function and returns the same
reference when that tenant replays the same nonce. The same nonce under another tenant produces a
different reference and cannot access the first tenant's records. The broker never accepts a raw
chat or turn id, and the nonce has no meaning outside this binding. Each execution binds the
invocation to that reference. Permanent local turn deletion writes an outbox item in the same
transaction; the authenticated, idempotent `release_turn` call is retried until acknowledged.
It never accepts a callback or return URL from a client. `ReturnDestinationId` selects a fixed
server-side allowlist entry. Authorization state binds the authenticated account, project, offer,
destination, nonce and 10 minute expiry and is single-use. The provider callback is one fixed HTTPS
ARK endpoint. A native return scheme must also be configured in that server-side allowlist.

### Self-hosted adapters

A Personal remote MCP or Activepieces endpoint stays account-optional. Its secret is place-bound to
the selected runtime and calls the endpoint directly. `ark-connect` derives a stable non-secret
`ProviderBoundaryRef` from the provider id, canonical issuer or origin, stable instance id when
available and authenticated project id. Duplicate credentials or a rotated URL cannot bypass
cross-project sharing confirmation. A provider without stable authenticated identity cannot be
reused across projects.

Activepieces is not installed on ARK's 1 vCPU, 2 GB control-plane host. A provider-managed endpoint
shows its observed catalog or server revision, schema digest and update policy, never an image pin.
A self-hosted entry shows an image digest only when ARK verified that exact deployment.

## Implementation plan

| Step | Repo and layer | Deliverable | Gate |
|---|---|---|---|
| 1. Contract | ARK, `ark-runtime` and `ark-connect` | Provider offers separate stable ARK plugin identity from external ids. Add signed generic delivery, prerequisite and processor disclosures, the provider trait, opaque binding, allowlisted return destination, authenticated opaque turn binding, durable invocation outcome and release, boundary identity, selected offer and switch state. Preserve unknown fields and migrate existing draft records | Serialization, rollback, fake-adapter, turn-bind issuance and replay, pre-selection offer aggregation and no-vendor-name tests |
| 2. Catalog | ARK | Generate a bounded signed snapshot from the Composio toolkit catalog. Bundle it with the release. Accept only monotonic signed refreshes. Default every operation to `Mutate` until a reviewed schema assertion narrows it | Tampered, expired, oversized, changed-schema and rollback fixtures |
| 3. Manager | ARK, `ark-interact` | Person-only add, connect, disconnect, enable, disable, retry, remove and switch operations. Freeze provider and schema for a running turn. Keep the old binding through a failed switch | Total state-transition tests in CLI and window view models |
| 4. Broker | ARK Cloud API | Authenticated generic provider routes backed by a Composio adapter. Stable account-plus-project tenant ids, hosted authorization, fixed callback and return-destination allowlists, exact connected-account pinning, sandbox off, direct execute, revoke, hard limits, authenticated turn issuance, turn-bound durable invocation tombstones and idempotent release, retention, cleanup, quotas and redaction | Two-user and two-project isolation; two accounts for one user, project and toolkit; turn issuance, same-tenant replay and cross-tenant refusal; restart and delayed invocation replay; release outbox retry and duplicate release; copied-link and substituted-destination; guessed-id; wrong-account; quota; timeout; every terminal session outcome; crash reconciliation; revoke and log-redaction tests |
| 5. Runtime execution | ARK, tool boundary | Turn enabled provider operations into ordinary ARK tools. Approval happens locally before the broker call. Results use existing size limits, cancellation and trace redaction | One read and one externally visible mutation. Mutation cannot run without approval |
| 6. Interface | ARK UI after steps 1 to 5 | Implement the approved catalog layout. Managed rows say ARK Account required and disclose ARK plus Composio processing before Connect. Personal providers remain available without an account | Empty, loading, offline, quota, expired connection, refused, retry, switch, keyboard, small-window and reduced-motion page tests |
| 7. Swap proof | ARK and Cloud | Run the same contract suite against Composio and a fake second provider. Switch one test integration without changing core, approval or UI code. Require reconnection and preserve the old provider on failure | Independent implementation QA GO |
| 8. Release and deploy | ARK, Cloud, Desktop | Release the ARK contract first. Implement and check the Cloud consumer against that tag. The owner separately authorizes broker deployment. After healthy production checks, Desktop pins the immutable ARK tag and is released only when asked | No deployment, pin, push or release is inferred from implementation approval |
| 9. Dogfood | ARK Desktop and Cloud | After the owner-approved deployment and Desktop pin, the owner connects two low-risk accounts for one toolkit, chooses one explicitly, completes one read, approves one mutation, revokes it, exhausts a test quota and repeats after provider failure | The selected account alone changes. Real logs contain metadata only. No credential or payload appears |

The first release imports the broad catalog but labels only dogfooded integrations as reviewed.
Everything else remains available with provider provenance and the conservative `Mutate` risk.
Reassess the default provider when managed calls reach half of the relevant free allowance, ARK has
recurring revenue, a qualified organization rejects the retention boundary, or provider terms
change materially.
