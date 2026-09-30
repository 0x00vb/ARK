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

    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
}
