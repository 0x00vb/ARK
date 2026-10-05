//! # ark-connect
//!
//! Capabilities that need an account, the way a person sees them: Mail, Calendar, later
//! GitHub and others. ARK owns the connection: it signs in to the provider with its own
//! client ([`oauth`]), keeps the tokens in the OS keychain ([`secrets`]), knows which scopes
//! each capability needs ([`catalog`]), revokes on disconnect, and gives the connected
//! capabilities' tools to turns as an [`ark_tools::ToolSource`] ([`Connections`]).
//!
//! Service names live only here and in `catalog.json`. A capability's tools have neutral
//! names (`mail.send`), so another provider can back the same capability. Why: ADR 0011.

pub mod api_keys;
pub mod catalog;
pub mod oauth;
pub mod secrets;
pub mod services;

mod acquire;
mod connections;

pub use api_keys::ApiKeys;
pub use catalog::{Capability, Catalog, OAuth, Provider};
pub use connections::{Account, CapabilityStatus, Connecting, Connections, State};
pub use secrets::{Keychain, MemorySecrets, Secrets};

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("{0}")]
    NotSetUp(String),
    #[error("{0}")]
    Cancelled(String),
    #[error("{0}")]
    Provider(String),
    #[error("{0}")]
    NotConnected(String),
    #[error("keychain: {0}")]
    Secrets(String),
    #[error("unknown capability `{0}`")]
    Unknown(String),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
