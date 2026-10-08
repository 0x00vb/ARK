//! A runtime served on a Unix socket (ADR 0052): the near end of an SSH tunnel to a person's
//! server. The socket and its folder admit only their owner, a socket left by a stopped process
//! is replaced, a second server on the same socket is refused, and saved tokens work the same
//! as over TCP, for raw requests and for `remote::Client`.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ark_interact::ark_orchestrate::Orchestrator;
use ark_interact::App;
use ark_sdk::prelude::ark_model::{Completion, MockProvider, Usage};
use ark_sdk::prelude::*;
use ark_ui::access::Tokens;
use ark_ui::remote::Client;
use ark_ui::{Access, ArkUi};
use serde_json::{json, Value};

fn app(dir: &Path) -> App {
    let provider: Arc<dyn ark_model::Provider> = Arc::new(
        MockProvider::new("m", vec![]).with_fallback(|_| Completion {
            content: "Here.".into(),
            tool_calls: vec![],
            usage: Usage::default(),
        }),
    );
    let ark = Ark::builder()
        .store_root(dir)
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

fn mode(p: &Path) -> u32 {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

/// A raw HTTP request over the socket and the whole response.
async fn raw(sock: &Path, req: String) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::UnixStream::connect(sock).await.unwrap();
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    out
}

fn rpc(auth: &str, method: &str) -> String {
    let body = json!({ "method": method, "params": {} }).to_string();
    format!(
        "POST /rpc HTTP/1.1\r\nHost: localhost\r\n{auth}Content-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

#[tokio::test]
async fn a_socket_admits_only_its_owner_and_a_saved_token() {
    let dir = tempfile::tempdir().unwrap();
    let tokens = Tokens::in_store(dir.path());
    let token = tokens.create("laptop").unwrap();
    let folder = dir.path().join("run/ark");
    let sock = folder.join("ark.sock");
    let (_host, served) = ArkUi::new(app(dir.path()))
        .access(Access::Tokens(tokens.clone()))
        .serve_background(&format!("unix:{}", sock.display()))
        .await
        .unwrap();
    assert_eq!(served.url, format!("unix:{}", sock.display()));
    assert_eq!(served.port, 0);
    assert_eq!(mode(&sock), 0o600, "the socket");
    assert_eq!(mode(&folder), 0o700, "its folder, made here");

    let health = raw(
        &sock,
        "GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n".into(),
    )
    .await;
    assert!(health.starts_with("HTTP/1.1 200") && health.contains(r#""ok":true"#));
    let none = raw(&sock, rpc("", "hello")).await;
    assert!(none.starts_with("HTTP/1.1 401"), "{none}");
    let ok = raw(
        &sock,
        rpc(&format!("Authorization: Bearer {token}\r\n"), "hello"),
    )
    .await;
    assert!(ok.contains(r#""ok":true"#), "{ok}");
    assert!(tokens.revoke("laptop").unwrap());
    let revoked = raw(
        &sock,
        rpc(&format!("Authorization: Bearer {token}\r\n"), "hello"),
    )
    .await;
    assert!(revoked.starts_with("HTTP/1.1 401"));

    // A second server on the same socket does not start, and leaves the first one serving.
    let other = tempfile::tempdir().unwrap();
    let refused = ArkUi::new(app(other.path()))
        .access(Access::Tokens(Tokens::in_store(other.path())))
        .serve_background(&format!("unix:{}", sock.display()))
        .await
        .err()
        .expect("a second server on a live socket");
    assert!(refused.to_string().contains("already serving"), "{refused}");
    let still = raw(
        &sock,
        "GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n".into(),
    )
    .await;
    assert!(still.starts_with("HTTP/1.1 200"));
}

#[tokio::test]
async fn a_socket_left_by_a_stopped_process_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("run");
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let sock = folder.join("ark.sock");
    // A listener that went away without removing its file: nothing answers there.
    drop(std::os::unix::net::UnixListener::bind(&sock).unwrap());
    assert!(sock.exists());
    let tokens = Tokens::in_store(dir.path());
    let (_host, _served) = ArkUi::new(app(dir.path()))
        .access(Access::Tokens(tokens))
        .serve_background(&format!("unix:{}", sock.display()))
        .await
        .unwrap();
    let health = raw(
        &sock,
        "GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n".into(),
    )
    .await;
    assert!(health.starts_with("HTTP/1.1 200"));
}

#[tokio::test]
async fn a_socket_is_refused_in_a_folder_others_can_write_or_without_saved_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let shared = dir.path().join("shared");
    std::fs::create_dir(&shared).unwrap();
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
    let e = ArkUi::new(app(dir.path()))
        .access(Access::Tokens(Tokens::in_store(dir.path())))
        .serve_background(&format!("unix:{}", shared.join("ark.sock").display()))
        .await
        .err()
        .expect("a folder others can write");
    assert!(e.to_string().contains("other users can write"), "{e}");
    assert!(!shared.join("ark.sock").exists());

    let other = tempfile::tempdir().unwrap();
    let e = ArkUi::new(app(other.path()))
        .serve_background(&format!(
            "unix:{}",
            other.path().join("s/ark.sock").display()
        ))
        .await
        .err()
        .expect("the one-time token on a socket");
    assert!(e.to_string().contains("saved access tokens"), "{e}");
}

#[tokio::test]
async fn the_client_reaches_a_runtime_on_a_socket() {
    let dir = tempfile::tempdir().unwrap();
    let tokens = Tokens::in_store(dir.path());
    let token = tokens.create("laptop").unwrap();
    let sock = dir.path().join("run/ark.sock");
    let url = format!("unix:{}", sock.display());
    let (_host, _served) = ArkUi::new(app(dir.path()))
        .access(Access::Tokens(tokens))
        .serve_background(&url)
        .await
        .unwrap();

    let refused = Client::new(&url, "ark_wrong").hello().await.unwrap_err();
    assert_eq!(
        refused.to_string(),
        format!("the ARK runtime at {url} refused the access token")
    );
    let client = Client::new(&url, &token);
    assert_eq!(client.url(), url);
    let hello = client.hello().await.unwrap();
    assert_eq!(hello["version"], env!("CARGO_PKG_VERSION"));

    // Events stream over the same socket: a turn's answer arrives.
    let mut events = client.events().await.unwrap();
    client.call("send", json!({ "text": "hi" })).await.unwrap();
    let answer = tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(v) = events.recv().await {
            if v.to_string().contains("Here.") {
                return v;
            }
        }
        Value::Null
    })
    .await
    .expect("an event with the answer");
    assert!(answer["seq"].is_number(), "{answer}");

    // Nothing there: the error names the socket like an address.
    let gone = dir.path().join("run/gone.sock");
    let e = Client::new(&format!("unix:{}", gone.display()), &token)
        .hello()
        .await
        .unwrap_err();
    assert!(
        e.to_string().starts_with(&format!(
            "cannot reach the ARK runtime at unix:{}",
            gone.display()
        )),
        "{e}"
    );
}
