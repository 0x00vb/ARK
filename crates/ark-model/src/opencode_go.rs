//! OpenCode Go's direct API for coding-agent conversations.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};

use crate::{
    Backend, BackendKind, Completion, HttpConfig, Message, OpenAi, Provider, ProviderError, Result,
    TextSink, ToolDef,
};

pub(crate) const NAME: &str = "opencode-go";
pub(crate) const KEY: &str = "OPENCODE_GO_API_KEY";
const BASE: &str = "https://opencode.ai/inference/go/openai/v1";
const MODELS_URL: &str = "https://opencode.ai/inference/go/v1/models";
const MODELS: &[&str] = &["glm-5.3-flash", "kimi-k2.7-code", "deepseek-v4.1-flash"];
const USER_AGENT_VALUE: &str = concat!("ark/", env!("CARGO_PKG_VERSION"));

/// A Go model. Bind it with [`Provider::for_conversation`] before calling it.
/// Credentials are read for each request, so saved-key rotation affects existing routers.
#[derive(Clone)]
pub struct OpenCodeGo {
    model: String,
    account: Option<crate::accounts::ModelAccount>,
    conversation: Option<HeaderValue>,
    client: reqwest::Client,
    http: HttpConfig,
    base: String,
    #[cfg(test)]
    source: Option<Arc<dyn crate::keys::KeySource>>,
}

impl OpenCodeGo {
    pub fn new(model: impl Into<String>) -> Result<Self> {
        let model = model.into();
        if !MODELS.contains(&model.as_str()) {
            return Err(failure(
                "This OpenCode Go model is not supported. Choose a listed model.",
            ));
        }
        let http = HttpConfig::default();
        let client = reqwest::Client::builder()
            .connect_timeout(http.connect_timeout)
            .read_timeout(http.idle_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            model,
            account: None,
            conversation: None,
            client,
            http,
            base: BASE.into(),
            #[cfg(test)]
            source: None,
        })
    }

    pub(crate) fn with_account(mut self, account: crate::accounts::ModelAccount) -> Self {
        self.account = Some(account);
        self
    }

    fn transport(&self) -> Result<(OpenAi, String)> {
        let conversation = self.conversation.clone().ok_or_else(|| {
            failure(
            "OpenCode Go needs a conversation. Use ask_session, ask, or harness_for_conversation.",
        )
        })?;
        #[cfg(test)]
        let key = match &self.source {
            Some(source) => source.key(KEY),
            None => crate::keys::api_key(KEY),
        };
        #[cfg(not(test))]
        let key = crate::keys::api_key(KEY);
        let key = match &self.account {
            Some(account) => account.key()?,
            None => key.ok_or_else(|| ProviderError::MissingCredential(KEY.into()))?,
        };
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static(USER_AGENT_VALUE));
        headers.insert("x-opencode-session", conversation);
        let transport = OpenAi::new(&self.base, &self.model, key.clone())
            .named(NAME)
            .with_raw_status()
            .with_transport(self.client.clone(), headers, self.http.clone());
        Ok((transport, key))
    }
}

fn redact(error: ProviderError, key: &str) -> ProviderError {
    let clean = |detail: String| detail.replace(key, "[redacted]");
    match error {
        ProviderError::Status {
            status,
            detail,
            retry_after,
            ..
        } => ProviderError::Status {
            provider: NAME.into(),
            status,
            detail: clean(detail),
            retry_after,
        },
        ProviderError::Api { detail, .. } => ProviderError::Api {
            provider: NAME.into(),
            detail: clean(detail),
        },
        ProviderError::Malformed { detail, .. } => ProviderError::Malformed {
            provider: NAME.into(),
            detail: clean(detail),
        },
        ProviderError::Overloaded { detail, .. } => ProviderError::Overloaded {
            provider: NAME.into(),
            detail: clean(detail),
        },
        ProviderError::Refused { detail, .. } => ProviderError::Refused {
            provider: NAME.into(),
            detail: clean(detail),
        },
        other => other,
    }
}

