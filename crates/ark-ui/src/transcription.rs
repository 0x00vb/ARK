//! Dictation's provider boundary. The page owns microphone capture; an application may inject a
//! transcriber at the selected runtime. `ark-ui` deliberately ships no engine (ADR 0070).

use async_trait::async_trait;
use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    NotInstalled,
    Downloading,
    Ready,
    Failed,
}

#[derive(Clone, Debug, Serialize)]
pub struct State {
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub downloaded: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
}

impl State {
    pub fn new(status: Status) -> Self {
        Self {
            status,
            downloaded: None,
            total: None,
        }
    }
}

#[async_trait]
pub trait TranscriptionProvider: Send + Sync {
    fn state(&self) -> State;
    async fn prepare(&self) -> anyhow::Result<()>;
    /// 16 kHz, mono, signed 16-bit PCM. Empty language means automatic detection.
    async fn transcribe(&self, pcm: Vec<i16>, language: Option<String>) -> anyhow::Result<String>;
}
