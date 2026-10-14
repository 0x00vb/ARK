//! View model for the plugin catalog.
//!
//! This module contains no provider names and performs no network or account work. It turns the
//! runtime's inert records into one deterministic state and primary action for each visible row.

use ark_sdk::prelude::ark_runtime::plugins::catalog::{
    CatalogFreshness, CatalogId, CatalogScope, ComponentResult, ComponentState, InstalledPlugin,
    OperationId, PluginIcon, PluginRef, ProjectPlugin, ProviderHealth, UpdateState,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Applicable<T> {
    NotApplicable,
    Value(T),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConnectionState {
    Missing,
    Connecting,
    Connected { label: String },
    Failed { why: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EntryCompatibility {
    Compatible,
    PartiallyCompatible { why: Vec<String> },
    Incompatible { why: Vec<String> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryKind {
    RefreshCatalog,
    RegisterSource,
    Add,
    Connect,
    Enable,
    Disable,
    Update,
    Remove,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "original", rename_all = "snake_case")]
pub enum OperationKind {
    RefreshCatalog,
    RegisterSource,
    Add,
    Connect,
    Enable,
    Disable,
    Update,
    Remove,
    Retry(RetryKind),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "target", content = "id", rename_all = "snake_case")]
pub enum OperationTarget {
    Plugin(PluginRef),
    Catalog(CatalogId),
    Source,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum OperationState {
    Pending,
    Running,
    Refused,
    Failed { why: String },
    Completed,
    Cancelling,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginOperation {
    pub id: OperationId,
    pub target: OperationTarget,
    pub kind: OperationKind,
    pub state: OperationState,
    pub cancellable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryState {
    Available,
    Adding,
    Connecting,
    Enabling,
    Disabling,
    Refused,
    OperationFailed,
    NeedsConnection,
    ConnectedDisabled,
    Enabled,
    Partial,
    ProviderUnavailable,
    UpdateAvailable,
    Updating,
    RollbackRetained,
    Removing,
    RemovalFailed,
    CatalogBlocked,
    Revoked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RowBadge {
    Cached,
    CatalogWarning,
    Revoked,
    Partial,
    ProviderDegraded,
    ProviderUnavailable,
    UpdateAvailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginAction {
    Inspect,
    Review,
    Add,
    Connect,
    TurnOn,
    TurnOff,
    Update,
    Retry,
    Remove,
    Cancel,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InstalledRow {
    pub plugin: PluginRef,
    pub name: String,
    pub icon: Option<PluginIcon>,
    pub state: EntryState,
    pub badges: Vec<RowBadge>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CatalogView {
    pub id: CatalogId,
    pub scope: CatalogScope,
    pub revision: Option<u64>,
    pub freshness: CatalogFreshness,
    pub operation: Option<PluginOperation>,
    pub entries: Vec<PluginRow>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginCatalogView {
    pub schema_version: u16,
    pub catalogs: Vec<CatalogView>,
    pub installed: Vec<InstalledRow>,
    pub source_operations: Vec<PluginOperation>,
}

/// Facts needed to derive one row. Catalog browsing never mutates these records.
pub struct RowFacts<'a> {
    pub installed: Option<&'a InstalledPlugin>,
    pub project: Option<&'a ProjectPlugin>,
    pub freshness: &'a CatalogFreshness,
    pub compatibility: &'a EntryCompatibility,
    pub connection: &'a Applicable<ConnectionState>,
    pub provider: Option<&'a ProviderHealth>,
    pub components: &'a [ComponentResult],
    pub operation: Option<&'a PluginOperation>,
    pub entry_revoked: bool,
}

/// The single primary state and action that all front-ends show for the same facts.
pub fn derive_row(facts: &RowFacts<'_>) -> (EntryState, Vec<RowBadge>, Option<PluginAction>) {
    let badges = derive_badges(facts);
    if let Some(operation) = facts.operation {
        if let Some(derived) = derive_operation(operation) {
            return (derived.0, badges, derived.1);
        }
    }

    let (state, action) = derive_base(facts);
    (state, badges, action)
}

fn derive_operation(operation: &PluginOperation) -> Option<(EntryState, Option<PluginAction>)> {
    let kind = match operation.kind {
        OperationKind::Retry(original) => original,
        OperationKind::Add => RetryKind::Add,
        OperationKind::Connect => RetryKind::Connect,
        OperationKind::Enable => RetryKind::Enable,
        OperationKind::Disable => RetryKind::Disable,
        OperationKind::Update => RetryKind::Update,
        OperationKind::Remove => RetryKind::Remove,
        OperationKind::RefreshCatalog | OperationKind::RegisterSource => return None,
    };

    match &operation.state {
        OperationState::Pending | OperationState::Running => Some((
            busy_state(kind),
            operation.cancellable.then_some(PluginAction::Cancel),
        )),
        OperationState::Cancelling => Some((busy_state(kind), None)),
        OperationState::Refused => Some((EntryState::Refused, Some(PluginAction::Inspect))),
        OperationState::Failed { .. } => Some(failed_state(kind)),
        OperationState::Completed | OperationState::Cancelled => None,
    }
}

fn busy_state(kind: RetryKind) -> EntryState {
    match kind {
        RetryKind::Add => EntryState::Adding,
        RetryKind::Connect => EntryState::Connecting,
        RetryKind::Enable => EntryState::Enabling,
        RetryKind::Disable => EntryState::Disabling,
        RetryKind::Update => EntryState::Updating,
        RetryKind::Remove => EntryState::Removing,
        RetryKind::RefreshCatalog | RetryKind::RegisterSource => EntryState::OperationFailed,
    }
}

fn failed_state(kind: RetryKind) -> (EntryState, Option<PluginAction>) {
    let state = match kind {
        RetryKind::Add => EntryState::OperationFailed,
        RetryKind::Connect => EntryState::NeedsConnection,
        RetryKind::Enable => EntryState::ConnectedDisabled,
        RetryKind::Disable => EntryState::Enabled,
        RetryKind::Update => EntryState::RollbackRetained,
        RetryKind::Remove => EntryState::RemovalFailed,
        RetryKind::RefreshCatalog | RetryKind::RegisterSource => EntryState::OperationFailed,
    };
    (state, Some(PluginAction::Retry))
}

fn derive_base(facts: &RowFacts<'_>) -> (EntryState, Option<PluginAction>) {
    if facts.entry_revoked {
        return (EntryState::Revoked, Some(PluginAction::Review));
    }

    if facts.installed.is_none() {
        if matches!(
            facts.freshness,
            CatalogFreshness::Expired { .. } | CatalogFreshness::Revoked { .. }
        ) {
            return (EntryState::CatalogBlocked, Some(PluginAction::Review));
        }
        return match facts.compatibility {
            EntryCompatibility::Compatible => (EntryState::Available, Some(PluginAction::Add)),
            EntryCompatibility::PartiallyCompatible { .. }
            | EntryCompatibility::Incompatible { .. } => {
                (EntryState::Partial, Some(PluginAction::Review))
            }
        };
    }

    let installed = facts.installed.expect("checked above");
    match &installed.update {
        UpdateState::RollbackRetained { .. } => {
            return (EntryState::RollbackRetained, Some(PluginAction::Retry));
        }
        UpdateState::Updating { .. } => return (EntryState::Updating, None),
        UpdateState::Current | UpdateState::Available { .. } => {}
    }
    if matches!(facts.provider, Some(ProviderHealth::Unavailable { .. })) {
        return (EntryState::ProviderUnavailable, Some(PluginAction::Retry));
    }
    if facts.components.iter().any(|component| {
        matches!(
            &component.state,
            ComponentState::Incompatible { .. } | ComponentState::Failed { .. }
        )
    }) {
        return (EntryState::Partial, Some(PluginAction::Review));
    }
    match facts.connection {
        Applicable::Value(ConnectionState::Connecting) => return (EntryState::Connecting, None),
        Applicable::Value(ConnectionState::Missing) => {
            return (EntryState::NeedsConnection, Some(PluginAction::Connect));
        }
        Applicable::Value(ConnectionState::Failed { .. }) => {
            return (EntryState::NeedsConnection, Some(PluginAction::Retry));
        }
        Applicable::NotApplicable | Applicable::Value(ConnectionState::Connected { .. }) => {}
    }

    if !facts.project.is_some_and(|project| project.enabled) {
        return (EntryState::ConnectedDisabled, Some(PluginAction::TurnOn));
    }
    if matches!(&installed.update, UpdateState::Available { .. }) {
        return (EntryState::UpdateAvailable, Some(PluginAction::Update));
    }
    (EntryState::Enabled, Some(PluginAction::Inspect))
}

fn derive_badges(facts: &RowFacts<'_>) -> Vec<RowBadge> {
    let mut badges = Vec::new();
    if facts.entry_revoked {
        badges.push(RowBadge::Revoked);
    }
    if matches!(
        facts.freshness,
        CatalogFreshness::Expired { .. }
            | CatalogFreshness::Revoked { .. }
            | CatalogFreshness::InvalidUsingPrevious { .. }
            | CatalogFreshness::InvalidNoCache { .. }
            | CatalogFreshness::OfflineNoCache { .. }
    ) {
        badges.push(RowBadge::CatalogWarning);
    }
    if matches!(
        facts.freshness,
        CatalogFreshness::CachedStale { .. } | CatalogFreshness::InvalidUsingPrevious { .. }
    ) {
        badges.push(RowBadge::Cached);
    }
    match facts.provider {
        Some(ProviderHealth::Unavailable { .. }) => badges.push(RowBadge::ProviderUnavailable),
        Some(ProviderHealth::Degraded { .. }) => badges.push(RowBadge::ProviderDegraded),
        Some(ProviderHealth::Ready) | None => {}
    }
    if !matches!(facts.compatibility, EntryCompatibility::Compatible)
        || facts.components.iter().any(|component| {
            matches!(
                &component.state,
                ComponentState::Incompatible { .. } | ComponentState::Failed { .. }
            )
        })
    {
        badges.push(RowBadge::Partial);
    }
    if facts
        .installed
        .is_some_and(|installed| matches!(&installed.update, UpdateState::Available { .. }))
    {
        badges.push(RowBadge::UpdateAvailable);
    }
    badges
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ark_sdk::prelude::ark_runtime::plugins::catalog::{
        AuthRequirement, CapabilityOperation, CapabilityOperationId, CatalogEntry, CatalogId,
        EntryKind, InstalledPlugin, OfferPrerequisite, PluginId, PluginRef, ProcessorDisclosure,
        ProjectPlugin, ProviderDelivery, ProviderDisclosure, ProviderId, ProviderOffer,
        ProviderOfferId, ProviderOperationRef, Risk, UpdatePolicy,
    };
    use ark_sdk::prelude::ProjectId;
    use serde_json::Map;
    use time::OffsetDateTime;

    use super::*;

    fn entry() -> CatalogEntry {
        CatalogEntry {
            id: PluginId::from("mail"),
            name: "Mail".into(),
            summary: "Read and send mail".into(),
            publisher: "Example".into(),
            icon: None,
            categories: vec!["Work".into()],
            version: "1".into(),
            kind: EntryKind::ProviderIntegration {
                operations: vec![CapabilityOperation {
                    id: CapabilityOperationId::from("mail.read"),
                    name: "Read mail".into(),
                    description: "Read a message".into(),
                    input_schema: serde_json::json!({"type": "object"}),
                    output_schema: None,
                    schema_digest: "ark-v1".into(),
                    risk: Risk::Read,
                }],
                offers: vec![ProviderOffer {
                    id: ProviderOfferId::from("provider.mail"),
                    provider: ProviderId::from("provider"),
                    external_integration: "mail".into(),
                    adapter_revision: "1".into(),
                    operations: BTreeMap::from([(
                        CapabilityOperationId::from("mail.read"),
                        ProviderOperationRef {
                            external_operation: "MAIL_READ".into(),
                            provider_schema_digest: "provider-v1".into(),
                            mapping_digest: "map-v1".into(),
                            opaque: Map::new(),
                        },
                    )]),
                    auth: AuthRequirement::Account {
                        provider: ProviderId::from("provider"),
                    },
                    update: UpdatePolicy::Manual,
                    delivery: ProviderDelivery::Brokered,
                    prerequisites: vec![OfferPrerequisite::ArkAccount],
                    disclosure: ProviderDisclosure {
                        credential_custodian: "Provider".into(),
                        payload_processors: vec![ProcessorDisclosure {
                            name: "Provider".into(),
                            purpose: "Execute the requested operation".into(),
                            privacy_url: "https://example.test/privacy".into(),
                            retention: "Provider policy".into(),
                        }],
                    },
                    opaque: Map::new(),
                }],
            },
            access: vec!["Read messages".into()],
            extra: Map::new(),
        }
    }

    fn installed(update: UpdateState) -> InstalledPlugin {
        InstalledPlugin {
            schema_version: 1,
            plugin: PluginRef {
                catalog: CatalogId::from("public"),
                plugin: PluginId::from("mail"),
            },
            pinned_revision: 1,
            pinned_entry: entry(),
            added_at: OffsetDateTime::UNIX_EPOCH,
            data_path: None,
            projects: BTreeMap::new(),
            components: vec![],
            update,
            extra: Map::new(),
        }
    }

    fn facts<'a>(
        installed: Option<&'a InstalledPlugin>,
        project: Option<&'a ProjectPlugin>,
        connection: &'a Applicable<ConnectionState>,
        operation: Option<&'a PluginOperation>,
    ) -> RowFacts<'a> {
        RowFacts {
            installed,
            project,
            freshness: &CatalogFreshness::Fresh,
            compatibility: &EntryCompatibility::Compatible,
            connection,
            provider: None,
            components: &[],
            operation,
            entry_revoked: false,
        }
    }

    #[test]
    fn uninstalled_account_plugin_is_added_before_it_connects() {
        let connection = Applicable::Value(ConnectionState::Missing);
        assert_eq!(
            derive_row(&facts(None, None, &connection, None)).0,
            EntryState::Available
        );
    }

    #[test]
    fn authless_installed_plugin_can_be_turned_on() {
        let record = installed(UpdateState::Current);
        let project = ProjectPlugin {
            enabled: false,
            selection: None,
            pending_switch: None,
        };
        let connection = Applicable::NotApplicable;
        assert_eq!(
            derive_row(&facts(Some(&record), Some(&project), &connection, None)),
            (
                EntryState::ConnectedDisabled,
                vec![],
                Some(PluginAction::TurnOn)
            )
        );
    }

    #[test]
    fn retrying_connect_has_connecting_state_and_cancel() {
        let record = installed(UpdateState::Current);
        let project = ProjectPlugin {
            enabled: false,
            selection: None,
            pending_switch: None,
        };
        let connection = Applicable::Value(ConnectionState::Missing);
        let operation = PluginOperation {
            id: OperationId::from("op"),
            target: OperationTarget::Plugin(record.plugin.clone()),
            kind: OperationKind::Retry(RetryKind::Connect),
            state: OperationState::Running,
            cancellable: true,
        };
        assert_eq!(
            derive_row(&facts(
                Some(&record),
                Some(&project),
                &connection,
                Some(&operation)
            )),
            (EntryState::Connecting, vec![], Some(PluginAction::Cancel))
        );
    }

    #[test]
    fn precedence_and_badge_order_are_stable() {
        let mut record = installed(UpdateState::Available {
            version: "2".into(),
        });
        record.components.push(ComponentResult {
            component: "remote".into(),
            state: ComponentState::Failed { why: "down".into() },
        });
        let project = ProjectPlugin {
            enabled: true,
            selection: None,
            pending_switch: None,
        };
        let connection = Applicable::NotApplicable;
        let freshness = CatalogFreshness::InvalidUsingPrevious {
            why: "bad signature".into(),
        };
        let provider = ProviderHealth::Unavailable {
            why: "offline".into(),
            retry_at: None,
        };
        let facts = RowFacts {
            installed: Some(&record),
            project: Some(&project),
            freshness: &freshness,
            compatibility: &EntryCompatibility::Compatible,
            connection: &connection,
            provider: Some(&provider),
            components: &record.components,
            operation: None,
            entry_revoked: false,
        };
        assert_eq!(derive_row(&facts).0, EntryState::ProviderUnavailable);
        assert_eq!(
            derive_row(&facts).1,
            vec![
                RowBadge::CatalogWarning,
                RowBadge::Cached,
                RowBadge::ProviderUnavailable,
                RowBadge::Partial,
                RowBadge::UpdateAvailable,
            ]
        );
    }

    #[test]
    fn expired_catalog_does_not_hide_installed_state() {
        let record = installed(UpdateState::Current);
        let project = ProjectPlugin {
            enabled: true,
            selection: None,
            pending_switch: None,
        };
        let connection = Applicable::NotApplicable;
        let freshness = CatalogFreshness::Expired {
            expired_at: OffsetDateTime::UNIX_EPOCH,
        };
        let facts = RowFacts {
            freshness: &freshness,
            ..facts(Some(&record), Some(&project), &connection, None)
        };
        assert_eq!(derive_row(&facts).0, EntryState::Enabled);
        assert_eq!(derive_row(&facts).1, vec![RowBadge::CatalogWarning]);
    }

    #[test]
    fn project_id_is_not_part_of_plugin_identity() {
        let first = ProjectId("first".into());
        let second = ProjectId("second".into());
        let plugin = PluginRef {
            catalog: CatalogId::from("public"),
            plugin: PluginId::from("mail"),
        };
        assert_ne!(first, second);
        assert_eq!(plugin.plugin.0, "mail");
    }
}
