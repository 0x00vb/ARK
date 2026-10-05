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

struct Notes;

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

/// Reads the notes once, then answers "Read.".
fn model() -> Arc<MockProvider> {
    Arc::new(MockProvider::new("m", vec![]).with_fallback(|req| {
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
    }))
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
    client
        .call(
            "spawn_agent",
            json!({ "spec": { "name": "reader", "role": "reads the notes", "tools": ["notes.*"] } }),
        )
        .await
        .unwrap();
    let r = client
        .call(
            "submit_task",
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
        let t = later.call("task", json!({ "task": task })).await.unwrap();
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
    let read = client
        .call("file_read", json!({ "path": "chapters/draft/one.md" }))
        .await
        .unwrap();
    assert_eq!(read["text"], "It was a dark night.");
    let root = client.call("files", json!({ "path": "" })).await.unwrap();
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
            "upload_parts",
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
            "upload_parts",
            json!({ "upload": "again12345", "parts": [] }),
        )
        .await
        .unwrap();
    let first = client
        .call(
            "upload_project",
            json!({ "upload": "again12345", "name": "twice" }),
        )
        .await
        .unwrap();
    let second = client
        .call(
            "upload_project",
            json!({ "upload": "again12345", "name": "twice" }),
        )
        .await
        .unwrap();
    assert_eq!(first, second);
}
