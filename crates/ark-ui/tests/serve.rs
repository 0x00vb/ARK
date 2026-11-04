//! A runtime other machines connect to (ADR 0016): saved access tokens, any `Host`, the
//! `remote::Client`, and work that runs with no client connected.

use std::sync::Arc;
use std::time::Duration;

use ark_interact::ark_orchestrate::Orchestrator;
use ark_interact::App;
use ark_sdk::prelude::ark_model::{Completion, MockProvider, ToolCall, Usage};
use ark_sdk::prelude::*;
use ark_ui::access::Tokens;
use ark_ui::remote::Client;
use ark_ui::{Access, ArkUi};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

struct Notes;

#[tokio::test]
async fn wasm_compilation_is_scoped_to_an_exact_trusted_worker() {
    let dir = tempfile::tempdir().unwrap();
    let (_, served) = ArkUi::new(app(dir.path()))
        .wasm_worker_asset("reader.js", b"export {};".as_slice())
        .asset("plain.js", b"export {};".as_slice())
        .asset("reader.wasm", b"bytes".as_slice())
        .serve_background("127.0.0.1:0")
        .await
        .unwrap();
    for (path, wasm) in [
        ("/".to_string(), false),
        ("/ext/reader.js".into(), true),
        (format!("/_ark/{}/ext/reader.js", ark_ui::VERSION), true),
        ("/ext/plain.js".into(), false),
        ("/ext/reader.js.js".into(), false),
        ("/ext/%72eader.js".into(), false),
        ("/ext/reader.wasm".into(), false),
    ] {
        let response = raw(
            served.port,
            format!(
                "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
                served.port
            ),
        )
        .await;
        assert_eq!(
            response.contains("'wasm-unsafe-eval'"),
            wasm,
            "{path}: {response}"
        );
        assert!(!response.contains("'unsafe-eval'"));
        assert!(response.contains("frame-ancestors 'none'"));
        if wasm {
            assert!(response.contains("connect-src 'none'"));
            assert!(response.contains("text/javascript"));
        }
        if path.ends_with(".wasm") {
            assert!(response.contains("application/wasm"));
        }
        assert_eq!(response.matches("Content-Security-Policy:").count(), 1);
    }
}

#[tokio::test]
async fn only_registered_frame_assets_receive_the_same_origin_policy() {
    let dir = tempfile::tempdir().unwrap();
    let (_, served) = ArkUi::new(app(dir.path()))
        .frame_asset("viewer.html", b"<p>viewer</p>".as_slice())
        .asset("sibling.html", b"<p>sibling</p>".as_slice())
        .serve_background("127.0.0.1:0")
        .await
        .unwrap();
    for (path, framed) in [
        ("/", false),
        ("/ext/viewer.html", true),
        ("/ext/sibling.html", false),
        ("/ext/viewer.html.js", false),
        ("/ext/%76iewer.html", false),
    ] {
        let response = raw(
            served.port,
            format!(
                "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
                served.port
            ),
        )
        .await;
        assert!(
            response.contains(if framed {
                "frame-ancestors 'self'"
            } else {
                "frame-ancestors 'none'"
            }),
            "{path}: {response}"
        );
        assert_eq!(response.matches("Content-Security-Policy:").count(), 1);
        assert!(response.contains("X-Content-Type-Options: nosniff"));
        assert!(response.contains("Cache-Control: no-store"));
    }
}

#[async_trait::async_trait]
impl Adapter for Notes {
    fn name(&self) -> &str {
        "notes"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            "notes.read",
            "read the notes",
            RiskTier::Read,
        )]
    }
    async fn run(
        &self,
        _action: &str,
        _i: Value,
        _c: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        Ok(ActionOutput::json(json!({ "text": "the locket" })))
    }
}

