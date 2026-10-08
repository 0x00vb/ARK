//! A client of a runtime served elsewhere (ADR 0016): the methods a window calls
//! (`Host::call` in `ark-ui`) and the numbered events it follows, over HTTP and server-sent
//! events. Any front-end uses it: `ark-ui` opens a window on it, `ark-cli` a terminal
//! (ADR 0024), scripts and other services call it directly.

use futures::StreamExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;

/// Calls a served runtime's methods and follows its events.
#[derive(Clone)]
pub struct Client {
    /// The address as given, for `url()` and errors.
    addr: String,
    /// Where requests go: `addr`, or `http://localhost` on a Unix socket.
    base: String,
    token: String,
    http: reqwest::Client,
}

impl Client {
    /// A runtime at `url` (`https://ark.example.com`), reached with an access token.
    /// `unix:/path/to.sock` reaches a runtime serving on a Unix socket (ADR 0052), such as the
    /// near end of an SSH tunnel.
    pub fn new(url: &str, token: &str) -> Client {
        let addr = url.trim_end_matches('/').to_string();
        let builder =
            reqwest::Client::builder().connect_timeout(std::time::Duration::from_secs(10));
        let (base, builder) = match addr.strip_prefix("unix:") {
            Some(path) => unix_socket(builder, path),
            None => (addr.clone(), builder),
        };
        Client {
            addr,
            base,
            token: token.to_string(),
            http: builder.build().unwrap_or_default(),
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
