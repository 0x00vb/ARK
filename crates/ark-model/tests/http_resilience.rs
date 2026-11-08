//! Providers against a local HTTP server that misbehaves: rate limits, silence, a stream that
//! stalls halfway, a bad request. Each failure must surface as a classified error within the
//! configured time, never as a hang.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ark_model::{HttpConfig, Message, Ollama, Provider, ProviderError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// What the server does with a request: write these chunks, pausing between them.
#[derive(Clone)]
enum Reply {
    Raw(Vec<(Duration, String)>),
    /// Accept, read the request, never answer.
    Silent,
}

/// Serve one request with `reply`; return the base URL and the captured request body.
async fn serve(reply: Reply) -> (String, Arc<Mutex<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let body = Arc::new(Mutex::new(String::new()));
    let seen = body.clone();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        // Read headers, then the body by content-length.
        loop {
            let n = sock.read(&mut tmp).await.unwrap();
            buf.extend_from_slice(&tmp[..n]);
            if let Some(end) = find(&buf, b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf[..end]).to_lowercase();
                let len: usize = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0);
                while buf.len() < end + 4 + len {
                    let n = sock.read(&mut tmp).await.unwrap();
                    buf.extend_from_slice(&tmp[..n]);
                }
                *seen.lock().unwrap() = String::from_utf8_lossy(&buf[end + 4..]).to_string();
                break;
            }
        }
        match reply {
            Reply::Silent => tokio::time::sleep(Duration::from_secs(30)).await,
            Reply::Raw(chunks) => {
                for (pause, chunk) in chunks {
                    tokio::time::sleep(pause).await;
                    if sock.write_all(chunk.as_bytes()).await.is_err() {
                        return;
                    }
                    let _ = sock.flush().await;
                }
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        }
    });
    (format!("http://{addr}"), body)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn http(idle_ms: u64) -> HttpConfig {
    HttpConfig {
        connect_timeout: Duration::from_secs(2),
        idle_timeout: Duration::from_millis(idle_ms),
        request_timeout: Duration::from_secs(5),
        max_output_tokens: Some(321),
    }
}

fn ollama(base: &str, idle_ms: u64) -> Ollama {
    Ollama::new("m").with_host(base).with_http(http(idle_ms))
}

fn response(status: &str, headers: &[&str], body: &str) -> String {
    let mut h = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n",
        body.len()
    );
    for x in headers {
        h.push_str(x);
        h.push_str("\r\n");
    }
    format!("{h}\r\n{body}")
}

#[tokio::test]
async fn a_rate_limit_carries_its_retry_after() {
    let (base, _) = serve(Reply::Raw(vec![(
        Duration::ZERO,
        response(
            "429 Too Many Requests",
            &["retry-after: 2"],
            r#"{"error":"slow down"}"#,
        ),
    )]))
    .await;
    let err = ollama(&base, 2000)
        .complete("s", &[Message::user("q")], &[])
        .await
        .unwrap_err();
    match &err {
        ProviderError::Status {
            status,
            retry_after,
            detail,
            ..
        } => {
            assert_eq!(*status, 429);
            assert_eq!(*retry_after, Some(Duration::from_secs(2)));
            assert_eq!(detail, "slow down");
        }
        other => panic!("{other}"),
    }
    assert!(err.is_transient());
}

#[tokio::test]
async fn a_bad_request_is_not_retried() {
    let (base, _) = serve(Reply::Raw(vec![(
        Duration::ZERO,
        response("400 Bad Request", &[], r#"{"error":"no such model"}"#),
    )]))
    .await;
    let err = ollama(&base, 2000)
        .complete("s", &[Message::user("q")], &[])
        .await
        .unwrap_err();
    assert!(!err.is_transient(), "{err}");
}

#[tokio::test]
async fn a_silent_server_times_out_instead_of_hanging() {
    let (base, _) = serve(Reply::Silent).await;
    let started = Instant::now();
    let err = ollama(&base, 300)
        .complete_streaming("s", &[Message::user("q")], &[], &|_: &str| {})
        .await
        .unwrap_err();
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert!(err.is_transient(), "a timeout may succeed on retry: {err}");
}

#[tokio::test]
async fn a_stream_that_stalls_halfway_fails_after_the_text_it_sent() {
    let head = "HTTP/1.1 200 OK\r\ncontent-type: application/x-ndjson\r\ntransfer-encoding: chunked\r\n\r\n";
    let line = r#"{"message":{"content":"partial "},"done":false}"#.to_string() + "\n";
    let chunk = format!("{:x}\r\n{line}\r\n", line.len());
    let (base, _) = serve(Reply::Raw(vec![
        (Duration::ZERO, head.to_string()),
        (Duration::ZERO, chunk),
    ]))
    .await;
    let seen = Mutex::new(String::new());
    let started = Instant::now();
    let err = ollama(&base, 300)
        .complete_streaming("s", &[Message::user("q")], &[], &|t: &str| {
            seen.lock().unwrap().push_str(t)
        })
        .await
        .unwrap_err();
    assert_eq!(
        *seen.lock().unwrap(),
        "partial ",
        "the text before the stall arrived"
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(err.is_transient(), "{err}");
}

#[tokio::test]
async fn the_output_cap_is_sent() {
    let ok = r#"{"message":{"content":"hi"},"done":true,"prompt_eval_count":3,"eval_count":1}"#;
    let (base, body) = serve(Reply::Raw(vec![(
        Duration::ZERO,
        response("200 OK", &[], ok),
    )]))
    .await;
    let c = ollama(&base, 2000)
        .complete("s", &[Message::user("q")], &[])
        .await
        .unwrap();
    assert_eq!(c.content, "hi");
    let sent: serde_json::Value = serde_json::from_str(&body.lock().unwrap()).unwrap();
    assert_eq!(sent["options"]["num_predict"], 321);
}

#[tokio::test]
async fn a_retired_model_says_so_and_leaves_the_catalog() {
    let (gone, _) = serve(Reply::Raw(vec![(
        Duration::ZERO,
        response("410 Gone", &[], r#"{"error":"model retired"}"#),
    )]))
    .await;
    let err = Ollama::new("glm-4.6")
        .with_host(&gone)
        .with_http(http(2000))
        .complete("s", &[Message::user("q")], &[])
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "glm-4.6 was retired by ollama. Pick another model."
    );
    assert!(!err.is_transient());

    let tags =
        r#"{"models":[{"name":"glm-4.6:latest"},{"name":"glm-4.6:q4"},{"name":"qwen3:4b"}]}"#;
    let (host, _) = serve(Reply::Raw(vec![(
        Duration::ZERO,
        response("200 OK", &[], tags),
    )]))
    .await;
    std::env::set_var("OLLAMA_HOST", &host);
    let listed = ark_model::catalog()
        .await
        .into_iter()
        .find(|b| b.provider == "ollama")
        .unwrap();
    assert_eq!(
        listed.models,
        vec!["glm-4.6:q4".to_string(), "qwen3:4b".to_string()]
    );
}
