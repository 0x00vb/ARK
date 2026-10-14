//! Provider-neutral plugin catalog and installation records.
//!
//! Catalog data is inert. Accepting a signed snapshot or persisting an installation does not
//! start code, connect an account, enable a capability, or approve an invocation.

use std::collections::BTreeMap;
use std::path::PathBuf;

use ark_core::ProjectId;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use time::OffsetDateTime;

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }
    };
}

string_id!(PluginId);
string_id!(CatalogId);
string_id!(ProviderId);
string_id!(ConnectionRef);
string_id!(ProviderBoundaryRef);
string_id!(ProviderOfferId);
string_id!(ProviderBindingRef);
string_id!(ProviderTenantRef);
string_id!(CapabilityOperationId);
string_id!(ProviderInvocationId);
string_id!(ProviderTurnRef);
string_id!(ReturnDestinationId);
string_id!(OperationId);

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PluginRef {
    pub catalog: CatalogId,
    pub plugin: PluginId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogScope {
    Public,
    Personal,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CatalogKey {
    pub id: String,
    pub public_key: String,
    pub valid_from: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PluginIcon {
    BuiltIn {
        name: String,
    },
    Embedded {
        media_type: IconMediaType,
        digest: String,
        data_base64: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IconMediaType {
    Png,
    Webp,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CatalogSnapshot {
    pub schema_version: u16,
    pub catalog: CatalogId,
    pub scope: CatalogScope,
    pub revision: u64,
    pub issued_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
    pub key_id: String,
    pub entries: Vec<CatalogEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub revoked_entries: Vec<PluginId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_keys: Vec<CatalogKey>,
    #[serde(default, flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub id: PluginId,
    pub name: String,
    pub summary: String,
    pub publisher: String,
    pub icon: Option<PluginIcon>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<String>,
    pub version: String,
    #[serde(flatten)]
    pub kind: EntryKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub access: Vec<String>,
    #[serde(default, flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
        #[serde(default)]
        operations: Vec<CapabilityOperation>,
        #[serde(default)]
        offers: Vec<ProviderOffer>,
    },
}

/// A stable, ARK-owned operation. Provider adapters map it to their own operation at runtime.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CapabilityOperation {
    pub id: CapabilityOperationId,
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub output_schema: Option<Value>,
    pub schema_digest: String,
    pub risk: Risk,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    Read,
    Mutate,
}

/// One provider's implementation of an ARK catalog entry. The public catalog may offer several
/// alternatives without changing the integration's ARK identity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderOffer {
    pub id: ProviderOfferId,
    pub provider: ProviderId,
    pub external_integration: String,
    pub adapter_revision: String,
    pub operations: BTreeMap<CapabilityOperationId, ProviderOperationRef>,
    pub auth: AuthRequirement,
    pub update: UpdatePolicy,
    pub delivery: ProviderDelivery,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prerequisites: Vec<OfferPrerequisite>,
    pub disclosure: ProviderDisclosure,
    #[serde(default)]
    pub opaque: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderOperationRef {
    pub external_operation: String,
    pub provider_schema_digest: String,
    pub mapping_digest: String,
    #[serde(default)]
    pub opaque: Map<String, Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderDelivery {
    Brokered,
    Direct,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OfferPrerequisite {
    ArkAccount,
    LocalSecretStore,
    ReachableEndpoint,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderDisclosure {
    pub credential_custodian: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub payload_processors: Vec<ProcessorDisclosure>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessorDisclosure {
    pub name: String,
    pub purpose: String,
    pub privacy_url: String,
    pub retention: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PackageComponent {
    Skill { name: String, digest: String },
    LocalServer { namespace: String },
    RemoteServer { namespace: String, origin: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthRequirement {
    None,
    Account { provider: ProviderId },
    Secret { fields: Vec<SecretField> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretField {
    pub id: String,
    pub label: String,
    pub required: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdatePolicy {
    Manual,
    Pinned,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InstalledPlugin {
    pub schema_version: u16,
    pub plugin: PluginRef,
    pub pinned_revision: u64,
    pub pinned_entry: CatalogEntry,
    pub added_at: OffsetDateTime,
    pub data_path: Option<PathBuf>,
    #[serde(default)]
    pub projects: BTreeMap<ProjectId, ProjectPlugin>,
    #[serde(default)]
    pub components: Vec<ComponentResult>,
    pub update: UpdateState,
    #[serde(default, flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectPlugin {
    pub enabled: bool,
    #[serde(default)]
    pub selection: Option<ProviderSelection>,
    #[serde(default)]
    pub pending_switch: Option<ProviderSwitch>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderSelection {
    pub offer: ProviderOfferId,
    pub binding: ProviderBindingRef,
    pub operation_schema_digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderSwitch {
    pub from: ProviderSelection,
    pub to_offer: ProviderOfferId,
    pub candidate: Option<ProviderBindingRef>,
    pub state: ProviderSwitchState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProviderSwitchState {
    Connecting,
    CheckingCompatibility,
    Ready,
    Failed { why: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentResult {
    pub component: String,
    pub state: ComponentState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ComponentState {
    Ready,
    NeedsConnection,
    Incompatible { why: String },
    Failed { why: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum UpdateState {
    Current,
    Available { version: String },
    Updating { operation: OperationId },
    RollbackRetained { why: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProviderHealth {
    Ready,
    Degraded {
        why: String,
    },
    Unavailable {
        why: String,
        retry_at: Option<OffsetDateTime>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CatalogFreshness {
    Fresh,
    CachedStale {
        fetched_at: OffsetDateTime,
        why: String,
    },
    OfflineNoCache {
        why: String,
    },
    InvalidUsingPrevious {
        why: String,
    },
    InvalidNoCache {
        why: String,
    },
    Expired {
        expired_at: OffsetDateTime,
    },
    Revoked {
        why: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_metadata_and_unknown_fields_round_trip() {
        let input = serde_json::json!({
            "id": "mail",
            "name": "Mail",
            "summary": "Read and send mail",
            "publisher": "Example",
            "icon": null,
            "categories": ["work"],
            "version": "1",
            "kind": "provider_integration",
            "operations": [{
                "id": "mail.read", "name": "Read mail", "description": "Read a message",
                "input_schema": {"type": "object"}, "output_schema": null,
                "schema_digest": "ark-v1", "risk": "read"
            }],
            "offers": [{
                "id": "integration-host.mail", "provider": "integration-host",
                "external_integration": "mail", "adapter_revision": "42",
                "operations": {"mail.read": {"external_operation": "MAIL_READ", "provider_schema_digest": "host-v1", "mapping_digest": "map-v1", "opaque": {"piece": "mail"}}},
                "auth": {"kind": "account", "provider": "mail"}, "update": "manual",
                "delivery": "brokered", "prerequisites": ["ark_account"],
                "disclosure": {"credential_custodian": "Integration host", "payload_processors": []},
                "opaque": {"action_count": 12}
            }],
            "access": ["Read messages"],
            "future_field": {"kept": true}
        });

        let entry: CatalogEntry = serde_json::from_value(input).unwrap();
        let output = serde_json::to_value(entry).unwrap();
        assert_eq!(output["offers"][0]["opaque"]["action_count"], 12);
        assert_eq!(
            output["offers"][0]["operations"]["mail.read"]["opaque"]["piece"],
            "mail"
        );
        assert_eq!(output["future_field"]["kept"], true);
    }

    #[test]
    fn selection_pins_an_offer_without_changing_ark_identity() {
        let selection = ProviderSelection {
            offer: ProviderOfferId::from("provider-a.mail"),
            binding: ProviderBindingRef::from("binding-1"),
            operation_schema_digest: "ark-v1".into(),
        };
        let switched = ProviderSwitch {
            from: selection.clone(),
            to_offer: ProviderOfferId::from("provider-b.mail"),
            candidate: None,
            state: ProviderSwitchState::Connecting,
        };
        assert_eq!(switched.from.offer.0, "provider-a.mail");
        assert_eq!(switched.to_offer.0, "provider-b.mail");
    }

    #[test]
    fn catalog_is_part_of_plugin_identity() {
        let public = PluginRef {
            catalog: CatalogId::from("public"),
            plugin: PluginId::from("mail"),
        };
        let personal = PluginRef {
            catalog: CatalogId::from("personal"),
            plugin: PluginId::from("mail"),
        };
        assert_ne!(public, personal);
    }
}
