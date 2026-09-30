use thiserror::Error;

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("unknown action: {0}")]
    UnknownAction(String),

    #[error("invalid input for `{action}`: {detail}")]
    BadInput { action: String, detail: String },

    #[error("artifact not found: {0}")]
    ArtifactNotFound(String),

    #[error("tool `{tool}` unavailable: {detail}")]
    ToolUnavailable { tool: String, detail: String },

    #[error("tool `{tool}` failed (exit {code}): {stderr}")]
    ToolFailed {
        tool: String,
        code: i32,
        stderr: String,
    },

    #[error("parse error: {0}")]
    Parse(String),

    #[error("tool error: {0}")]
    Tool(String),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
}

impl AdapterError {
    pub fn bad_input(action: &str, detail: impl Into<String>) -> Self {
        AdapterError::BadInput {
            action: action.into(),
            detail: detail.into(),
        }
    }
}
