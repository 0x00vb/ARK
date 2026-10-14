//! The provider boundary for marketplace integrations.
//!
//! Runtime code deals only in ARK ids and opaque references. An adapter may live in a consumer,
//! a Cloud broker client, or a person-owned remote endpoint. It must never expose a vendor key,
//! session header, or external account id to the catalog or UI layers.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;

use crate::cancel::CancelToken;

use super::catalog::{
    CapabilityOperationId, PluginRef, ProviderBindingRef, ProviderInvocationId, ProviderOffer,
    ProviderOfferId, ProviderTenantRef, ProviderTurnRef, ReturnDestinationId,
};

#[async_trait::async_trait]
pub trait IntegrationProvider: Send + Sync {
    async fn catalog(&self, request: ProviderCatalogRequest)
        -> ProviderResult<ProviderCatalogPage>;
    async fn begin_connect(&self, request: ConnectRequest) -> ProviderResult<ConnectStart>;
    async fn connection(&self, request: ConnectionRequest) -> ProviderResult<ConnectionStatus>;
    async fn disconnect(&self, request: DisconnectRequest) -> ProviderResult<DisconnectResult>;
    async fn bind_turn(&self, request: BindTurnRequest) -> ProviderResult<BindTurnResult>;
    async fn execute(&self, request: ExecuteRequest) -> ProviderResult<ProviderExecutionOutcome>;
    async fn release_turn(&self, request: ReleaseTurnRequest) -> ProviderResult<ReleaseTurnResult>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCatalogRequest {
    pub cursor: Option<String>,
    pub limit: u16,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderCatalogPage {
    pub offers: Vec<ProviderOffer>,
    pub next: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectRequest {
    pub tenant: ProviderTenantRef,
    pub plugin: PluginRef,
    pub offer: ProviderOfferId,
    pub return_destination: ReturnDestinationId,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectStart {
    pub operation: String,
    pub authorization_url: String,
    pub expires_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionRequest {
    pub tenant: ProviderTenantRef,
    pub binding: ProviderBindingRef,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionStatus {
    pub binding: ProviderBindingRef,
    pub account_label: String,
    pub state: ConnectionState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConnectionState {
    Missing,
    Connecting,
    Connected,
    Failed { why: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisconnectRequest {
    pub tenant: ProviderTenantRef,
    pub binding: ProviderBindingRef,
    pub revoke_upstream: bool,
    pub idempotency_key: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisconnectResult {
    Revoked,
    DisabledRevocationPending,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindTurnRequest {
    pub tenant: ProviderTenantRef,
    /// A fresh, random 256-bit nonce stored with the local durable turn.
    pub client_nonce: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindTurnResult {
    pub turn: ProviderTurnRef,
}

#[derive(Clone)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderExecution {
    pub provider_request: String,
    pub operation_schema_digest: String,
    pub result: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseTurnRequest {
    pub tenant: ProviderTenantRef,
    pub turn: ProviderTurnRef,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseTurnResult {
    Released,
    AlreadyReleased,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderErrorKind {
    Invalid,
    Refused,
    NotConnected,
    Quota,
    RateLimited,
    Timeout,
    Cancelled,
    ProviderUnavailable,
    IncompatibleSchema,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderError {
    pub kind: ProviderErrorKind,
    pub safe_detail: String,
    pub retry_at: Option<OffsetDateTime>,
}

pub type ProviderResult<T> = Result<T, ProviderError>;
