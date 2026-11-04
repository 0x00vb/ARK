use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("the conversation changed; refresh it and try again")]
    Conflict,
    #[error("memory: {0}")]
    Memory(String),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),

    #[error(transparent)]
    Core(#[from] ark_core::CoreError),
}
