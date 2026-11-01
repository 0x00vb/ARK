use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("a project with that name already exists")]
    ProjectNameConflict,

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
    #[error("another ARK runtime{} is using the store at {}; stop the other writer before opening it", .pid.map(|p| format!(" (pid {p})")).unwrap_or_default(), .root.display())]
    Busy {
        root: std::path::PathBuf,
        pid: Option<u32>,
    },
}