fn failure(detail: &str) -> ProviderError {
    ProviderError::Api {
        provider: NAME.into(),
        detail: detail.into(),
    }
}

#[async_trait]
impl Provider for OpenCodeGo {
    fn host_controls_tools(&self) -> bool {
        true
    }
    fn for_conversation(&self, conversation: &str) -> Result<Option<Arc<dyn Provider>>> {
        if conversation.is_empty() {
            return Err(failure(
                "OpenCode Go needs a nonempty conversation identifier.",
            ));
        }
        let mut digest = blake3::Hasher::new();
        digest.update(b"ark-conversation-v1\0");
        digest.update(conversation.as_bytes());
        let id = HeaderValue::from_str(digest.finalize().to_hex().as_str())
            .map_err(|_| failure("Could not encode the conversation identifier."))?;
        let mut bound = self.clone();
        bound.conversation = Some(id);
        Ok(Some(Arc::new(bound)))
    }

    fn describe(&self) -> String {
        format!("{NAME} {}", self.model)
    }
    fn model_id(&self) -> String {
        format!("{NAME}:{}", self.model)
    }

    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<Completion> {
        let (transport, key) = self.transport()?;
        transport
            .complete(system, messages, tools)
            .await
            .map_err(|error| redact(error, &key))
    }

    async fn complete_streaming(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
        on_text: TextSink<'_>,
    ) -> Result<Completion> {
        let (transport, key) = self.transport()?;
        transport
            .complete_streaming(system, messages, tools, on_text)
            .await
            .map_err(|error| redact(error, &key))
    }
}

static LISTED: Mutex<Option<(Instant, Vec<String>)>> = Mutex::new(None);

async fn models() -> std::result::Result<Vec<String>, String> {
    if let Some((at, models)) = LISTED.lock().unwrap().as_ref() {
        if at.elapsed() < Duration::from_secs(300) {
            return Ok(models.clone());
        }
    }
    let models = fetch_models(MODELS_URL).await?;
    *LISTED.lock().unwrap() = Some((Instant::now(), models.clone()));
    Ok(models)
}

