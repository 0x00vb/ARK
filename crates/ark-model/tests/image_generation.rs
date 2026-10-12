use std::sync::{Arc, Mutex};

use ark_model::{OpenAi, Provider};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[tokio::test]
async fn a_supported_openai_model_requests_base64_image_bytes() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(String::new()));
    let request = seen.clone();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let mut chunk = [0; 4096];
        loop {
            let n = socket.read(&mut chunk).await.unwrap();
            bytes.extend_from_slice(&chunk[..n]);
            let Some(head_end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
                continue;
            };
            let head = String::from_utf8_lossy(&bytes[..head_end]);
            let length: usize = head
                .lines()
                .find(|line| line.to_ascii_lowercase().starts_with("content-length:"))
                .and_then(|line| line.split_once(':'))
                .and_then(|(_, value)| value.trim().parse().ok())
                .unwrap_or(0);
            if bytes.len() >= head_end + 4 + length {
                break;
            }
        }
        *request.lock().unwrap() = String::from_utf8_lossy(&bytes).to_string();
        let body = r#"{"data":[{"b64_json":"cG5n"}]}"#;
        let reply = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(reply.as_bytes()).await.unwrap();
    });

    let provider = OpenAi::new(format!("http://{addr}/v1"), "gpt-5", "sk-test");
    assert!(provider.generates_images());
    let image = provider
        .generate_image("a small blue square")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(image.media_type, "image/png");
    assert_eq!(image.bytes, b"png");
    let request = seen.lock().unwrap();
    assert!(
        request.starts_with("POST /v1/images/generations "),
        "{request}"
    );
    assert!(
        request.contains(r#""response_format":"b64_json""#),
        "{request}"
    );
    assert!(
        request.contains(r#""prompt":"a small blue square""#),
        "{request}"
    );
    assert!(!request.contains("http://") && !request.contains("https://"));
}

#[tokio::test]
async fn an_unsupported_model_has_no_image_request() {
    let provider = OpenAi::new("http://127.0.0.1:9/v1", "gpt-4o", "sk-test");
    assert!(!provider.generates_images());
    assert!(provider.generate_image("anything").await.is_none());
}

#[tokio::test]
async fn an_oversized_image_response_is_rejected_before_its_body_is_read() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0; 4096];
        let _ = socket.read(&mut request).await.unwrap();
        socket
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 40000000\r\nconnection: close\r\n\r\n")
            .await
            .unwrap();
    });

    let provider = OpenAi::new(format!("http://{addr}/v1"), "gpt-5", "sk-test");
    let error = provider
        .generate_image("too large")
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("25 MB image limit"), "{error}");
}
