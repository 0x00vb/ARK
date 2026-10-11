//! Transport client for a served ARK runtime.
//!
//! This crate depends on wire and transport libraries, never on ARK interaction or UI code.

use futures::StreamExt;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::mpsc;

/// A stable error returned by the versioned client protocol.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtocolError {
    pub status: u16,
    pub code: String,
    pub display: String,
    pub retry_after_seconds: Option<u64>,
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.display)
    }
}

impl std::error::Error for ProtocolError {}

/// Calls a served runtime's methods and follows its events.
#[derive(Clone)]
pub struct Client {
    /// The address as given, for `url()` and errors.
    addr: String,
    /// Where requests go: `addr`, or `http://localhost` on a Unix socket.
    base: String,
    token: String,
    http: reqwest::Client,
    terminal_writes: Arc<tokio::sync::Mutex<()>>,
}

impl Client {
    /// A runtime at `url` (`https://ark.example.com`), reached with an access token.
    /// `unix:/path/to.sock` reaches a runtime serving on a Unix socket (ADR 0052), such as the
    /// near end of an SSH tunnel.
    pub fn new(url: &str, token: &str) -> Client {
        let addr = url.trim_end_matches('/').to_string();
        let builder = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none());
        let (base, builder) = match addr.strip_prefix("unix:") {
            Some(path) => unix_socket(builder, path),
            None => (addr.clone(), builder),
        };
        Client {
            addr,
            base,
            token: token.to_string(),
            http: builder.build().unwrap_or_default(),
            terminal_writes: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// The runtime's address as given, without a trailing slash.
    pub fn url(&self) -> &str {
        &self.addr
    }

    /// One protocol call: what `ark.call(method, params)` does in the page.
    pub async fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let r = self
            .http
            .post(format!("{}/rpc", self.base))
            .bearer_auth(&self.token)
            .json(&json!({ "method": method, "params": params }))
            .send()
            .await
            .map_err(|e| self.unreachable(e))?;
        self.status(r.status().as_u16())?;
        let v: Value = r.json().await?;
        if v["ok"].as_bool() == Some(true) {
            Ok(v["result"].clone())
        } else {
            anyhow::bail!("{}", v["error"].as_str().unwrap_or("the call failed"))
        }
    }

    /// The runtime's `hello`: its version, current session and running turn. Checks that the
    /// address and the token work.
    pub async fn hello(&self) -> anyhow::Result<Value> {
        self.call("hello", json!({})).await
    }

    /// The version-independent contract offered at this address.
    pub async fn bootstrap(&self) -> anyhow::Result<ark_protocol::Bootstrap> {
        self.client_get("/client/bootstrap", false).await
    }

    pub async fn client_hello(&self) -> anyhow::Result<ark_protocol::Hello> {
        self.client_get("/client/v1/hello", true).await
    }

    pub async fn projects(&self) -> anyhow::Result<ark_protocol::ProjectsSnapshot> {
        self.client_get("/client/v1/projects", true).await
    }

    pub async fn computers(&self) -> anyhow::Result<ark_protocol::ComputersSnapshot> {
        self.client_get("/client/v1/computers", true).await
    }

    pub async fn terminal_open(
        &self,
        request: &ark_protocol::TerminalOpenRequest,
    ) -> anyhow::Result<ark_protocol::TerminalOpenResponse> {
        self.client_post("/client/v1/terminals/open", request).await
    }

    pub async fn terminal_read(
        &self,
        request: &ark_protocol::TerminalReadRequest,
    ) -> anyhow::Result<ark_protocol::TerminalReadResponse> {
        self.client_post("/client/v1/terminals/read", request).await
    }

    /// One bounded, non-retrying Terminal write. Clones of a client still send one at a time.
    pub async fn terminal_write(
        &self,
        request: &ark_protocol::TerminalWriteRequest,
    ) -> anyhow::Result<ark_protocol::TerminalOperationResponse> {
        let _serial = self.terminal_writes.lock().await;
        self.client_post("/client/v1/terminals/write", request)
            .await
    }

    pub async fn terminal_resize(
        &self,
        request: &ark_protocol::TerminalResizeRequest,
    ) -> anyhow::Result<ark_protocol::TerminalOperationResponse> {
        self.client_post("/client/v1/terminals/resize", request)
            .await
    }

