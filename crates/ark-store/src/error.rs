use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),

    #[error(transparent)]
    Core(#[from] ark_core::CoreError),

    /// Another process runs on this store (ADR 0062). `pid` is unknown where the platform
    /// does not let a locked file be read.
    #[error("another ARK runtime{} is using the store at {}; connect to it instead of starting a second one", .pid.map(|p| format!(" (pid {p})")).unwrap_or_default(), .root.display())]
    Busy {
        root: std::path::PathBuf,
        pid: Option<u32>,
    },
}
