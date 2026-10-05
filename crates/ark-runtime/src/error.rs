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

    /// A plugin could not be added, removed or started.
    #[error("{0}")]
    Plugin(String),
}

impl AgentError {
    /// A model call's failure: [`AgentError::OutOfCredit`] when the account is out of credit,
    /// else [`AgentError::Provider`].
    pub fn model(e: &ark_model::ProviderError) -> Self {
        match e {
            ark_model::ProviderError::OutOfCredit { provider, detail } => AgentError::OutOfCredit {
                provider: provider.clone(),
                message: detail.clone(),
            },
            other => AgentError::Provider(other.to_string()),
        }
    }
}
