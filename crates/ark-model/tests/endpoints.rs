//! Named endpoints against a local OpenAI-compatible server: a registered name reaches it with
//! its own key and model ids, its models come from `GET /models`, a 402 is out of credit, and an
//! endpoint that cannot be reached shows as not ready without breaking the catalog.

use std::sync::{Arc, Mutex};

use ark_model::{endpoints, Endpoint, Message, ProviderError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Serve every request: `GET /models` lists two models, `POST /chat/completions` answers with
/// `chat` (status, body). Returns the base URL and each request's first line, auth and body.
async fn serve(chat: (u16, &'static str)) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 8192];
                let (head, body) = loop {
                    let n = sock.read(&mut tmp).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                        continue;
                    };
                    let head = String::from_utf8_lossy(&buf[..end]).to_string();
                    let len: usize = head
                        .to_lowercase()
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:").map(str::to_string))
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0);
                    while buf.len() < end + 4 + len {
                        let n = sock.read(&mut tmp).await.unwrap();
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    break (head, String::from_utf8_lossy(&buf[end + 4..]).to_string());
                };
                let first = head.lines().next().unwrap_or("").to_string();
                let auth = head
                    .lines()
                    .find(|l| l.to_lowercase().starts_with("authorization:"))
                    .unwrap_or("")
                    .to_string();
                log.lock()
                    .unwrap()
                    .push(format!("{first} | {auth} | {body}"));
                let (status, text) = if first.starts_with("GET /v1/models") {
                    (
                        200,
                        r#"{"object":"list","data":[{"id":"auto"},{"id":"claude"}]}"#,
                    )
                } else {
                    chat
                };
                let resp = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
                    text.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    (format!("http://{addr}/v1"), seen)
}

const ANSWER: &str = r#"{"choices":[{"message":{"role":"assistant","content":"hi"}}],"usage":{"prompt_tokens":5,"completion_tokens":1}}"#;

#[tokio::test]
async fn a_named_endpoint_is_a_provider_with_its_own_key_and_models() {
    let (base, seen) = serve((200, ANSWER)).await;
    endpoints::register(
        Endpoint::new("t-router", &base)
            .label("Router")
            .key("sk-test"),
    )
    .unwrap();

    // `claude` here is the endpoint's model id, not the Claude Code provider.
    let p = ark_model::build("t-router", Some("claude".into())).unwrap();
    assert_eq!(p.describe(), "t-router claude");
    let c = p
        .complete("sys", &[Message::user("hello")], &[])
        .await
        .unwrap();
    assert_eq!(c.content, "hi");
    let chat = seen.lock().unwrap().last().cloned().unwrap();
    assert!(chat.starts_with("POST /v1/chat/completions"), "{chat}");
    assert!(chat.contains("Bearer sk-test"), "{chat}");
    assert!(chat.contains(r#""model":"claude""#), "{chat}");

    let backends = ark_model::catalog().await;
    let b = backends.iter().find(|b| b.provider == "t-router").unwrap();
    assert!(b.ready, "{b:?}");
    assert_eq!(b.label, "Router");
    assert_eq!(b.models, ["auto", "claude"]);
    endpoints::unregister("t-router");
}

#[tokio::test]
async fn a_402_is_out_of_credit_with_the_providers_words() {
    let (base, _) = serve((
        402,
        r#"{"error":{"message":"Your balance is used up. Add credit to continue.","type":"insufficient_balance","code":"insufficient_balance"}}"#,
    ))
    .await;
    endpoints::register(Endpoint::new("t-broke", &base).key("sk-test")).unwrap();
    let p = ark_model::build("t-broke", Some("auto".into())).unwrap();
    let e = p
        .complete("sys", &[Message::user("hello")], &[])
        .await
        .unwrap_err();
    assert!(e.is_out_of_credit(), "{e:?}");
    assert!(
        !e.is_transient(),
        "more of the same call does not add credit"
    );
    assert_eq!(
        e.to_string(),
        "Your balance is used up. Add credit to continue."
    );
    match e {
        ProviderError::OutOfCredit { provider, .. } => assert_eq!(provider, "t-broke"),
        other => panic!("{other:?}"),
    }
    endpoints::unregister("t-broke");
}

#[tokio::test]
async fn an_unreachable_endpoint_shows_as_not_ready() {
    // A port nothing listens on.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    endpoints::register(Endpoint::new("t-gone", format!("http://127.0.0.1:{port}/v1")).key("k"))
        .unwrap();
    let backends = ark_model::catalog().await;
    let b = backends.iter().find(|b| b.provider == "t-gone").unwrap();
    assert!(!b.ready);
    assert_eq!(b.detail, "not reachable");
    assert!(
        backends.iter().any(|b| b.provider == "ollama"),
        "the rest is still listed"
    );
    endpoints::unregister("t-gone");
}

#[test]
fn a_model_is_required_and_a_missing_key_is_named() {
    endpoints::register(Endpoint::new("t-nokey", "http://127.0.0.1:9/v1")).unwrap();
    let e = ark_model::build("t-nokey", None).err().unwrap();
    assert!(e.to_string().contains("t-nokey:<model>"), "{e}");
    let e = ark_model::build("t-nokey", Some("m".into())).err().unwrap();
    assert!(
        matches!(&e, ProviderError::MissingCredential(v) if v == "T_NOKEY_API_KEY"),
        "{e:?}"
    );
    endpoints::unregister("t-nokey");
}
