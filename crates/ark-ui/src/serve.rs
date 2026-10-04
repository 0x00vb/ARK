//! The workspace over HTTP. It serves the same page as the desktop window, answers
//! `POST /rpc` with [`Host::call`], streams events on `GET /events` (server-sent events), and
//! answers `GET /health` (no token: `{ok, version}`, for supervisors and proxies).
//!
//! Every other request except the page's static files needs a token ([`Access`]). With the
//! one-time token (local use) the `Host` header must also name the loopback address, so
//! another local process or a web page cannot drive the agent. With saved tokens (a runtime
//! other machines connect to, ADR 0016) any `Host` is accepted: a reverse proxy in front
//! terminates TLS and passes its own name.

use std::sync::Arc;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::access::Tokens;
use crate::assets::Assets;
use crate::host::Host;

/// Who may call the server.
#[derive(Clone, Debug, Default)]
pub enum Access {
    /// A token made at start and printed in the URL; only requests addressed to the loopback
    /// host are answered. For a browser on the same machine.
    #[default]
    OneTime,
    /// The saved tokens ([`Tokens`]); any address and `Host`. For a runtime other machines
    /// connect to, behind a TLS proxy or on a private network.
    Tokens(Tokens),
}

/// A running server.
pub struct Served {
    /// The page's address: with the one-time token in it, or bare with saved tokens.
    pub url: String,
    /// The one-time token; empty with saved tokens.
    pub token: String,
    pub port: u16,
}

/// How long a client may take to send a request's head and body.
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

enum Check {
    OneTime(String),
    Tokens(Tokens),
}

pub(crate) async fn serve(
    host: Arc<Host>,
    assets: Arc<Assets>,
    addr: &str,
    access: Access,
) -> anyhow::Result<(Served, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    let port = local.port();
    let (served, check) = match access {
        Access::OneTime => {
            let token = uuid::Uuid::new_v4().simple().to_string();
            let served = Served {
                url: format!("http://127.0.0.1:{port}/?token={token}"),
                token: token.clone(),
                port,
            };
            (served, Check::OneTime(token))
        }
        Access::Tokens(tokens) => {
            let served = Served {
                url: format!("http://{local}/"),
                token: String::new(),
                port,
            };
            (served, Check::Tokens(tokens))
        }
    };
    let check = Arc::new(check);
    let task = tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else {
                // Out of file descriptors or similar: pause instead of spinning.
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                continue;
            };
            let (host, assets, check) = (host.clone(), assets.clone(), check.clone());
            tokio::spawn(async move {
                let _ = handle(sock, host, assets, &check, port).await;
            });
        }
    });
    Ok((served, task))
}

struct Request {
    method: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    fn query(&self, key: &str) -> Option<&str> {
        self.query.split('&').find_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            (k == key).then_some(v)
        })
    }
}

async fn read_request(sock: &mut TcpStream) -> anyhow::Result<Request> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        let n = sock.read(&mut chunk).await?;
        if n == 0 {
            anyhow::bail!("closed");
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() > 64 * 1024 {
            anyhow::bail!("header too large");
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split_whitespace();
    let method = first.next().unwrap_or("").to_string();
    let target = first.next().unwrap_or("/").to_string();
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target, String::new()),
    };
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    let len: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    if len > 32 * 1024 * 1024 {
        anyhow::bail!("body too large");
    }
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < len {
        let n = sock.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(len);
    Ok(Request {
        method,
        path,
        query,
        headers,
        body,
    })
}

async fn respond(
    sock: &mut TcpStream,
    status: &str,
    mime: &str,
    body: &[u8],
) -> anyhow::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    sock.write_all(head.as_bytes()).await?;
    sock.write_all(body).await?;
    Ok(())
}

impl Request {
    /// The token the client sent: `Authorization: Bearer`, `X-Ark-Token`, or `?token=` (an
    /// `EventSource` cannot set headers).
    fn token(&self) -> &str {
        self.header("authorization")
            .and_then(|v| v.strip_prefix("Bearer "))
            .or_else(|| self.header("x-ark-token"))
            .or_else(|| self.query("token"))
            .unwrap_or("")
    }
}

async fn handle(
    mut sock: TcpStream,
    host: Arc<Host>,
    assets: Arc<Assets>,
    check: &Check,
    port: u16,
) -> anyhow::Result<()> {
    let req = tokio::time::timeout(READ_TIMEOUT, read_request(&mut sock)).await??;
    let authed = match check {
        Check::OneTime(token) => {
            let host_ok = matches!(
                req.header("host"),
                Some(h) if h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}")
            );
            if !host_ok {
                return respond(
                    &mut sock,
                    "421 Misdirected Request",
                    "text/plain",
                    b"wrong host",
                )
                .await;
            }
            req.token() == token
        }
        Check::Tokens(tokens) => tokens.check(req.token()).is_some(),
    };
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/health") => {
            let body = json!({ "ok": true, "version": env!("CARGO_PKG_VERSION") });
            respond(
                &mut sock,
                "200 OK",
                "application/json",
                body.to_string().as_bytes(),
            )
            .await
        }
        ("POST", "/rpc") => {
            if !authed {
                return respond(&mut sock, "401 Unauthorized", "text/plain", b"token").await;
            }
            let v: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
            let method = v["method"].as_str().unwrap_or("").to_string();
            let params = v.get("params").cloned().unwrap_or(json!({}));
            let out = match host.call(&method, params).await {
                Ok(r) => json!({ "ok": true, "result": r }),
                Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
            };
            respond(
                &mut sock,
                "200 OK",
                "application/json",
                out.to_string().as_bytes(),
            )
            .await
        }
        ("GET", "/events") => {
            if !authed {
                return respond(&mut sock, "401 Unauthorized", "text/plain", b"token").await;
            }
            let mut rx = host.subscribe();
            sock.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nConnection: keep-alive\r\n\r\n: ready\n\n",
            )
            .await?;
            let mut ping = tokio::time::interval(std::time::Duration::from_secs(15));
            loop {
                tokio::select! {
                    ev = rx.recv() => match ev {
                        Ok(v) => sock.write_all(format!("data: {v}\n\n").as_bytes()).await?,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            sock.write_all(b"data: {\"kind\":\"resync\"}\n\n").await?
                        }
                        Err(_) => return Ok(()),
                    },
                    _ = ping.tick() => sock.write_all(b": ping\n\n").await?,
                }
            }
        }
        ("GET", path) => match assets.get(path) {
            Some((body, mime)) => respond(&mut sock, "200 OK", mime, &body).await,
            None => respond(&mut sock, "404 Not Found", "text/plain", b"not found").await,
        },
        _ => respond(&mut sock, "405 Method Not Allowed", "text/plain", b"").await,
    }
}
