//! Dictation uses the captured computer, not whichever chat a window now shows.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ark_interact::App;
use ark_sdk::prelude::*;
use ark_ui::access::Tokens;
use ark_ui::transcription::{State, Status, TranscriptionProvider};
use ark_ui::{Access, ArkUi};
use base64::Engine as _;
use serde_json::{json, Value};

struct Speech {
    name: &'static str,
    calls: AtomicUsize,
    delay: Duration,
}

#[async_trait::async_trait]
impl TranscriptionProvider for Speech {
    fn state(&self) -> State {
        State::new(Status::Ready)
    }
    async fn prepare(&self) -> anyhow::Result<()> {
        Ok(())
    }
    async fn transcribe(&self, pcm: Vec<i16>, _: Option<String>) -> anyhow::Result<String> {
        assert_eq!(pcm, vec![123; 1600]);
        self.calls.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(self.delay).await;
        Ok(self.name.into())
    }
}

fn app(dir: &std::path::Path) -> App {
    App::new("Speech test").ark(
        Ark::builder()
            .store_root(dir)
            .provider(Arc::new(ark_model::MockProvider::new("m", vec![])))
            .planner(false)
            .critic(false)
            .build()
            .unwrap(),
    )
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

#[tokio::test(flavor = "multi_thread")]
async fn recordings_keep_their_computer_and_authenticated_owner_after_chat_switches() {
    let a_dir = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let tokens_b = Tokens::in_store(b_dir.path());
    let link_token = tokens_b.create("origin").unwrap();
    let remote = Arc::new(Speech {
        name: "remote speech",
        calls: AtomicUsize::new(0),
        delay: Duration::from_secs(9),
    });
    let (_, b_server) = ArkUi::new(app(b_dir.path()))
        .transcription_provider(remote.clone())
        .access(Access::Tokens(tokens_b))
        .serve_background("127.0.0.1:0")
        .await
        .unwrap();
    let tokens_a = Tokens::in_store(a_dir.path());
    let token = tokens_a.create("laptop").unwrap();
    let other = tokens_a.create("other device").unwrap();
    let local = Arc::new(Speech {
        name: "local speech",
        calls: AtomicUsize::new(0),
        delay: Duration::ZERO,
    });
    let (a, a_server) = ArkUi::new(app(a_dir.path()))
        .transcription_provider(local.clone())
        .link(
            "vps",
            &format!("http://127.0.0.1:{}", b_server.port),
            &link_token,
        )
        .access(Access::Tokens(tokens_a))
        .serve_background("127.0.0.1:0")
        .await
        .unwrap();
    let url = format!("http://127.0.0.1:{}", a_server.port);
    let summary = a.call("summary", json!({})).await.unwrap();
    let local_chat = summary["session"]["id"].as_str().unwrap();
    let status = rpc(&url, &token, "dictation_status", json!({"place":"vps"})).await;
    assert_eq!(status["result"]["status"], "ready", "{status}");

    // Before any remote chat exists, the pending computer already owns Begin.
    let begin = rpc(&url, &token, "dictation_begin", json!({"place":"vps"})).await;
    let remote_id = begin["result"]["id"].as_str().expect("remote upload");
    let remote_runtime = begin["result"]["runtime"].as_str().unwrap();
    let here = rpc(&url, &token, "dictation_begin", json!({"place":""})).await;
    let local_id = here["result"]["id"]
        .as_str()
        .expect("separate local upload");
    let placed = a.call("new_session", json!({"place":"vps"})).await.unwrap();
    let held_chat = placed["session"]["id"].as_str().unwrap();
    let bytes: Vec<u8> = vec![123i16; 1600]
        .into_iter()
        .flat_map(i16::to_le_bytes)
        .collect();
    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
    let chunk = rpc(
        &url,
        &token,
        "dictation_chunk",
        json!({"place":"", "id":local_id, "index":0,"data":data}),
    )
    .await;
    assert_eq!(
        chunk["ok"], true,
        "local audio stays here while a held chat is shown: {chunk}"
    );
    let stolen = rpc(
        &url,
        &other,
        "dictation_cancel",
        json!({"place":"", "id":local_id,"_dictation_owner":"forged"}),
    )
    .await;
    assert_eq!(stolen["ok"], false, "ownership is transport-authenticated");
    let finished = rpc(
        &url,
        &token,
        "dictation_finish",
        json!({"place":"", "id":local_id}),
    )
    .await;
    assert_eq!(finished["result"]["text"], "local speech", "{finished}");
    a.call("open_session", json!({"id":local_chat}))
        .await
        .unwrap();
    let chunk = rpc(
        &url,
        &token,
        "dictation_chunk",
        json!({"place":"vps", "runtime":remote_runtime, "id":remote_id,"index":0,"data":data}),
    )
    .await;
    assert_eq!(chunk["ok"], true, "{chunk}");
    let finished = rpc(
        &url,
        &token,
        "dictation_finish",
        json!({"place":"vps","runtime":remote_runtime,"id":remote_id}),
    )
    .await;
    assert_eq!(
        finished["result"]["text"], "remote speech",
        "finish takes longer than the normal eight-second link timeout: {finished}"
    );
    assert_eq!(local.calls.load(Ordering::SeqCst), 1);
    assert_eq!(remote.calls.load(Ordering::SeqCst), 1);

    let begin = rpc(&url, &token, "dictation_begin", json!({"place":"vps"})).await;
    a.call("open_session", json!({"id":held_chat}))
        .await
        .unwrap();
    let cancelled = rpc(
        &url,
        &token,
        "dictation_cancel",
        json!({"place":"vps","id":begin["result"]["id"]}),
    )
    .await;
    assert_eq!(cancelled["result"]["cancelled"], true, "{cancelled}");
    let unknown = rpc(
        &url,
        &token,
        "dictation_begin",
        json!({"place":"missing-server"}),
    )
    .await;
    assert_eq!(
        unknown["ok"], false,
        "an unknown computer never falls back here"
    );
    let missing = rpc(&url, &token, "dictation_begin", json!({})).await;
    assert_eq!(missing["ok"], false, "every request must name its computer");
    assert_eq!(local.calls.load(Ordering::SeqCst), 1);
    // Reusing a configured name for another server must fail before forwarding audio.
    a.links()
        .set(vec![ark_interact::links::Link::new("vps", &url, &token)]);
    let changed = rpc(
        &url,
        &token,
        "dictation_chunk",
        json!({"place":"vps", "runtime":remote_runtime,"id":remote_id,"index":0,"data":data}),
    )
    .await;
    assert_eq!(changed["ok"], false);
    assert!(
        changed["error"]
            .as_str()
            .unwrap()
            .contains("computer changed"),
        "{changed}"
    );
}