#[tokio::test]
async fn native_client_bootstraps_pairs_and_reads_typed_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let tokens = Tokens::in_store(dir.path());
    let code = tokens
        .pair_code(
            "Owner Mac",
            Duration::from_secs(60),
            Duration::from_secs(90 * 24 * 3600),
            None,
        )
        .unwrap();
    let (runtime, served) = ArkUi::new(app(dir.path()))
        .access(Access::Tokens(tokens.clone()))
        .serve_background("127.0.0.1:0")
        .await
        .unwrap();
    let address = "runtime.example";

    let bootstrap = raw(
        served.port,
        format!("GET /client/bootstrap HTTP/1.1\r\nHost: {address}\r\n\r\n"),
    )
    .await;
    assert!(bootstrap.starts_with("HTTP/1.1 200"), "{bootstrap}");
    assert!(bootstrap.contains(r#""major":1"#));

    let body = json!({
        "code": code,
        "deviceName": "Valentino's iPhone",
        "installationNonce": "install-1",
    })
    .to_string();
    let paired = raw(
        served.port,
        post("/client/v1/pair", address, None, "", &body),
    )
    .await;
    assert!(paired.starts_with("HTTP/1.1 200"), "{paired}");
    let paired: Value = serde_json::from_str(paired.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    let credential = paired["credential"].as_str().unwrap();
    let mutation_epoch = paired["mutationEpoch"].as_str().unwrap();
    assert!(credential.starts_with("arks_"));
    assert_eq!(paired["nextMutationCounter"], "1");
    assert_eq!(
        tokens.check(credential).as_deref(),
        Some("Owner Mac › Valentino's iPhone")
    );
    let auth = format!("Authorization: Bearer {credential}\r\n");

    for path in ["/client/v1/hello", "/client/v1/home", "/client/v1/chats"] {
        let response = raw(
            served.port,
            format!("GET {path} HTTP/1.1\r\nHost: {address}\r\n{auth}\r\n"),
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 200"), "{path}: {response}");
        let value: Value =
            serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert!(value.is_object(), "{path}: {value}");
        match path {
            "/client/v1/hello" => {
                serde_json::from_value::<ark_protocol::Hello>(value.clone()).unwrap();
                assert_eq!(value["mutationEpoch"], mutation_epoch);
                assert_eq!(value["nextMutationCounter"], "1");
            }
            "/client/v1/home" => {
                serde_json::from_value::<ark_protocol::HomeSnapshot>(value).unwrap();
            }
            "/client/v1/chats" => {
                serde_json::from_value::<ark_protocol::ChatsSnapshot>(value).unwrap();
            }
            _ => unreachable!(),
        }
    }

    let before_terminal = runtime
        .call(
            "add_project",
            json!({ "name": "Phone Terminal", "path": work.path() }),
        )
        .await
        .unwrap();
    let project = before_terminal["project"]["id"].as_str().unwrap();
    for path in ["/client/v1/projects", "/client/v1/computers"] {
        let response = raw(
            served.port,
            format!("GET {path} HTTP/1.1\r\nHost: {address}\r\n{auth}\r\n"),
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 200"), "{path}: {response}");
        let body = response.split("\r\n\r\n").nth(1).unwrap();
        if path.ends_with("projects") {
            serde_json::from_str::<ark_protocol::ProjectsSnapshot>(body).unwrap();
        } else {
            serde_json::from_str::<ark_protocol::ComputersSnapshot>(body).unwrap();
        }
    }
    let opened = raw(
        served.port,
        post(
            "/client/v1/terminals/open",
            address,
            None,
            &auth,
            &json!({
                "projectId": project,
                "computer": null,
                "cols": 80,
                "rows": 24,
                "copyLargeProject": false,
            })
            .to_string(),
        ),
    )
    .await;
    assert!(opened.starts_with("HTTP/1.1 200"), "{opened}");
    let opened_body = opened.split("\r\n\r\n").nth(1).unwrap();
    serde_json::from_str::<ark_protocol::TerminalOpenResponse>(opened_body).unwrap();
    let opened: Value = serde_json::from_str(opened_body).unwrap();
    let handle = opened["handle"].as_str().unwrap();
    let generation = opened["generation"].as_str().unwrap();
    let write = json!({
        "handle": handle,
        "generation": generation,
        "sequence": "1",
        "data": "printf 'native-terminal-ok\\n'\r",
    })
    .to_string();
    let written = raw(
        served.port,
        post("/client/v1/terminals/write", address, None, &auth, &write),
    )
    .await;
    assert!(written.starts_with("HTTP/1.1 200"), "{written}");
    let duplicate = raw(
        served.port,
        post("/client/v1/terminals/write", address, None, &auth, &write),
    )
    .await;
    assert!(duplicate.starts_with("HTTP/1.1 409"), "{duplicate}");

    let mut cursor = opened["cursor"].as_str().unwrap().to_string();
    let mut output = String::new();
    for _ in 0..20 {
        let read = raw(
            served.port,
            post(
                "/client/v1/terminals/read",
                address,
                None,
                &auth,
                &json!({ "handle": handle, "generation": generation, "after": cursor }).to_string(),
            ),
        )
        .await;
        assert!(read.starts_with("HTTP/1.1 200"), "{read}");
        let read: Value = serde_json::from_str(read.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        output.push_str(read["data"].as_str().unwrap_or(""));
        cursor = read["cursor"].as_str().unwrap().to_string();
        if output.contains("native-terminal-ok") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(output.contains("native-terminal-ok"), "{output:?}");
    let after_terminal = runtime.call("summary", json!({})).await.unwrap();
    assert_eq!(
        after_terminal["session"]["id"], before_terminal["session"]["id"],
        "Terminal must not change the current chat"
    );

    let before = runtime.call("summary", json!({})).await.unwrap();

    for request in [
        format!("GET /client/v1/hello?token={credential} HTTP/1.1\r\nHost: {address}\r\n\r\n"),
        format!(
            "GET /client/v1/hello HTTP/1.1\r\nHost: {address}\r\nX-Ark-Token: {credential}\r\n\r\n"
        ),
    ] {
        let response = raw(served.port, request).await;
        assert!(response.starts_with("HTTP/1.1 401"), "{response}");
    }

    let create_body = json!({ "project": project }).to_string();
    let created = raw(
        served.port,
        post("/client/v1/chats", address, None, &auth, &create_body),
    )
    .await;
    assert!(created.starts_with("HTTP/1.1 200"), "{created}");
    let created: Value = serde_json::from_str(created.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    let after = runtime.call("summary", json!({})).await.unwrap();
    assert_ne!(created["id"], before["session"]["id"]);
    assert_eq!(after["session"]["id"], before["session"]["id"]);

    let mutation_input = json!({ "attachmentIds": [], "body": "Review the delivery dates." });
    let input_digest = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&mutation_input).unwrap())
    );
    let mutation = json!({
        "epoch": mutation_epoch,
        "counter": "2",
        "operation": "chat.send",
        "target": created["id"],
        "inputDigest": input_digest.clone(),
        "input": mutation_input.clone(),
    })
    .to_string();
    let unsupported = json!({
        "epoch": mutation_epoch,
        "counter": "1",
        "operation": "unknown.operation",
        "target": created["id"],
        "inputDigest": input_digest,
        "input": mutation_input,
    })
    .to_string();
    let refused = raw(
        served.port,
        post("/client/v1/mutations", address, None, &auth, &unsupported),
    )
    .await;
    assert!(refused.starts_with("HTTP/1.1 400"), "{refused}");
    let missing_input = json!({ "attachmentIds": [], "body": "No missing chat." });
    let missing_digest = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&missing_input).unwrap())
    );
    let missing = json!({
        "epoch": mutation_epoch,
        "counter": "1",
        "operation": "chat.send",
        "target": "missing-chat",
        "inputDigest": missing_digest,
        "input": missing_input,
    })
    .to_string();
    let missing = raw(
        served.port,
        post("/client/v1/mutations", address, None, &auth, &missing),
    )
    .await;
    assert!(missing.starts_with("HTTP/1.1 200"), "{missing}");
    assert!(missing.contains("no_longer_applicable"), "{missing}");
    let first = raw(
        served.port,
        post("/client/v1/mutations", address, None, &auth, &mutation),
    )
    .await;
    assert!(first.starts_with("HTTP/1.1 200"), "{first}");
    let first_receipt: Value =
        serde_json::from_str(first.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(first_receipt["status"], "completed");

    let replay = raw(
        served.port,
        post("/client/v1/mutations", address, None, &auth, &mutation),
    )
    .await;
    let replay_receipt: Value =
        serde_json::from_str(replay.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(replay_receipt["id"], first_receipt["id"]);

    let pending_input = json!({ "attachmentIds": [], "body": "Follow up once." });
    let pending_digest = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&pending_input).unwrap())
    );
    let pending = json!({
        "epoch": mutation_epoch,
        "counter": "3",
        "operation": "chat.send",
        "target": created["id"],
        "inputDigest": pending_digest,
        "input": pending_input,
    })
    .to_string();
    let accepted = raw(
        served.port,
        post("/client/v1/mutations", address, None, &auth, &pending),
    )
    .await;
    assert!(accepted.starts_with("HTTP/1.1 200"), "{accepted}");
    assert!(accepted.contains(r#""status":"accepted""#), "{accepted}");

    let gap_input = json!({ "attachmentIds": [], "body": "Skipped counter." });
    let gap_digest = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&gap_input).unwrap())
    );
    let gap = json!({
        "epoch": mutation_epoch,
        "counter": "4",
        "operation": "chat.send",
        "target": created["id"],
        "inputDigest": gap_digest,
        "input": gap_input,
    })
    .to_string();
    let gap = raw(
        served.port,
        post("/client/v1/mutations", address, None, &auth, &gap),
    )
    .await;
    assert!(gap.starts_with("HTTP/1.1 409"), "{gap}");
    assert!(gap.contains("counter_gap"), "{gap}");

    let mut pending_receipt = Value::Null;
    for _ in 0..40 {
        let response = raw(
            served.port,
            post("/client/v1/mutations", address, None, &auth, &pending),
        )
        .await;
        pending_receipt = serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        if pending_receipt["status"] == "completed" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(pending_receipt["status"], "completed");

    let device = format!("{:x}", Sha256::digest(credential.as_bytes()));
    let record_id = format!("{device}:{mutation_epoch}:3");
    let store = ark_store::Store::open(dir.path()).unwrap();
    let turns = store
        .list_session_turns(created["id"].as_str().unwrap(), None)
        .unwrap();
    assert_eq!(
        turns
            .iter()
            .filter(|turn| turn["input"] == "Follow up once.")
            .count(),
        1
    );
    let mut interrupted = store
        .get_record("client_mutation", &record_id)
        .unwrap()
        .unwrap();
    assert!(interrupted["effect"]["turnIndex"].is_number());
    interrupted["receipt"]["status"] = json!("accepted");
    interrupted["receipt"]["display"] = json!("Accepted");
    store
        .put_record("client_mutation", &record_id, &device, &interrupted)
        .unwrap();
    let (_resumed, _served_again) = ArkUi::new(app(dir.path()))
        .access(Access::Tokens(tokens.clone()))
        .serve_background("127.0.0.1:0")
        .await
        .unwrap();
    let resumed = store
        .get_record("client_mutation", &record_id)
        .unwrap()
        .unwrap();
    assert_eq!(resumed["receipt"]["status"], "completed");
    let turns = store
        .list_session_turns(created["id"].as_str().unwrap(), None)
        .unwrap();
    assert_eq!(
        turns
            .iter()
            .filter(|turn| turn["input"] == "Follow up once.")
            .count(),
        1,
        "restart after the durable turn began must not send it again"
    );

    let mut events = tokio::net::TcpStream::connect(("127.0.0.1", served.port))
        .await
        .unwrap();
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    events
        .write_all(
            format!("GET /client/v1/events?after=0 HTTP/1.1\r\nHost: {address}\r\n{auth}\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    let streamed = tokio::time::timeout(Duration::from_secs(2), async {
        let mut streamed = Vec::new();
        let mut chunk = [0; 4 * 1024];
        while !String::from_utf8_lossy(&streamed).contains(r#""type":"terminal""#) {
            let read = events.read(&mut chunk).await.unwrap();
            assert!(read > 0, "event stream closed before an event");
            streamed.extend_from_slice(&chunk[..read]);
        }
        String::from_utf8(streamed).unwrap()
    })
    .await
    .unwrap();
    assert!(streamed.contains("text/event-stream"), "{streamed}");
    assert!(streamed.contains(r#""sequence":"#), "{streamed}");
    assert!(streamed.contains(r#""type":"terminal""#), "{streamed}");
    assert!(streamed.contains(handle), "{streamed}");
    assert!(
        streamed.contains(r#""affects":[]"#) || streamed.contains(r#""affects":["#),
        "{streamed}"
    );

    let mut future_events = tokio::net::TcpStream::connect(("127.0.0.1", served.port))
        .await
        .unwrap();
    future_events
        .write_all(
            format!(
                "GET /client/v1/events?after=18446744073709551615 HTTP/1.1\r\nHost: {address}\r\n{auth}\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let future = tokio::time::timeout(Duration::from_secs(2), async {
        let mut streamed = Vec::new();
        let mut chunk = [0; 4 * 1024];
        while !String::from_utf8_lossy(&streamed).contains(r#""type":"resync""#) {
            let read = future_events.read(&mut chunk).await.unwrap();
            assert!(read > 0, "event stream closed before resync");
            streamed.extend_from_slice(&chunk[..read]);
        }
        String::from_utf8(streamed).unwrap()
    })
    .await
    .unwrap();
    assert!(future.contains(r#""type":"resync""#), "{future}");

    let ended = raw(
        served.port,
        post(
            "/client/v1/terminals/end",
            address,
            None,
            &auth,
            &json!({ "handle": handle, "generation": generation }).to_string(),
        ),
    )
    .await;
    assert!(ended.starts_with("HTTP/1.1 200"), "{ended}");

    let signed_out = raw(
        served.port,
        format!("DELETE /client/v1/session HTTP/1.1\r\nHost: {address}\r\n{auth}\r\n"),
    )
    .await;
    assert!(signed_out.starts_with("HTTP/1.1 204"), "{signed_out}");
    let revoked = raw(
        served.port,
        format!("GET /client/v1/hello HTTP/1.1\r\nHost: {address}\r\n{auth}\r\n"),
    )
    .await;
    assert!(revoked.starts_with("HTTP/1.1 401"), "{revoked}");
    let revoked_terminal = raw(
        served.port,
        post(
            "/client/v1/terminals/read",
            address,
            None,
            &auth,
            &json!({ "handle": handle, "generation": generation, "after": cursor }).to_string(),
        ),
    )
    .await;
    assert!(
        revoked_terminal.starts_with("HTTP/1.1 401"),
        "{revoked_terminal}"
    );
}

/// Reads the notes once, then answers "Read.".
fn model() -> Arc<MockProvider> {
    Arc::new(
        MockProvider::new("m", vec![])
            .with_chunk_delay(Duration::from_millis(200))
            .with_fallback(|req| {
                if req.messages.iter().any(|m| m.tool_call_id.is_some()) {
                    Completion {
                        content: "Read.".into(),
                        tool_calls: vec![],
                        usage: Usage::default(),
                    }
                } else {
                    Completion {
                        content: String::new(),
                        tool_calls: vec![ToolCall {
                            id: "c".into(),
                            name: "notes.read".into(),
                            arguments: json!({}),
                        }],
                        usage: Usage::default(),
                    }
                }
            }),
    )
}

fn app(dir: &std::path::Path) -> App {
    let provider: Arc<dyn ark_model::Provider> = model();
    let ark = Ark::builder()
        .store_root(dir)
        .tool(Arc::new(Notes))
        .profile(AgentProfile::builder().core_actions(["notes.read"]).build())
        .provider(provider)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    App::new("Test").ark(ark).orchestrate(|ark, project, gate| {
        Ok(Orchestrator::builder(ark, project)
            .approvals(gate)
            .build()?)
    })
}

async fn resource_call(
    client: &Client,
    kind: &str,
    operation: &str,
    id: &str,
    input: Value,
) -> anyhow::Result<Value> {
    let location = client.call("summary", json!({})).await?["location"].clone();
    client
        .call(
            "resource",
            json!({
                "ref": { "location": location, "kind": kind, "id": id },
                "operation": { "kind": operation, "input": input },
            }),
        )
        .await
}

/// A raw HTTP request and the whole response.
async fn raw(port: u16, req: String) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    out
}

fn rpc(host: &str, auth: &str, method: &str) -> String {
    let body = json!({ "method": method, "params": {} }).to_string();
    format!(
        "POST /rpc HTTP/1.1\r\nHost: {host}\r\n{auth}Content-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

#[tokio::test]
async fn saved_tokens_admit_any_host_and_revoke_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let tokens = Tokens::in_store(dir.path());
    let laptop = tokens.create("laptop").unwrap();
    let phone = tokens.create("phone").unwrap();
    let (_host, served) = ArkUi::new(app(dir.path()))
        .access(Access::Tokens(tokens.clone()))
        .serve_background("127.0.0.1:0")
        .await
        .unwrap();
    assert!(served.token.is_empty() && !served.url.contains("token"));
    let port = served.port;

    // A proxy passes its own name: accepted, with a token.
    let host = "ark.example.com";
    let health = raw(
        port,
        format!("GET /health HTTP/1.1\r\nHost: {host}\r\n\r\n"),
    )
    .await;
    assert!(health.starts_with("HTTP/1.1 200") && health.contains(r#""ok":true"#));
    let page = raw(port, format!("GET / HTTP/1.1\r\nHost: {host}\r\n\r\n")).await;
    assert!(
        page.starts_with("HTTP/1.1 200"),
        "the page itself is public"
    );
    let none = raw(port, rpc(host, "", "hello")).await;
    assert!(none.starts_with("HTTP/1.1 401"));
    let wrong = raw(
        port,
        rpc(host, "Authorization: Bearer ark_wrong\r\n", "hello"),
    )
    .await;
    assert!(wrong.starts_with("HTTP/1.1 401"));
    let ok = raw(
        port,
        rpc(
            host,
            &format!("Authorization: Bearer {laptop}\r\n"),
            "hello",
        ),
    )
    .await;
    assert!(ok.contains(r#""ok":true"#), "{ok}");
    let events = raw(
        port,
        format!("GET /events?token=ark_wrong HTTP/1.1\r\nHost: {host}\r\n\r\n"),
    )
    .await;
    assert!(events.starts_with("HTTP/1.1 401"));

    // Revoking one token shuts it out without a restart; the other still works.
    assert!(tokens.revoke("laptop").unwrap());
    let revoked = raw(
        port,
        rpc(host, &format!("x-ark-token: {laptop}\r\n"), "hello"),
    )
    .await;
    assert!(revoked.starts_with("HTTP/1.1 401"));
    let still = raw(
        port,
        rpc(host, &format!("x-ark-token: {phone}\r\n"), "hello"),
    )
    .await;
    assert!(still.contains(r#""ok":true"#));
}

/// A POST with a JSON body, from a page at `origin` (or none).
fn post(path: &str, host: &str, origin: Option<&str>, extra: &str, body: &str) -> String {
    let origin = origin
        .map(|o| format!("Origin: {o}\r\n"))
        .unwrap_or_default();
    format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\n{origin}{extra}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

/// The value a response sets for `name`, and the rest of that header line.
fn set_cookie<'a>(resp: &'a str, name: &str) -> Option<(&'a str, &'a str)> {
    resp.lines()
        .find_map(|l| l.strip_prefix("Set-Cookie: "))
        .and_then(|c| c.strip_prefix(name)?.strip_prefix('='))
        .and_then(|c| c.split_once(';'))
}

#[tokio::test]
async fn a_one_time_link_becomes_a_session_cookie_the_page_never_sees() {
    let dir = tempfile::tempdir().unwrap();
    let tokens = Tokens::in_store(dir.path());
    let hour = Duration::from_secs(3600);
    let (_host, served) = ArkUi::new(app(dir.path()))
        .access(Access::Tokens(tokens.clone()))
        .entry("https://account.example.com/open")
        .serve_background("127.0.0.1:0")
        .await
        .unwrap();
    let port = served.port;
    let host = "ws-1.example.dev";
    let me = "https://ws-1.example.dev";

    // Out: the page learns where to go, and every answer carries the page's policy.
    let out = raw(
        port,
        format!("GET /session HTTP/1.1\r\nHost: {host}\r\n\r\n"),
    )
    .await;
    assert!(out.contains(r#""signed_in":false"#), "{out}");
    assert!(out.contains(r#""entry":"https://account.example.com/open""#));
    let page = raw(port, format!("GET / HTTP/1.1\r\nHost: {host}\r\n\r\n")).await;
    for h in [
        "Content-Security-Policy: default-src 'self'; script-src 'self';",
        "frame-ancestors 'none'",
        "Referrer-Policy: no-referrer",
        "X-Content-Type-Options: nosniff",
    ] {
        assert!(page.contains(h), "page lacks {h}");
    }

    // A link from another page is refused; from this page it becomes an HttpOnly cookie, once.
    let code = tokens
        .link("Chrome on Mac", Duration::from_secs(60), hour)
        .unwrap();
    let body = json!({ "code": code }).to_string();
    let elsewhere = raw(
        port,
        post("/session", host, Some("https://evil.example"), "", &body),
    )
    .await;
    assert!(elsewhere.starts_with("HTTP/1.1 403"), "{elsewhere}");
    let inside = raw(port, post("/session", host, Some(me), "", &body)).await;
    assert!(inside.starts_with("HTTP/1.1 200"), "{inside}");
    let (session, attrs) = set_cookie(&inside, "__Host-ark-session").expect("a cookie");
    assert!(session.starts_with("arks_"));
    for a in ["Path=/", "HttpOnly", "SameSite=Strict", "Secure"] {
        assert!(
            attrs.contains(a) || inside.contains(&format!("; {a}")),
            "cookie lacks {a}"
        );
    }
    assert!(
        !inside.split("\r\n\r\n").nth(1).unwrap().contains(session),
        "not in the body"
    );
    let again = raw(port, post("/session", host, Some(me), "", &body)).await;
    assert!(
        again.starts_with("HTTP/1.1 401") && again.contains(r#""entry""#),
        "once"
    );

    // The cookie reads and changes from this page; a change from another page is refused.
    let cookie = format!("Cookie: theme=dark; __Host-ark-session={session}\r\n");
    let rpc_body = json!({ "method": "hello", "params": {} }).to_string();
    let ok = raw(port, post("/rpc", host, Some(me), &cookie, &rpc_body)).await;
    assert!(ok.contains(r#""ok":true"#), "{ok}");
    let csrf = raw(
        port,
        post(
            "/rpc",
            host,
            Some("https://evil.example"),
            &cookie,
            &rpc_body,
        ),
    )
    .await;
    assert!(csrf.starts_with("HTTP/1.1 403"), "{csrf}");
    let blind = raw(port, post("/rpc", host, None, &cookie, &rpc_body)).await;
    assert!(
        blind.starts_with("HTTP/1.1 403"),
        "no Origin, no Sec-Fetch-Site: refused"
    );
    let same = raw(
        port,
        post(
            "/rpc",
            host,
            None,
            &format!("{cookie}Sec-Fetch-Site: same-origin\r\n"),
            &rpc_body,
        ),
    )
    .await;
    assert!(same.contains(r#""ok":true"#));
    let state = raw(
        port,
        format!("GET /session HTTP/1.1\r\nHost: {host}\r\n{cookie}\r\n"),
    )
    .await;
    assert!(state.contains(r#""signed_in":true"#));

    // Signing out ends this session only; revoking the name ends the rest.
    let other = tokens
        .redeem(
            &tokens
                .link("Chrome on Mac", Duration::from_secs(60), hour)
                .unwrap(),
        )
        .unwrap()
        .unwrap();
    let del = raw(port, format!("DELETE /session HTTP/1.1\r\nHost: {host}\r\nOrigin: {me}\r\n{cookie}Content-Length: 0\r\n\r\n")).await;
    assert!(del.starts_with("HTTP/1.1 200"));
    assert_eq!(
        set_cookie(&del, "__Host-ark-session").map(|c| c.0),
        Some("")
    );
    let after = raw(port, post("/rpc", host, Some(me), &cookie, &rpc_body)).await;
    assert!(after.starts_with("HTTP/1.1 401"));
    assert_eq!(tokens.check(&other.token).as_deref(), Some("Chrome on Mac"));
    tokens.revoke("Chrome on Mac").unwrap();
    let revoked = raw(
        port,
        post(
            "/rpc",
            host,
            Some(me),
            &format!("Cookie: __Host-ark-session={}\r\n", other.token),
            &rpc_body,
        ),
    )
    .await;
    assert!(revoked.starts_with("HTTP/1.1 401"));

    // On a loopback address the cookie is named for the port and works over plain HTTP.
    let local = format!("127.0.0.1:{port}");
    let code = tokens
        .link("Firefox", Duration::from_secs(60), hour)
        .unwrap();
    let body = json!({ "code": code }).to_string();
    let inside = raw(
        port,
        post(
            "/session",
            &local,
            Some(&format!("http://{local}")),
            "",
            &body,
        ),
    )
    .await;
    let (_, attrs) = set_cookie(&inside, &format!("ark-session-{port}")).expect("a port cookie");
    assert!(!attrs.contains("Secure") && !inside.contains("; Secure"));
}

#[tokio::test]
async fn the_one_time_token_still_answers_only_the_loopback_host() {
    let dir = tempfile::tempdir().unwrap();
    let (_host, served) = ArkUi::new(app(dir.path()))
        .serve_background("127.0.0.1:0")
        .await
        .unwrap();
    let auth = format!("x-ark-token: {}\r\n", served.token);
    let proxied = raw(served.port, rpc("ark.example.com", &auth, "hello")).await;
    assert!(proxied.starts_with("HTTP/1.1 421"));
}

#[tokio::test]
async fn a_client_runs_a_task_that_finishes_with_no_client_connected() {
    let dir = tempfile::tempdir().unwrap();
    let tokens = Tokens::in_store(dir.path());
    let token = tokens.create("script").unwrap();
    let (_host, served) = ArkUi::new(app(dir.path()))
        .access(Access::Tokens(tokens))
        .serve_background("127.0.0.1:0")
        .await
        .unwrap();
    let url = format!("http://127.0.0.1:{}", served.port);

    let refused = Client::new(&url, "ark_wrong").hello().await.unwrap_err();
    assert!(format!("{refused}").contains("refused the access token"));

    let client = Client::new(&url, &token);
    let hello = client.hello().await.unwrap();
    assert_eq!(hello["version"], env!("CARGO_PKG_VERSION"));
    resource_call(
        &client,
        "project",
        "spawn_agent",
        "",
        json!({ "spec": { "name": "reader", "role": "reads the notes", "tools": ["notes.*"] } }),
    )
    .await
    .unwrap();
    let r = resource_call(
        &client,
        "project",
        "submit_task",
        "",
        json!({ "agent": "reader", "task": { "objective": "read the notes" } }),
    )
    .await
    .unwrap();
    let task = r["task"].as_str().unwrap().to_string();
    // Nobody listens to events: the task runs in the runtime on its own. A new client (a
    // window opened later) reads its result.
    drop(client);
    let later = Client::new(&url, &token);
    let mut state = Value::Null;
    for _ in 0..100 {
        let t = resource_call(&later, "project", "task", "", json!({ "task": task }))
            .await
            .unwrap();
        state = t["task"]["state"].clone();
        if state == "done" {
            assert_eq!(t["task"]["result"], "Read.");
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the task did not finish: {state}");
}

#[tokio::test]
async fn a_folder_sent_from_another_computer_becomes_a_project_there() {
    use ark_ui::remote::{send_folder, Folder};

    let dir = tempfile::tempdir().unwrap();
    let tokens = Tokens::in_store(dir.path());
    let token = tokens.create("laptop").unwrap();
    let (_host, served) = ArkUi::new(app(dir.path()))
        .access(Access::Tokens(tokens))
        .serve_background("127.0.0.1:0")
        .await
        .unwrap();
    let client = Client::new(&format!("http://127.0.0.1:{}", served.port), &token);

    // The folder on this computer: nested files, one larger than a part, an empty one, and a
    // link, which is not sent.
    let local = tempfile::tempdir().unwrap();
    let l = local.path();
    std::fs::create_dir_all(l.join("chapters/draft")).unwrap();
    std::fs::write(l.join("notes.md"), "# Notes\n").unwrap();
    std::fs::write(l.join("chapters/draft/one.md"), "It was a dark night.").unwrap();
    let big: Vec<u8> = (0..2_500_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(l.join("chapters/cover.bin"), &big).unwrap();
    std::fs::write(l.join("empty.txt"), "").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("/etc", l.join("outside")).unwrap();
    let folder = Folder::read(l).unwrap();
    assert_eq!(folder.files(), 4);
    #[cfg(unix)]
    assert_eq!(folder.skipped(), 1);
    assert_eq!(folder.bytes(), 2_500_000 + 8 + 20);

    // Stopped halfway: no project, nothing left.
    let before = client.call("projects", json!({})).await.unwrap();
    let mut calls = 0;
    let stopped = send_folder(&client, &folder, "novel", |_| {
        calls += 1;
        calls < 2
    })
    .await;
    assert!(stopped.is_err());
    assert_eq!(client.call("projects", json!({})).await.unwrap(), before);
    let uploads = dir.path().join("uploads");
    assert_eq!(std::fs::read_dir(&uploads).unwrap().count(), 0);

    let mut last = Default::default();
    let p = send_folder(&client, &folder, "novel", |s| {
        last = s;
        true
    })
    .await
    .unwrap();
    assert_eq!(p["name"], "novel");
    assert_eq!((last.files, last.bytes), (4, folder.bytes()));
    let id = p["id"].as_str().unwrap();
    client
        .call("open_project", json!({ "id": id }))
        .await
        .unwrap();
    let read = resource_call(
        &client,
        "file",
        "file_read",
        "chapters/draft/one.md",
        json!({ "path": "chapters/draft/one.md" }),
    )
    .await
    .unwrap();
    assert_eq!(read["text"], "It was a dark night.");
    let root = resource_call(&client, "file", "files", "", json!({ "path": "" }))
        .await
        .unwrap();
    let names: Vec<&str> = root["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["chapters", "empty.txt", "notes.md"]);
    let folder_there = std::path::PathBuf::from(root["root"].as_str().unwrap());
    assert_eq!(
        std::fs::read(folder_there.join("chapters/cover.bin")).unwrap(),
        big
    );

    // The same name again is a second project.
    let again = send_folder(&client, &folder, "novel", |_| true)
        .await
        .unwrap();
    assert_eq!(again["name"], "novel 2");

    // A path out of the upload's folder is refused.
    let out = client
        .call(
            "upload_project_parts",
            json!({ "upload": "abcdef123456", "parts": [{ "path": "../../access.json", "data": "", "offset": 0 }] }),
        )
        .await
        .unwrap_err();
    assert!(format!("{out}").contains("outside"), "{out}");

    // An empty folder is a project too; a folder over a limit is not read to the end.
    let empty = tempfile::tempdir().unwrap();
    let e = send_folder(
        &client,
        &Folder::read(empty.path()).unwrap(),
        "blank",
        |_| true,
    )
    .await
    .unwrap();
    assert_eq!(e["name"], "blank");
    assert!(Folder::read_at_most(l, 3, u64::MAX).unwrap().is_none());
    assert!(Folder::read_at_most(l, 10, 1000).unwrap().is_none());
    assert_eq!(
        Folder::read_at_most(l, 4, u64::MAX)
            .unwrap()
            .unwrap()
            .files(),
        4
    );

    // Asking again for a finished upload's project (its answer was lost) answers the same.
    client
        .call(
            "upload_project_parts",
            json!({ "upload": "again12345", "parts": [] }),
        )
        .await
        .unwrap();
    let first = client
        .call(
            "upload_project_finish",
            json!({ "upload": "again12345", "name": "twice" }),
        )
        .await
        .unwrap();
    let second = client
        .call(
            "upload_project_finish",
            json!({ "upload": "again12345", "name": "twice" }),
        )
        .await
        .unwrap();
    assert_eq!(first, second);
}

#[tokio::test]
async fn a_revoked_session_stops_receiving_events_on_a_stream_it_already_had() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let dir = tempfile::tempdir().unwrap();
    let tokens = Tokens::in_store(dir.path());
    let script = tokens.create("script").unwrap();
    let (_host, served) = ArkUi::new(app(dir.path()))
        .access(Access::Tokens(tokens.clone()))
        .serve_background("127.0.0.1:0")
        .await
        .unwrap();
    let port = served.port;
    let host = "ws-1.example.dev";
    let s = tokens
        .redeem(
            &tokens
                .link(
                    "Chrome on Mac",
                    Duration::from_secs(60),
                    Duration::from_secs(3600),
                )
                .unwrap(),
        )
        .unwrap()
        .unwrap();
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    stream
        .write_all(
            format!(
                "GET /events HTTP/1.1\r\nHost: {host}\r\nCookie: __Host-ark-session={}\r\n\r\n",
                s.token
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut buf = vec![0u8; 4096];
    let n = stream.read(&mut buf).await.unwrap();
    assert!(String::from_utf8_lossy(&buf[..n]).starts_with("HTTP/1.1 200"));

    // Signed out elsewhere; then something happens in the runtime.
    tokens.revoke("Chrome on Mac").unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let client = Client::new(&format!("http://127.0.0.1:{port}"), &script);
    resource_call(
        &client,
        "project",
        "spawn_agent",
        "",
        json!({ "spec": { "name": "reader", "role": "reads the notes", "tools": ["notes.*"] } }),
    )
    .await
    .unwrap();
    resource_call(
        &client,
        "project",
        "submit_task",
        "",
        json!({ "agent": "reader", "task": { "objective": "read the notes" } }),
    )
    .await
    .unwrap();
    let mut got = String::new();
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let n = stream.read(&mut buf).await.unwrap_or(0);
            if n == 0 {
                return;
            }
            got.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    })
    .await;
    assert!(closed.is_ok(), "the stream stayed open after the revoke");
    assert!(
        !got.contains("data:"),
        "an event reached a revoked session: {got}"
    );
}

/// A device with no link (a phone's Home Screen app) signs in with a code a signed-in one made,
/// for 90 days; the manifest names the app; a device added for notifications is tied to the
/// session that added it, and ends with it (ADR 0047).
#[tokio::test]
async fn a_typed_code_signs_a_phone_in_and_its_notifications_end_with_its_session() {
    let dir = tempfile::tempdir().unwrap();
    let tokens = Tokens::in_store(dir.path());
    let laptop = tokens.create("laptop").unwrap();
    let (host_, served) = ArkUi::new(app(dir.path()))
        .access(Access::Tokens(tokens.clone()))
        .accent("#b0418f")
        .serve_background("127.0.0.1:0")
        .await
        .unwrap();
    let port = served.port;
    let host = "ark.example.dev";
    let me = "https://ark.example.dev";

    // Signed out, the page learns a code signs in here, and how the app looks.
    let out = raw(
        port,
        format!("GET /session HTTP/1.1\r\nHost: {host}\r\n\r\n"),
    )
    .await;
    assert!(
        out.contains(r#""pairing":true"#) && out.contains(r##""accent":"#b0418f""##),
        "{out}"
    );
    let manifest = raw(
        port,
        format!("GET /manifest.webmanifest HTTP/1.1\r\nHost: {host}\r\n\r\n"),
    )
    .await;
    assert!(
        manifest.contains("Content-Type: application/manifest+json"),
        "{manifest}"
    );
    assert!(
        manifest.contains(r#""name":"Test""#) && manifest.contains(r#""display":"standalone""#)
    );
    let sw = raw(port, format!("GET /sw.js HTTP/1.1\r\nHost: {host}\r\n\r\n")).await;
    assert!(
        sw.starts_with("HTTP/1.1 200") && sw.contains("showNotification"),
        "the worker is public"
    );

    // Only a signed-in device makes a code.
    let none = raw(port, post("/pair", host, Some(me), "", "{}")).await;
    assert!(none.starts_with("HTTP/1.1 401"), "{none}");
    let auth = format!("Authorization: Bearer {laptop}\r\n");
    let made = raw(port, post("/pair", host, Some(me), &auth, "{}")).await;
    assert!(made.starts_with("HTTP/1.1 200"), "{made}");
    let made: Value = serde_json::from_str(made.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert!(
        made["qr_svg"]
            .as_str()
            .is_some_and(|svg| svg.contains("<svg") && svg.contains("#0f1115")),
        "the signed-in HTTPS origin gets a local QR"
    );
    let code = made["code"]
        .as_str()
        .unwrap()
        .to_lowercase()
        .replace('-', " ");

    // Typed on the phone: a session for 90 days, in a cookie.
    let body = json!({ "code": code, "name": "Phone Safari" }).to_string();
    let signed = raw(port, post("/session", host, Some(me), "", &body)).await;
    assert!(signed.starts_with("HTTP/1.1 200"), "{signed}");
    let (session, attrs) = set_cookie(&signed, "__Host-ark-session").expect("a cookie");
    let age: u64 = attrs
        .split("Max-Age=")
        .nth(1)
        .and_then(|a| a.split(';').next())
        .and_then(|a| a.trim().parse().ok())
        .unwrap_or_else(|| panic!("{attrs}"));
    assert!(age > 89 * 24 * 3600, "{age}");
    assert_eq!(
        tokens.check(session).as_deref(),
        Some("laptop › Phone Safari"),
        "named under who made the code"
    );

    // The phone asks for notifications: tied to its session, whatever the page says.
    let cookie = format!("Cookie: __Host-ark-session={session}\r\n");
    let add = json!({ "method": "push_add", "params": {
        "subscription": { "endpoint": "https://web.push.apple.com/abc", "keys": { "p256dh": "BCVx", "auth": "BTBZ" } },
        "name": "Phone Safari", "_by": "laptop" } })
    .to_string();
    let added = raw(port, post("/rpc", host, Some(me), &cookie, &add)).await;
    assert!(added.contains(r#""ok":true"#), "{added}");
    let listed = host_.call("push_devices", json!({})).await.unwrap();
    assert_eq!(listed[0]["name"], "Phone Safari");
    let record = ark_ui::push::Device::id("https://web.push.apple.com/abc");
    let ark = Ark::builder().store_root(dir.path()).build().unwrap();
    let d = ark
        .engine()
        .store
        .get_record(ark_ui::push::DEVICE, &record)
        .unwrap()
        .unwrap();
    assert_eq!(
        d["by"], "laptop › Phone Safari",
        "the server names who asked"
    );
    assert_eq!(d["origin"], me);

    // Settings › Devices lists it; Sign out ends its session and its notifications.
    let signed_in = host_.call("devices_signed_in", json!({})).await.unwrap();
    assert_eq!(signed_in[0]["device"], "Phone Safari");
    assert_eq!(signed_in[0]["notified"], true);
    host_
        .call(
            "device_sign_out",
            json!({ "name": "laptop › Phone Safari" }),
        )
        .await
        .unwrap();
    assert!(tokens.check(session).is_none());
    assert_eq!(
        host_.call("push_devices", json!({})).await.unwrap(),
        json!([])
    );

    // A device paired by another one goes when that one is revoked.
    let made = raw(port, post("/pair", host, Some(me), &auth, "{}")).await;
    let made: Value = serde_json::from_str(made.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    let s = tokens
        .redeem_as(made["code"].as_str().unwrap(), "Tablet Chrome")
        .unwrap()
        .unwrap();
    assert!(tokens.revoke("laptop").unwrap());
    assert!(
        tokens.check(&s.token).is_none(),
        "signed out with what paired it"
    );
}
