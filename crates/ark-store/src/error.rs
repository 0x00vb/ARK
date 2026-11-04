use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("a project with that name already exists")]
    ProjectNameConflict,

    #[error("memory: {0}")]
    Memory(String),

    #[error("record exceeds its read limit")]
    RecordTooLarge,

    #[error("immutable record conflicts with existing data")]
    ImmutableConflict,

    #[error("record mutation clock is exhausted")]
    RecordEpochExhausted,

    #[error("project storage is busy; try again")]
    ProjectBusy,

    #[error("project location changed; reload and try again")]
    ProjectLocationChanged,

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
