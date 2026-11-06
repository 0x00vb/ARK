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

/// One stretch of speech in a transcript, timed from the start of the audio it was read from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Segment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[async_trait]
pub trait TranscriptionProvider: Send + Sync {
    fn state(&self) -> State;
    async fn prepare(&self) -> anyhow::Result<()>;
    /// 16 kHz, mono, signed 16-bit PCM. Empty language means automatic detection.
    async fn transcribe(&self, pcm: Vec<i16>, language: Option<String>) -> anyhow::Result<String>;
    /// The same audio as timed stretches of speech, so a live preview can settle the early ones
    /// and read only the rest again. A provider without timings answers one stretch for the
    /// whole audio, which is never settled.
    async fn segments(
        &self,
        pcm: Vec<i16>,
        language: Option<String>,
    ) -> anyhow::Result<Vec<Segment>> {
        let end_ms = pcm.len() as u64 * 1000 / 16_000;
        let text = self.transcribe(pcm, language).await?;
        Ok(vec![Segment {
            start_ms: 0,
            end_ms,
            text,
        }])
    }
}