async fn fetch_models(url: &str) -> std::result::Result<Vec<String>, String> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "Could not create the model client.".to_string())?;
    let response = client
        .get(url)
        .header(USER_AGENT, USER_AGENT_VALUE)
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .map_err(|_| "Models unavailable. Try again.".to_string())?;
    if !response.status().is_success() {
        return Err(format!("Models: HTTP {}", response.status().as_u16()));
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "Could not read the model list.".to_string())?;
        if bytes.len().saturating_add(chunk.len()) > 1024 * 1024 {
            return Err("Model list is too large.".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid model list.".to_string())?;
    let data = value["data"]
        .as_array()
        .ok_or_else(|| "Invalid model list.".to_string())?;
    Ok(MODELS
        .iter()
        .filter(|id| data.iter().any(|m| m["id"].as_str() == Some(**id)))
        .map(|id| (*id).to_string())
        .collect())
}

pub(crate) async fn backend() -> Backend {
    let present = crate::keys::has_api_key(KEY);
    let listing = if present {
        models().await
    } else {
        Ok(Vec::new())
    };
    let (ready, detail, models) = match listing {
        _ if !present => (false, "No key".into(), Vec::new()),
        Ok(models) if models.is_empty() => (false, "No supported models".into(), models),
        Ok(models) => {
            let environment = std::env::var(KEY).is_ok_and(|k| !k.trim().is_empty());
            (
                true,
                if environment {
                    "Environment key set"
                } else {
                    "Key saved"
                }
                .into(),
                models,
            )
        }
        Err(detail) => (false, detail, Vec::new()),
    };
    Backend {
        provider: NAME.into(),
        label: "OpenCode Go".into(),
        identity: None,
        kind: BackendKind::Api,
        ready,
        detail,
        fix: None,
        models,
        model_options: vec![],
        step: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn server(
        status: u16,
        body: &'static str,
        content_type: &'static str,
        location: Option<String>,
    ) -> (String, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let captured = seen.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let captured = captured.clone();
                let location = location.clone();
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    let mut buffer = [0; 4096];
                    loop {
                        let n = socket.read(&mut buffer).await.unwrap();
                        if n == 0 {
                            return;
                        }
                        bytes.extend_from_slice(&buffer[..n]);
                        assert!(bytes.len() <= 65536);
                        if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                            let head = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                            let len = head
                                .lines()
                                .find_map(|l| l.strip_prefix("content-length:"))
                                .map(|s| s.trim().parse::<usize>().unwrap())
                                .unwrap_or(0);
                            if bytes.len() >= end + 4 + len {
                                break;
                            }
                        }
                    }
                    captured
                        .lock()
                        .unwrap()
                        .push(String::from_utf8(bytes).unwrap());
                    let location = location
                        .map(|l| format!("location: {l}\r\n"))
                        .unwrap_or_default();
                    let reply = format!("HTTP/1.1 {status} X\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\nretry-after: 7\r\n{location}\r\n{body}", body.len());
                    socket.write_all(reply.as_bytes()).await.unwrap();
                });
            }
        });
        (format!("http://{address}"), seen, task)
    }

    struct Credentials(Mutex<Option<String>>);
    impl crate::keys::KeySource for Credentials {
        fn key(&self, _var: &str) -> Option<String> {
            self.0.lock().unwrap().clone()
        }
        fn has(&self, _var: &str) -> bool {
            self.0.lock().unwrap().is_some()
        }
    }

    fn provider(base: &str) -> OpenCodeGo {
        let mut provider = OpenCodeGo::new("glm-5.3-flash").unwrap();
        provider.base = format!("{base}/inference/go/openai/v1");
        provider.source = Some(Arc::new(Credentials(Mutex::new(Some(
            "private-test-key".into(),
        )))));
        provider
    }

    fn session(request: &str) -> String {
        request
            .lines()
            .find_map(|l| l.strip_prefix("x-opencode-session: "))
            .unwrap()
            .into()
    }

    const ANSWER: &str = r#"{"choices":[{"message":{"content":"done","tool_calls":[{"id":"t","function":{"name":"files__read","arguments":"{\"path\":\"src/main.rs\"}"}}]}}],"usage":{"prompt_tokens":10,"completion_tokens":3,"prompt_tokens_details":{"cached_tokens":5}}}"#;

    #[tokio::test]
    async fn immutable_views_keep_the_conversation_and_send_real_transport_headers() {
        let (base, seen, task) = server(200, ANSWER, "application/json", None).await;
        let original = provider(&base);
        let a = original
            .for_conversation("chat/private-person")
            .unwrap()
            .unwrap();
        let b = a.for_conversation("chat/B").unwrap().unwrap();
        let again = original
            .for_conversation("chat/private-person")
            .unwrap()
            .unwrap();
        let tools = [ToolDef {
            name: "files.read".into(),
            description: "read".into(),
            parameters: json!({"type":"object"}),
        }];
        let messages = [Message::user("read")];
        let (first, second) = tokio::join!(
            a.complete("code", &messages, &tools),
            b.complete("code", &messages, &tools)
        );
        let result = first.unwrap();
        second.unwrap();
        again.complete("code", &[], &[]).await.unwrap();
        assert_eq!(result.tool_calls[0].name, "files.read");
        assert_eq!(result.tool_calls[0].arguments["path"], "src/main.rs");
        assert_eq!(result.usage, crate::Usage::measured(10, 3).with_cached(5));
        assert_eq!(a.model_id(), "opencode-go:glm-5.3-flash");
        assert!(original
            .complete("code", &[], &[])
            .await
            .unwrap_err()
            .to_string()
            .contains("needs a conversation"));
        assert!(original.for_conversation("").is_err());
        let requests = seen.lock().unwrap();
        assert_eq!(requests.len(), 3);
        let ids: Vec<_> = requests.iter().map(|r| session(r)).collect();
        assert_eq!(ids.iter().filter(|id| **id == ids[2]).count(), 2);
        assert_eq!(
            ids.iter().collect::<std::collections::HashSet<_>>().len(),
            2
        );
        for request in requests.iter() {
            assert!(request.starts_with("POST /inference/go/openai/v1/chat/completions "));
            assert!(request.contains("authorization: Bearer private-test-key"));
            assert!(request.contains(&format!("user-agent: {USER_AGENT_VALUE}")));
            assert!(!request.contains("private-person"));
            assert_eq!(session(request).len(), 64);
        }
        task.abort();
    }

    #[tokio::test]
    async fn streaming_uses_the_same_scope_and_usage() {
        const SSE: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: {\"choices\":[],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":1}}\n\ndata: [DONE]\n\n";
        let (base, seen, task) = server(200, SSE, "text/event-stream", None).await;
        let p = provider(&base).for_conversation("chat/A").unwrap().unwrap();
        let text = Mutex::new(String::new());
        let sink = |part: &str| text.lock().unwrap().push_str(part);
        for _ in 0..2 {
            let c = p.complete_streaming("code", &[], &[], &sink).await.unwrap();
            assert_eq!(c.usage.total(), 5);
        }
        assert_eq!(*text.lock().unwrap(), "hihi");
        let requests = seen.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(session(&requests[0]), session(&requests[1]));
        assert!(requests[0].contains(r#""include_usage":true"#));
        task.abort();
    }

    #[tokio::test]
    async fn failures_preserve_status_and_never_follow_redirects_or_switch_billing_paths() {
        for status in [400, 401, 403, 402, 429, 503] {
            let (base, seen, task) = server(
                status,
                r#"{"message":"provider refusal"}"#,
                "application/json",
                None,
            )
            .await;
            let p = provider(&base).for_conversation("chat/A").unwrap().unwrap();
            let error = p.complete("code", &[], &[]).await.unwrap_err();
            assert!(error.to_string().contains("provider refusal"));
            assert!(
                matches!(error, ProviderError::Status { status: actual, retry_after: Some(delay), .. } if actual == status && delay == Duration::from_secs(7))
            );
            assert_eq!(error.is_transient(), matches!(status, 429 | 503));
            assert_eq!(seen.lock().unwrap().len(), 1);
            task.abort();
        }
        let (destination, forwarded, destination_task) =
            server(200, ANSWER, "application/json", None).await;
        let (base, seen, task) = server(307, "", "application/json", Some(destination)).await;
        let p = provider(&base).for_conversation("chat/A").unwrap().unwrap();
        assert!(p.complete("code", &[], &[]).await.is_err());
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert!(forwarded.lock().unwrap().is_empty());
        task.abort();
        destination_task.abort();
    }

    #[tokio::test]
    async fn echoed_credentials_are_redacted_on_both_completion_paths() {
        for body in [
            r#"{"message":"invalid private-test-key"}"#,
            "invalid private-test-key",
        ] {
            let (base, _, task) = server(401, body, "text/plain", None).await;
            let p = provider(&base).for_conversation("chat/A").unwrap().unwrap();
            let first = p.complete("code", &[], &[]).await.unwrap_err();
            let second = p
                .complete_streaming("code", &[], &[], &|_| {})
                .await
                .unwrap_err();
            for error in [first, second] {
                assert!(!error.to_string().contains("private-test-key"));
                assert!(error.to_string().contains("[redacted]"));
            }
            task.abort();
        }
    }

    #[tokio::test]
    async fn an_existing_bound_provider_reads_rotated_and_removed_credentials() {
        let (base, seen, task) = server(200, ANSWER, "application/json", None).await;
        let source = Arc::new(Credentials(Mutex::new(Some("first-test-key".into()))));
        let mut original = provider(&base);
        original.source = Some(source.clone());
        let bound = original.for_conversation("chat/A").unwrap().unwrap();
        bound.complete("code", &[], &[]).await.unwrap();
        *source.0.lock().unwrap() = Some("second-test-key".into());
        bound.complete("code", &[], &[]).await.unwrap();
        *source.0.lock().unwrap() = None;
        assert!(matches!(
            bound.complete("code", &[], &[]).await,
            Err(ProviderError::MissingCredential(_))
        ));
        let requests = seen.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].contains("Bearer first-test-key"));
        assert!(requests[1].contains("Bearer second-test-key"));
        assert_eq!(session(&requests[0]), session(&requests[1]));
        task.abort();
    }

    async fn silent_stream() -> (
        String,
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Receiver<std::io::Result<usize>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (ready_tx, ready) = tokio::sync::oneshot::channel();
        let (closed_tx, closed) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let n = socket.read(&mut buffer).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buffer[..n]);
                if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                    let len = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .map(|s| s.trim().parse::<usize>().unwrap())
                        .unwrap_or(0);
                    if bytes.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            socket.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n").await.unwrap();
            ready_tx.send(()).unwrap();
            let n = tokio::time::timeout(Duration::from_secs(2), socket.read(&mut buffer))
                .await
                .unwrap();
            closed_tx.send(n).unwrap();
        });
        (format!("http://{address}"), ready, closed)
    }

    #[tokio::test]
    async fn cancellation_and_idle_timeout_release_a_silent_go_stream() {
        for cancel in [true, false] {
            let (base, ready, closed) = silent_stream().await;
            let mut original = provider(&base);
            if !cancel {
                original.client = reqwest::Client::builder()
                    .read_timeout(Duration::from_millis(100))
                    .build()
                    .unwrap();
            }
            let bound = original.for_conversation("chat/A").unwrap().unwrap();
            let call =
                tokio::spawn(
                    async move { bound.complete_streaming("code", &[], &[], &|_| {}).await },
                );
            tokio::time::timeout(Duration::from_secs(2), ready)
                .await
                .unwrap()
                .unwrap();
            if cancel {
                call.abort();
                assert!(call.await.unwrap_err().is_cancelled());
            } else {
                let error = tokio::time::timeout(Duration::from_secs(2), call)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap_err();
                assert!(matches!(error, ProviderError::Http(error) if error.is_timeout()));
            }
            match tokio::time::timeout(Duration::from_secs(2), closed)
                .await
                .unwrap()
                .unwrap()
            {
                Ok(0) => {}
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
                other => panic!("cancelled connection remained readable: {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn malformed_streaming_tool_arguments_are_never_repaired() {
        const SSE: &str = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"files__read\",\"arguments\":\"{broken\"}}]}}]}\n\ndata: [DONE]\n\n";
        let (base, _, task) = server(200, SSE, "text/event-stream", None).await;
        let bound = provider(&base).for_conversation("chat/A").unwrap().unwrap();
        assert!(matches!(
            bound.complete_streaming("code", &[], &[], &|_| {}).await,
            Err(ProviderError::Malformed { provider, .. }) if provider == NAME
        ));
        task.abort();
    }

    #[tokio::test]
    async fn discovery_filters_protocols_and_cannot_prove_entitlement() {
        let (base, seen, task) = server(200, r#"{"data":[{"id":"gpt-6-luna"},{"id":"glm-5.3-flash"},{"id":"gemini-4-argon-contributor"},{"id":"unknown"}]}"#, "application/json", None).await;
        assert_eq!(
            fetch_models(&format!("{base}/inference/go/v1/models"))
                .await
                .unwrap(),
            ["glm-5.3-flash"]
        );
        assert!(!seen.lock().unwrap()[0]
            .to_lowercase()
            .contains("authorization:"));
        assert!(OpenCodeGo::new("gpt-6-luna").is_err());
        assert!(OpenCodeGo::new("unknown").is_err());
        task.abort();
    }
}