    pub async fn terminal_end(
        &self,
        request: &ark_protocol::TerminalEndRequest,
    ) -> anyhow::Result<ark_protocol::TerminalOperationResponse> {
        self.client_post("/client/v1/terminals/end", request).await
    }

    /// One bounded, resumable v1 event stream. Reconnect policy belongs to the consumer.
    pub async fn client_events(
        &self,
        after: Option<&ark_protocol::DecimalString>,
    ) -> anyhow::Result<mpsc::Receiver<anyhow::Result<ark_protocol::EventEnvelope>>> {
        let mut request = self
            .http
            .get(format!("{}/client/v1/events", self.base))
            .bearer_auth(&self.token);
        if let Some(after) = after {
            request = request.query(&[("after", after.as_str())]);
        }
        let response = request
            .send()
            .await
            .map_err(|error| self.unreachable(error))?;
        if !response.status().is_success() {
            return Err(self.protocol_error(response).await);
        }
        let mut body = response.bytes_stream();
        let mut lines = SseLines::default();
        let mut first = Vec::new();
        loop {
            let chunk = body.next().await.ok_or_else(|| {
                anyhow::anyhow!("the ARK runtime at {} closed the event stream", self.addr)
            })??;
            let (ready, events) = lines.push(&chunk);
            first.extend(events);
            if ready {
                break;
            }
        }
        let (sender, receiver) = mpsc::channel(64);
        for value in first {
            let event = serde_json::from_value(value).map_err(anyhow::Error::from);
            if sender.send(event).await.is_err() {
                return Ok(receiver);
            }
        }
        tokio::spawn(async move {
            while let Some(chunk) = body.next().await {
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        let _ = sender.send(Err(error.into())).await;
                        return;
                    }
                };
                let (_, events) = lines.push(&chunk);
                for value in events {
                    let event = serde_json::from_value(value).map_err(anyhow::Error::from);
                    if sender.send(event).await.is_err() {
                        return;
                    }
                }
            }
        });
        Ok(receiver)
    }

    async fn client_get<T: DeserializeOwned>(&self, path: &str, auth: bool) -> anyhow::Result<T> {
        let request = self.http.get(format!("{}{path}", self.base));
        let request = if auth {
            request.bearer_auth(&self.token)
        } else {
            request
        };
        self.client_response(
            request
                .send()
                .await
                .map_err(|error| self.unreachable(error))?,
        )
        .await
    }

    async fn client_post<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> anyhow::Result<T> {
        let response = self
            .http
            .post(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await
            .map_err(|error| self.unreachable(error))?;
        self.client_response(response).await
    }

    async fn client_response<T: DeserializeOwned>(
        &self,
        response: reqwest::Response,
    ) -> anyhow::Result<T> {
        if response.status().is_success() {
            return Ok(response.json().await?);
        }
        Err(self.protocol_error(response).await)
    }

    async fn protocol_error(&self, response: reqwest::Response) -> anyhow::Error {
        let status = response.status().as_u16();
        match response.json::<ark_protocol::ErrorEnvelope>().await {
            Ok(error) => ProtocolError {
                status,
                code: error.code,
                display: error.display,
                retry_after_seconds: error.retry_after_seconds,
            }
            .into(),
            Err(_) => ProtocolError {
                status,
                code: "http_error".into(),
                display: format!("the ARK runtime at {} answered HTTP {status}", self.addr),
                retry_after_seconds: None,
            }
            .into(),
        }
    }

    /// Every event the runtime sends from now on, in order, each with its `seq`. Returns once
    /// the runtime has started sending, so nothing that happens after this call is missed. The
    /// channel closes when the connection does; a `{"kind":"resync"}` event means some were
    /// dropped because this client read too slowly.
    pub async fn events(&self) -> anyhow::Result<mpsc::UnboundedReceiver<Value>> {
        self.stream(false).await
    }

    /// [`events`](Self::events), with `{"kind": "alive"}` for each of the runtime's pings (every
    /// 15 s): a follower that hears nothing for longer knows the stream is gone.
    pub async fn events_alive(&self) -> anyhow::Result<mpsc::UnboundedReceiver<Value>> {
        self.stream(true).await
    }

    async fn stream(&self, alive: bool) -> anyhow::Result<mpsc::UnboundedReceiver<Value>> {
        let r = self
            .http
            .get(format!("{}/events", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| self.unreachable(e))?;
        self.status(r.status().as_u16())?;
        let mut body = r.bytes_stream();
        let mut lines = SseLines::default();
        // The runtime writes a comment as soon as it subscribed; wait for it.
        let mut first = Vec::new();
        loop {
            let chunk = body.next().await.ok_or_else(|| {
                anyhow::anyhow!("the ARK runtime at {} closed the event stream", self.addr)
            })??;
            let (ready, events) = lines.push(&chunk);
            first.extend(events);
            if ready {
                break;
            }
        }
        let (tx, rx) = mpsc::unbounded_channel();
        for v in first {
            let _ = tx.send(v);
        }
        tokio::spawn(async move {
            while let Some(Ok(chunk)) = body.next().await {
                let (ping, events) = lines.push(&chunk);
                if alive && ping && tx.send(json!({ "kind": "alive" })).is_err() {
                    return;
                }
                for v in events {
                    if tx.send(v).is_err() {
                        return;
                    }
                }
            }
        });
        Ok(rx)
    }

    /// The page's address with the token, for a window or a browser to open. The page keeps
    /// the token for the tab and removes it from the address bar. A runtime on a Unix socket
    /// has no address a browser opens.
    pub fn page_url(&self) -> String {
        format!("{}/?token={}", self.base, self.token)
    }

    fn unreachable(&self, e: reqwest::Error) -> anyhow::Error {
        anyhow::anyhow!("cannot reach the ARK runtime at {}: {e}", self.addr)
    }

    fn status(&self, code: u16) -> anyhow::Result<()> {
        match code {
            200 => Ok(()),
            401 => anyhow::bail!("the ARK runtime at {} refused the access token", self.addr),
            s => anyhow::bail!("the ARK runtime at {} answered HTTP {s}", self.addr),
        }
    }
}

/// Requests go to the Unix socket at `path`; the URL's host only fills the `Host` header.
#[cfg(unix)]
fn unix_socket(builder: reqwest::ClientBuilder, path: &str) -> (String, reqwest::ClientBuilder) {
    let builder = builder.unix_socket(std::path::Path::new(path));
    ("http://localhost".to_string(), builder)
}

/// No Unix sockets on this system: requests go to a name that never resolves (`.invalid`), so
/// each fails like an address that does not answer, and the error names the socket.
#[cfg(not(unix))]
fn unix_socket(builder: reqwest::ClientBuilder, _path: &str) -> (String, reqwest::ClientBuilder) {
    ("http://unix-socket.invalid".to_string(), builder)
}

/// Server-sent events cut into JSON values. Chunks may split a line anywhere.
#[derive(Default)]
struct SseLines {
    buf: Vec<u8>,
}

impl SseLines {
    /// Whether a comment line (the stream's `: ready`) was seen, and the events completed by
    /// `chunk`.
    fn push(&mut self, chunk: &[u8]) -> (bool, Vec<Value>) {
        self.buf.extend_from_slice(chunk);
        let (mut comment, mut out) = (false, Vec::new());
        while let Some(i) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=i).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\r', '\n']);
            if line.starts_with(':') {
                comment = true;
            } else if let Some(data) = line.strip_prefix("data:") {
                if let Ok(v) = serde_json::from_str(data.trim_start()) {
                    out.push(v);
                }
            }
        }
        (comment, out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_split_across_chunks_are_put_back_together() {
        let mut s = SseLines::default();
        assert_eq!(s.push(b": ready\n\n"), (true, vec![]));
        let (_, a) = s.push(b"data: {\"kind\":\"te");
        assert!(a.is_empty());
        let (comment, b) = s.push(b"xt\",\"seq\":3}\n\ndata: {\"seq\":4}\n\n: ping\n\n");
        assert!(comment, "a ping is a comment too");
        assert_eq!(b, vec![json!({"kind":"text","seq":3}), json!({"seq":4})]);
    }
}
