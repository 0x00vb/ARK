//! The workspace over HTTP. It serves the same page as the desktop window, answers
//! `POST /rpc` with [`Host::call`], streams events on `GET /events` (server-sent events), and
//! answers `GET /health` (no token: `{ok, version}`, for supervisors and proxies).
//!
//! Every other request except the page's static files needs a token ([`Access`]). With the
//! one-time token (local use) the `Host` header must also name the loopback address, so
//! another local process or a web page cannot drive the agent. With saved tokens (a runtime
//! other machines connect to, ADR 0016) any `Host` is accepted: a reverse proxy in front
//! terminates TLS and passes its own name.
//!
//! A browser can also come in through a one-time link (ADR 0040): the page posts the code to
//! `POST /session` and gets a session in an HttpOnly cookie, so no secret stays in the address,
//! the history or the page. A request carried by that cookie must come from this page (its
//! `Origin`) unless it only reads. `GET /session` says whether the browser is in and where one
//! that is not should go ([`ArkUi::entry`](crate::ArkUi::entry)); `DELETE /session` ends it.
//! Every answer carries a Content-Security-Policy that lets the page run only its own scripts.

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

/// Headers on every answer. The page runs only scripts it serves (no inline script, nothing
/// from elsewhere), is never framed, and sends no referrer. Styles stay inline-able (the page
/// and its libraries set them); frames may show any web page (the workbench's browser).
/// `ipc:` is the desktop window's own channel when it shows a runtime elsewhere.
const PAGE_HEADERS: &str = "Content-Security-Policy: default-src 'self'; script-src 'self'; \
style-src 'self' 'unsafe-inline'; img-src 'self' data: blob: https:; media-src 'self' data: blob:; \
font-src 'self' data:; connect-src 'self' ipc: http://ipc.localhost; \
frame-src 'self' blob: data: https: http:; worker-src 'self' blob:; object-src 'none'; \
base-uri 'none'; form-action 'self'; frame-ancestors 'none'\r\n\
Referrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\n";

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
    entry: Option<String>,
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
    let entry = Arc::new(entry);
    let task = tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else {
                // Out of file descriptors or similar: pause instead of spinning.
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                continue;
            };
            let (host, assets, check, entry) =
                (host.clone(), assets.clone(), check.clone(), entry.clone());
            tokio::spawn(async move {
                let _ = handle(sock, host, assets, &check, port, &entry).await;
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
    fn cookie(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("cookie"))
            .flat_map(|(_, v)| v.split(';'))
            .find_map(|kv| {
                let (k, v) = kv.trim().split_once('=')?;
                (k == name).then_some(v)
            })
    }
    /// The request comes from a page at this address: its `Origin` names this `Host`, or,
    /// without an `Origin`, the browser says it is same-origin.
    fn same_origin(&self) -> bool {
        match (self.header("origin"), self.header("host")) {
            (Some(o), Some(h)) => o.split_once("://").is_some_and(|(_, rest)| rest == h),
            _ => self.header("sec-fetch-site") == Some("same-origin"),
        }
    }
    fn loopback(&self) -> bool {
        let host = self.header("host").unwrap_or("");
        let name = host.rsplit_once(':').map_or(host, |(n, _)| n);
        matches!(name, "127.0.0.1" | "localhost" | "[::1]")
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
    respond_with(sock, status, mime, "", body).await
}

/// [`respond`] with more header lines (each ending in `\r\n`).
async fn respond_with(
    sock: &mut TcpStream,
    status: &str,
    mime: &str,
    extra: &str,
    body: &[u8],
) -> anyhow::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n{PAGE_HEADERS}{extra}Connection: close\r\n\r\n",
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

/// The session cookie of a browser at this address. On a loopback address (cookies do not
/// keep ports apart) its name has the port; elsewhere the `__Host-` prefix pins it to this
/// host, over HTTPS only.
struct Cookie {
    name: String,
    secure: bool,
}

impl Cookie {
    fn of(req: &Request, port: u16) -> Cookie {
        if req.loopback() {
            Cookie {
                name: format!("ark-session-{port}"),
                secure: false,
            }
        } else {
            Cookie {
                name: "__Host-ark-session".into(),
                secure: true,
            }
        }
    }
    fn set(&self, value: &str, max_age_s: u64) -> String {
        format!(
            "Set-Cookie: {}={value}; Path=/; Max-Age={max_age_s}; HttpOnly; SameSite=Strict{}\r\n",
            self.name,
            if self.secure { "; Secure" } else { "" }
        )
    }
}

async fn handle(
    mut sock: TcpStream,
    host: Arc<Host>,
    assets: Arc<Assets>,
    check: &Check,
    port: u16,
    entry: &Option<String>,
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
    // Without a token, a browser's session cookie (saved tokens only).
    let cookie = Cookie::of(&req, port);
    let session = match check {
        Check::Tokens(tokens) if req.token().is_empty() => req
            .cookie(&cookie.name)
            .filter(|t| tokens.check(t).is_some())
            .map(str::to_string),
        _ => None,
    };
    let authed = authed || session.is_some();
    // A cookie rides along with whatever this browser sends here: a change must come from
    // this page.
    let changes = req.method != "GET" && req.method != "HEAD";
    if changes && (session.is_some() || req.path == "/session") && !req.same_origin() {
        return respond(&mut sock, "403 Forbidden", "text/plain", b"origin").await;
    }
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/session") => {
            let body = json!({ "signed_in": authed, "entry": entry });
            respond(
                &mut sock,
                "200 OK",
                "application/json",
                body.to_string().as_bytes(),
            )
            .await
        }
        ("POST", "/session") => {
            let Check::Tokens(tokens) = check else {
                return respond(&mut sock, "404 Not Found", "text/plain", b"not found").await;
            };
            let v: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
            let code = v["code"].as_str().unwrap_or("");
            match tokens.redeem(code)? {
                Some(s) => {
                    let left = s.expires_ms.saturating_sub(now_ms()) / 1000;
                    let body = json!({ "ok": true, "expires_ms": s.expires_ms });
                    respond_with(
                        &mut sock,
                        "200 OK",
                        "application/json",
                        &cookie.set(&s.token, left),
                        body.to_string().as_bytes(),
                    )
                    .await
                }
                None => {
                    let body = json!({ "ok": false, "error": "link", "entry": entry });
                    respond(
                        &mut sock,
                        "401 Unauthorized",
                        "application/json",
                        body.to_string().as_bytes(),
                    )
                    .await
                }
            }
        }
        ("DELETE", "/session") => {
            if let (Check::Tokens(tokens), Some(t)) = (check, &session) {
                tokens.end(t)?;
            }
            let body = json!({ "ok": true, "entry": entry });
            respond_with(
                &mut sock,
                "200 OK",
                "application/json",
                &cookie.set("", 0),
                body.to_string().as_bytes(),
            )
            .await
        }
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
            // A stream outlives the check that opened it: with saved tokens it checks again at
            // each ping and, at most once a second, before an event, and ends once the token or
            // session it came with is revoked or over.
            let credential = match check {
                Check::Tokens(tokens) => Some((
                    tokens.clone(),
                    session.clone().unwrap_or_else(|| req.token().to_string()),
                )),
                Check::OneTime(_) => None,
            };
            let still = |last: &mut std::time::Instant| -> bool {
                *last = std::time::Instant::now();
                credential
                    .as_ref()
                    .is_none_or(|(t, c)| t.check(c).is_some())
            };
            let mut checked = std::time::Instant::now();
            let mut rx = host.subscribe();
            sock.write_all(
                format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\n{PAGE_HEADERS}Connection: keep-alive\r\n\r\n: ready\n\n")
                    .as_bytes(),
            )
            .await?;
            let mut ping = tokio::time::interval(std::time::Duration::from_secs(15));
            loop {
                tokio::select! {
                    ev = rx.recv() => {
                        if checked.elapsed() >= std::time::Duration::from_secs(1) && !still(&mut checked) {
                            return Ok(());
                        }
                        match ev {
                            Ok(v) => sock.write_all(format!("data: {v}\n\n").as_bytes()).await?,
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                sock.write_all(b"data: {\"kind\":\"resync\"}\n\n").await?
                            }
                            Err(_) => return Ok(()),
                        }
                    }
                    _ = ping.tick() => {
                        if !still(&mut checked) {
                            return Ok(());
                        }
                        sock.write_all(b": ping\n\n").await?
                    }
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

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
