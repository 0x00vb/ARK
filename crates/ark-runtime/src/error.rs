use thiserror::Error;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("unknown action: {0}")]
    UnknownAction(String),

    #[error("project not found: {0}")]
    ProjectNotFound(String),

    #[error("artifact not found: {0}")]
    ArtifactNotFound(String),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("store: {0}")]
    Store(#[from] ark_store::StoreError),

    #[error("adapter: {0}")]
    Adapter(#[from] ark_tools::AdapterError),

    #[error("provider: {0}")]
    Provider(String),

    /// The model's account has no credit left (the provider answered 402). `message` is the
    /// provider's own words. The turn stops; it can go on once credit is added or with another
    /// model.
    #[error("{message}")]
    OutOfCredit { provider: String, message: String },

    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),

    /// The model's own reply could not be used (arguments that do not parse, a reply that is
    /// not structured or was cut off), and asking it to fix its reply did not help (ADR 0050).
    /// The turn keeps its goal and calls; the next one continues it.
    #[error("{UNUSABLE_REPLY}: {detail}")]
    UnusableReply { provider: String, detail: String },

    /// A plugin could not be added, removed or started.
    #[error("{0}")]
    Plugin(String),

    #[error("the execution mode changed in another window")]
    StaleExecutionMode,
}

/// How an [`AgentError::UnusableReply`] reads, so a front-end can word it for the person.
pub const UNUSABLE_REPLY: &str = "the model's reply could not be used";

impl AgentError {
    /// A model call's failure: [`AgentError::OutOfCredit`] when the account is out of credit,
    /// else [`AgentError::Provider`].
    pub fn model(e: &ark_model::ProviderError) -> Self {
        match e {
            ark_model::ProviderError::OutOfCredit { provider, detail } => AgentError::OutOfCredit {
                provider: provider.clone(),
                message: detail.clone(),
            },
            ark_model::ProviderError::Malformed { provider, detail } => AgentError::UnusableReply {
                provider: provider.clone(),
                detail: detail.clone(),
            },
            other => AgentError::Provider(other.to_string()),
        }
    }
}
