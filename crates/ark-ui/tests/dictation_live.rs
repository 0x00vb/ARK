//! A recording shows its words while it goes on, and each preview reads only what is not settled.
use std::sync::{Arc, Mutex};

use ark_interact::App;
use ark_sdk::prelude::*;
use ark_ui::access::Tokens;
use ark_ui::transcription::{Segment, State, Status, TranscriptionProvider};
use ark_ui::{Access, ArkUi};
use base64::Engine as _;
use serde_json::{json, Value};

/// Each four seconds of audio holds one repeated value; its words are `block<value>`.
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
        Ok(pcm
            .chunks(4 * 16_000)
            .enumerate()
            .map(|(index, block)| Segment {
                start_ms: index as u64 * 4_000,
                end_ms: index as u64 * 4_000 + block.len() as u64 / 16,
                text: format!("block{}", block[0]),
            })
            .collect())
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

#[tokio::test(flavor = "multi_thread")]
async fn a_preview_settles_early_speech_and_the_final_text_joins_it() {
    let dir = tempfile::tempdir().unwrap();
    let tokens = Tokens::in_store(dir.path());
    let token = tokens.create("laptop").unwrap();
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
    let (_, server) = ArkUi::new(app)
        .transcription_provider(speech.clone())
        .access(Access::Tokens(tokens))
        .serve_background("127.0.0.1:0")
        .await
        .unwrap();
    let url = format!("http://127.0.0.1:{}", server.port);
    let begin = rpc(&url, &token, "dictation_begin", json!({"place":""})).await;
    let id = begin["result"]["id"].as_str().expect("upload").to_string();
    let put = |index: u64, value: i16| {
        let (url, token, id) = (url.clone(), token.clone(), id.clone());
        async move {
            let r = rpc(
                &url,
                &token,
                "dictation_chunk",
                json!({"place":"", "id":id, "index":index, "data":block(value)}),
            )
            .await;
            assert_eq!(r["ok"], true, "{r}");
        }
    };
    let preview = || {
        let (url, token, id) = (url.clone(), token.clone(), id.clone());
        async move {
            let r = rpc(&url, &token, "dictation_preview", json!({"place":"", "id":id})).await;
            assert_eq!(r["ok"], true, "{r}");
            r["result"]["text"].as_str().unwrap().to_string()
        }
    };

    assert_eq!(preview().await, "", "no audio, no words");
    put(0, 1).await;
    assert_eq!(preview().await, "block1");
    for (index, value) in [(1, 2), (2, 3), (3, 4)] {
        put(index, value).await;
    }
    assert_eq!(preview().await, "block1 block2 block3 block4");
    put(4, 5).await;
    assert_eq!(
        preview().await,
        "block1 block2 block3 block4 block5",
        "the first twelve seconds are settled and kept"
    );
    let reads = speech.reads.lock().unwrap().clone();
    assert_eq!(
        reads.last().copied(),
        Some(8 * 16_000),
        "the last preview read only the eight seconds not yet settled: {reads:?}"
    );

    let other = rpc(
        &url,
        &token,
        "dictation_preview",
        json!({"place":"", "id":id, "_dictation_owner":"forged"}),
    )
    .await;
    assert_eq!(other["ok"], true, "the transport sets the owner, not the page");
    let finished = rpc(&url, &token, "dictation_finish", json!({"place":"", "id":id})).await;
    assert_eq!(
        finished["result"]["text"], "block1 block2 block3 block4 block5",
        "{finished}"
    );
    let reads = speech.reads.lock().unwrap().clone();
    assert_eq!(reads.last().copied(), Some(8 * 16_000), "finish read the unsettled tail only");

    let gone = rpc(&url, &token, "dictation_preview", json!({"place":"", "id":id})).await;
    assert_eq!(gone["ok"], false, "a finished recording has no preview");
}
