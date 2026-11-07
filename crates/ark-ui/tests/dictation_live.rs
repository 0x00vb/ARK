//! A recording shows its words while it goes on, and each preview reads only what is not settled.
use std::sync::{Arc, Mutex};

use ark_interact::App;
use ark_sdk::prelude::*;
use ark_ui::access::Tokens;
use ark_ui::transcription::{Segment, State, Status, TranscriptionProvider};
use ark_ui::{Access, ArkUi};
use base64::Engine as _;
use serde_json::{json, Value};

/// Audio is runs of one repeated value. A run's words are `block<value>`; zero is silence.
struct Blocks {
    reads: Mutex<Vec<usize>>,
}

#[async_trait::async_trait]
impl TranscriptionProvider for Blocks {
    fn state(&self) -> State {
        State::new(Status::Ready)
    }
    async fn prepare(&self) -> anyhow::Result<()> {
        Ok(())
    }
    async fn transcribe(&self, pcm: Vec<i16>, _: Option<String>) -> anyhow::Result<String> {
        self.reads.lock().unwrap().push(pcm.len());
        Ok(self
            .segments(pcm, None)
            .await?
            .iter()
            .map(|s| s.text.clone())
            .collect::<Vec<_>>()
            .join(" "))
    }
    async fn segments(&self, pcm: Vec<i16>, _: Option<String>) -> anyhow::Result<Vec<Segment>> {
        self.reads.lock().unwrap().push(pcm.len());
        let mut segments = Vec::new();
        let mut start = 0;
        while start < pcm.len() {
            let value = pcm[start];
            let end = start + pcm[start..].iter().take_while(|v| **v == value).count();
            if value != 0 {
                segments.push(Segment {
                    start_ms: start as u64 / 16,
                    end_ms: end as u64 / 16,
                    text: format!("block{value}"),
                });
            }
            start = end;
        }
        Ok(segments)
    }
}

async fn rpc(url: &str, token: &str, method: &str, params: Value) -> Value {
    reqwest::Client::new()
        .post(format!("{url}/rpc"))
        .bearer_auth(token)
        .json(&json!({ "method": method, "params": params }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

fn block(value: i16) -> String {
    let bytes: Vec<u8> = vec![value; 4 * 16_000]
        .into_iter()
        .flat_map(i16::to_le_bytes)
        .collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// A served runtime with one open recording.
struct Rig {
    url: String,
    token: String,
    other: String,
    id: String,
    speech: Arc<Blocks>,
    next: u64,
    _keep: (tempfile::TempDir, Box<dyn std::any::Any>),
}

impl Rig {
    async fn new() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let tokens = Tokens::in_store(dir.path());
        let token = tokens.create("laptop").unwrap();
        let other = tokens.create("other device").unwrap();
        let speech = Arc::new(Blocks {
            reads: Mutex::new(vec![]),
        });
        let app = App::new("Live speech").ark(
            Ark::builder()
                .store_root(dir.path())
                .provider(Arc::new(ark_model::MockProvider::new("m", vec![])))
                .planner(false)
                .critic(false)
                .build()
                .unwrap(),
        );
        let (ui, server) = ArkUi::new(app)
            .transcription_provider(speech.clone())
            .access(Access::Tokens(tokens))
            .serve_background("127.0.0.1:0")
            .await
            .unwrap();
        let url = format!("http://127.0.0.1:{}", server.port);
        let begin = rpc(&url, &token, "dictation_begin", json!({"place":""})).await;
        let id = begin["result"]["id"].as_str().expect("upload").to_string();
        Rig {
            url,
            token,
            other,
            id,
            speech,
            next: 0,
            _keep: (dir, Box::new((ui, server))),
        }
    }

    /// Four more seconds of audio holding `value`.
    async fn put(&mut self, value: i16) {
        let r = rpc(
            &self.url,
            &self.token,
            "dictation_chunk",
            json!({"place":"", "id":self.id, "index":self.next, "data":block(value)}),
        )
        .await;
        assert_eq!(r["ok"], true, "{r}");
        self.next += 1;
    }

    async fn preview(&self) -> String {
        let r = rpc(
            &self.url,
            &self.token,
            "dictation_preview",
            json!({"place":"", "id":self.id}),
        )
        .await;
        assert_eq!(r["ok"], true, "{r}");
        r["result"]["text"].as_str().unwrap().to_string()
    }

    async fn finish(&self) -> Value {
        rpc(
            &self.url,
            &self.token,
            "dictation_finish",
            json!({"place":"", "id":self.id}),
        )
        .await
    }

    fn last_read(&self) -> Option<usize> {
        self.speech.reads.lock().unwrap().last().copied()
    }

    fn longest_read(&self) -> usize {
        self.speech
            .reads
            .lock()
            .unwrap()
            .iter()
            .copied()
            .max()
            .unwrap_or(0)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_preview_settles_early_speech_and_the_final_text_joins_it() {
    let mut rig = Rig::new().await;
    assert_eq!(rig.preview().await, "", "no audio, no words");
    rig.put(1).await;
    assert_eq!(rig.preview().await, "block1");
    for value in [2, 3, 4] {
        rig.put(value).await;
    }
    assert_eq!(rig.preview().await, "block1 block2 block3 block4");
    rig.put(5).await;
    assert_eq!(
        rig.preview().await,
        "block1 block2 block3 block4 block5",
        "the first twelve seconds are settled and kept"
    );
    assert_eq!(
        rig.last_read(),
        Some(8 * 16_000),
        "the last preview read only the eight seconds not yet settled"
    );

    let owned = rpc(
        &rig.url,
        &rig.other,
        "dictation_preview",
        json!({"place":"", "id":rig.id}),
    )
    .await;
    assert_eq!(
        owned["ok"], false,
        "another client's credential cannot preview this recording: {owned}"
    );
    let finished = rig.finish().await;
    assert_eq!(
        finished["result"]["text"], "block1 block2 block3 block4 block5",
        "{finished}"
    );
    assert_eq!(
        rig.last_read(),
        Some(8 * 16_000),
        "finish read the unsettled tail only"
    );
    let gone = rpc(
        &rig.url,
        &rig.token,
        "dictation_preview",
        json!({"place":"", "id":rig.id}),
    )
    .await;
    assert_eq!(gone["ok"], false, "a finished recording has no preview");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_long_silence_does_not_stop_the_words_that_follow() {
    let mut rig = Rig::new().await;
    for _ in 0..3 {
        rig.put(0).await;
    }
    assert_eq!(rig.preview().await, "");
    for _ in 0..9 {
        rig.put(0).await;
        rig.preview().await;
    }
    rig.put(9).await;
    assert_eq!(
        rig.preview().await,
        "block9",
        "words after forty seconds of silence still appear"
    );
    assert!(
        rig.longest_read() <= 30 * 16_000,
        "no preview read more than thirty seconds: {}",
        rig.longest_read()
    );
    let finished = rig.finish().await;
    assert_eq!(finished["result"]["text"], "block9", "{finished}");
}
