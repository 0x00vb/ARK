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
//!
//! The address is `host:port` for TCP, or `unix:/path/to.sock` for a Unix socket (ADR 0052): a
//! runtime on a person's server that their laptop reaches through an SSH tunnel. The socket is
//! mode 0600 in a folder only its owner changes, so other users of that computer cannot reach
//! it. It serves with saved tokens only.

use std::sync::Arc;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;

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
    /// `unix:/path/to.sock` on a Unix socket.
    pub url: String,
    /// The one-time token; empty with saved tokens.
    pub token: String,
    /// The TCP port; 0 on a Unix socket.
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

// Only explicitly registered trusted consumer HTML permits this same origin to frame it.
// This replaces the complete policy, never intersects it with a second CSP header.
const FRAME_ASSET_HEADERS: &str =
    "Content-Security-Policy: default-src 'self'; script-src 'self'; \
style-src 'self' 'unsafe-inline'; img-src 'self' data: blob: https:; media-src 'self' data: blob:; \
font-src 'self' data:; connect-src 'self' ipc: http://ipc.localhost; \
frame-src 'self' blob: data: https: http:; worker-src 'self' blob:; object-src 'none'; \
base-uri 'none'; form-action 'self'; frame-ancestors 'self'\r\n\
Referrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\n";

/// How long a typed device code is good for, and how long the session it becomes lasts: a
/// phone stays signed in for a season; revoking its name ends it (ADR 0047).
const PAIR_VALID_SECS: u64 = 10 * 60;
const PAIRED_SESSION_DAYS: u64 = 90;

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
    let listener = Listener::bind(addr, &access).await?;
    let port = listener.port();
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
                url: listener.url(),
                token: String::new(),
                port,
            };
            // A device that asked for notifications lasts as long as what added it.
            host.notifier.use_tokens(tokens.clone());
            (served, Check::Tokens(tokens))
        }
    };
    host.resume_client_mutations().await?;
    let check = Arc::new(check);
    let entry = Arc::new(entry);
    let task = tokio::spawn(async move {
        let mut mutations = tokio::time::interval(std::time::Duration::from_millis(250));
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let Ok(sock) = accepted else {
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
                _ = mutations.tick() => {
                    let _ = host.resume_client_mutations().await;
                }
            }
        }
    });
    Ok((served, task))
}

/// A connection, over TCP or a Unix socket.
trait Conn: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Conn for T {}

/// Where the server listens. The accept task owns it: a socket file goes when the task does.
enum Listener {
    Tcp(TcpListener),
    #[cfg(unix)]
    Unix(tokio::net::UnixListener, SocketFile),
}

impl Listener {
    async fn bind(addr: &str, access: &Access) -> anyhow::Result<Listener> {
        let Some(path) = addr.strip_prefix("unix:") else {
            return Ok(Listener::Tcp(TcpListener::bind(addr).await?));
        };
        if matches!(access, Access::OneTime) {
            anyhow::bail!("a Unix socket serves with saved access tokens only");
        }
        #[cfg(unix)]
        {
            let (listener, file) = bind_socket(std::path::Path::new(path)).await?;
            Ok(Listener::Unix(listener, file))
        }
        #[cfg(not(unix))]
        {
            anyhow::bail!("{path}: this system has no Unix sockets; serve on host:port")
        }
    }

    fn port(&self) -> u16 {
        match self {
            Listener::Tcp(l) => l.local_addr().map(|a| a.port()).unwrap_or(0),
            #[cfg(unix)]
            Listener::Unix(..) => 0,
        }
    }

    fn url(&self) -> String {
        match self {
            Listener::Tcp(l) => match l.local_addr() {
                Ok(a) => format!("http://{a}/"),
                Err(_) => String::new(),
            },
            #[cfg(unix)]
            Listener::Unix(_, file) => format!("unix:{}", file.0.display()),
        }
    }

    async fn accept(&self) -> std::io::Result<Box<dyn Conn>> {
        match self {
            Listener::Tcp(l) => Ok(Box::new(l.accept().await?.0)),
            #[cfg(unix)]
            Listener::Unix(l, _) => Ok(Box::new(l.accept().await?.0)),
        }
    }
}

/// The socket file of a server on a Unix socket, removed when the server stops.
#[cfg(unix)]
struct SocketFile(std::path::PathBuf);

#[cfg(unix)]
impl Drop for SocketFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Listen on the Unix socket at `path`: mode 0600, in a folder only this user changes (made
/// 0700 when missing). A socket left by a process that is gone is replaced; one that answers
/// means a server is already there, and this one does not start.
#[cfg(unix)]
async fn bind_socket(
    path: &std::path::Path,
) -> anyhow::Result<(tokio::net::UnixListener, SocketFile)> {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    if !path.is_absolute() {
        anyhow::bail!("{}: a Unix socket needs an absolute path", path.display());
    }
    let folder = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{}: no folder", path.display()))?;
    ark_os::fs::private_dir(folder)?;
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if !meta.file_type().is_socket() {
            anyhow::bail!("{}: a file that is not a socket is there", path.display());
        }
        match tokio::net::UnixStream::connect(path).await {
            Ok(_) => anyhow::bail!("{}: already serving", path.display()),
            // Nobody listens: a process that stopped without removing it.
            Err(_) => std::fs::remove_file(path)?,
        }
    }
    let listener = tokio::net::UnixListener::bind(path)
        .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    let file = SocketFile(path.to_path_buf());
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok((listener, file))
}

struct Request {
    method: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    /// A native client's credential. Native protocol routes accept only an Authorization header,
    /// never a URL token, browser cookie or compatibility header (ADR 0058).
    fn bearer_token(&self) -> &str {
        self.header("authorization")
            .and_then(|value| value.strip_prefix("Bearer "))
            .unwrap_or("")
    }

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

async fn read_request<S: AsyncRead + Unpin>(sock: &mut S) -> anyhow::Result<Request> {
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

async fn respond<S: AsyncWrite + Unpin>(
    sock: &mut S,
    status: &str,
    mime: &str,
    body: &[u8],
) -> anyhow::Result<()> {
    respond_with(sock, status, mime, "", body).await
}

/// [`respond`] with more header lines (each ending in `\r\n`).
async fn respond_with<S: AsyncWrite + Unpin>(
    sock: &mut S,
    status: &str,
    mime: &str,
    extra: &str,
    body: &[u8],
) -> anyhow::Result<()> {
    respond_with_policy(sock, status, mime, extra, body, PAGE_HEADERS).await
}

async fn respond_with_policy<S: AsyncWrite + Unpin>(
    sock: &mut S,
    status: &str,
    mime: &str,
    extra: &str,
    body: &[u8],
    policy: &str,
) -> anyhow::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n{policy}{extra}Connection: close\r\n\r\n",
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

async fn handle<S: AsyncRead + AsyncWrite + Unpin>(
    mut sock: S,
    host: Arc<Host>,
    assets: Arc<Assets>,
    check: &Check,
    port: u16,
    entry: &Option<String>,
) -> anyhow::Result<()> {
    let req = tokio::time::timeout(READ_TIMEOUT, read_request(&mut sock)).await??;
    let token_authed = match check {
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
    let client_credential = req.bearer_token();
    let client_authed =
        matches!(check, Check::Tokens(tokens) if tokens.check(client_credential).is_some());
    let authed = token_authed || session.is_some();
    if req.path.starts_with("/client/v1/") && !client_authed && !client_credential.is_empty() {
        host.forget_client_terminal_routes(&client_device_id(client_credential));
    }
    // A cookie rides along with whatever this browser sends here: a change must come from
    // this page.
    let changes = req.method != "GET" && req.method != "HEAD";
    if changes && (session.is_some() || req.path == "/session") && !req.same_origin() {
        return respond(&mut sock, "403 Forbidden", "text/plain", b"origin").await;
    }
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/client/bootstrap") => {
            let body =
                serde_json::to_vec(&ark_protocol::Bootstrap::current(env!("CARGO_PKG_VERSION")))?;
            respond(&mut sock, "200 OK", "application/json", &body).await
        }
        ("POST", "/client/v1/pair") => {
            let Check::Tokens(tokens) = check else {
                return client_error(
                    &mut sock,
                    "pairing_unavailable",
                    "This computer is not available for device pairing.",
                    "404 Not Found",
                )
                .await;
            };
            let request: ark_protocol::PairRequest = match serde_json::from_slice(&req.body) {
                Ok(value) => value,
                Err(_) => {
                    return client_error(
                        &mut sock,
                        "invalid_request",
                        "Check the pairing address and code.",
                        "400 Bad Request",
                    )
                    .await
                }
            };
            let Some(paired) = tokens.redeem_as(&request.code, &request.device_name)? else {
                return client_error(
                    &mut sock,
                    "pairing_code",
                    "That pairing code is no longer valid. Make a new one in ARK.",
                    "401 Unauthorized",
                )
                .await;
            };
            let digest = Sha256::digest(paired.token.as_bytes());
            let device_id = format!("{:x}", digest);
            let epoch = client_mutation_epoch(&paired.token);
            let expires_at = time::OffsetDateTime::from_unix_timestamp_nanos(
                i128::from(paired.expires_ms) * 1_000_000,
            )?
            .format(&time::format_description::well_known::Rfc3339)?;
            let body = serde_json::to_vec(&ark_protocol::PairResponse {
                device_id,
                credential: paired.token,
                expires_at,
                mutation_epoch: epoch,
                next_mutation_counter: ark_protocol::DecimalString::new("1").expect("one"),
            })?;
            respond(&mut sock, "200 OK", "application/json", &body).await
        }
        ("GET", "/client/v1/hello") => {
            if !client_authed {
                return client_error(
                    &mut sock,
                    "signed_out",
                    "This device is no longer connected. Pair it again.",
                    "401 Unauthorized",
                )
                .await;
            }
            let device_id = client_device_id(client_credential);
            let epoch = client_mutation_epoch(client_credential);
            let counter = host.client_mutation_counter(&device_id, &epoch)?;
            let hello = ark_protocol::Hello {
                protocol_major: ark_protocol::CURRENT_MAJOR,
                protocol_minor: ark_protocol::CURRENT_MINOR,
                minimum_client_minor: 0,
                capabilities: vec![
                    "home",
                    "chats",
                    "approvals",
                    "results",
                    "chat.send",
                    "chat.create",
                    "project.terminal",
                ]
                .into_iter()
                .map(str::to_string)
                .collect(),
                limits: ark_protocol::Limits {
                    page_size: 50,
                    event_bytes: 65_536,
                    upload_part_bytes: 5 * 1024 * 1024,
                    upload_bytes: 50 * 1024 * 1024,
                    terminal_write_bytes: ark_protocol::TERMINAL_WRITE_BYTES,
                    terminal_replay_bytes: ark_protocol::TERMINAL_REPLAY_BYTES,
                },
                mutation_epoch: epoch,
                next_mutation_counter: ark_protocol::DecimalString::new(
                    counter.saturating_add(1).to_string(),
                )
                .expect("u64 is decimal"),
            };
            respond(
                &mut sock,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&hello)?,
            )
            .await
        }
        ("GET", "/client/v1/home") => {
            if !client_authed {
                return client_error(
                    &mut sock,
                    "signed_out",
                    "This device is no longer connected. Pair it again.",
                    "401 Unauthorized",
                )
                .await;
            }
            let body = crate::client::home(&host).await?;
            respond(
                &mut sock,
                "200 OK",
                "application/json",
                body.to_string().as_bytes(),
            )
            .await
        }
        ("GET", "/client/v1/chats") => {
            if !client_authed {
                return client_error(
                    &mut sock,
                    "signed_out",
                    "This device is no longer connected. Pair it again.",
                    "401 Unauthorized",
                )
                .await;
            }
            let body = crate::client::chats(&host).await?;
            respond(
                &mut sock,
                "200 OK",
                "application/json",
                body.to_string().as_bytes(),
            )
            .await
        }
        ("GET", "/client/v1/projects") => {
            if !client_authed {
                return client_error(
                    &mut sock,
                    "signed_out",
                    "This device is no longer connected. Pair it again.",
                    "401 Unauthorized",
                )
                .await;
            }
            let body = crate::client::projects(&host).await?;
            respond(
                &mut sock,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&body)?,
            )
            .await
        }
        ("GET", "/client/v1/computers") => {
            if !client_authed {
                return client_error(
                    &mut sock,
                    "signed_out",
                    "This device is no longer connected. Pair it again.",
                    "401 Unauthorized",
                )
                .await;
            }
            let body = crate::client::computers(&host).await?;
            respond(
                &mut sock,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&body)?,
            )
            .await
        }
        ("POST", "/client/v1/terminals/open") => {
            if !client_authed {
                return client_error(
                    &mut sock,
                    "signed_out",
                    "This device is no longer connected. Pair it again.",
                    "401 Unauthorized",
                )
                .await;
            }
            let request: ark_protocol::TerminalOpenRequest = match serde_json::from_slice(&req.body)
            {
                Ok(value) => value,
                Err(_) => {
                    return client_error(
                        &mut sock,
                        "invalid_request",
                        "Check the Terminal details and try again.",
                        "400 Bad Request",
                    )
                    .await
                }
            };
            let owner = client_device_id(client_credential);
            match crate::client::terminal_open(&host, &owner, &request).await {
                Ok(body) => {
                    respond(
                        &mut sock,
                        "200 OK",
                        "application/json",
                        &serde_json::to_vec(&body)?,
                    )
                    .await
                }
                Err(_) => {
                    client_error(
                        &mut sock,
                        "terminal_unavailable",
                        "This Terminal is not available. Check the project and computer.",
                        "409 Conflict",
                    )
                    .await
                }
            }
        }
        ("POST", "/client/v1/terminals/read") => {
            if !client_authed {
                return client_error(
                    &mut sock,
                    "signed_out",
                    "This device is no longer connected. Pair it again.",
                    "401 Unauthorized",
                )
                .await;
            }
            let request: ark_protocol::TerminalReadRequest = match serde_json::from_slice(&req.body)
            {
                Ok(value) => value,
                Err(_) => {
                    return client_error(
                        &mut sock,
                        "invalid_request",
                        "Check the Terminal details and try again.",
                        "400 Bad Request",
                    )
                    .await
                }
            };
            let owner = client_device_id(client_credential);
            match crate::client::terminal_read(&host, &owner, &request).await {
                Ok(body) => {
                    respond(
                        &mut sock,
                        "200 OK",
                        "application/json",
                        &serde_json::to_vec(&body)?,
                    )
                    .await
                }
                Err(_) => {
                    client_error(
                        &mut sock,
                        "terminal_stale",
                        "This Terminal changed. Reopen it.",
                        "409 Conflict",
                    )
                    .await
                }
            }
        }
        ("POST", "/client/v1/terminals/write") => {
            if !client_authed {
                return client_error(
                    &mut sock,
                    "signed_out",
                    "This device is no longer connected. Pair it again.",
                    "401 Unauthorized",
                )
                .await;
            }
            let request: ark_protocol::TerminalWriteRequest =
                match serde_json::from_slice(&req.body) {
                    Ok(value) => value,
                    Err(_) => {
                        return client_error(
                            &mut sock,
                            "invalid_request",
                            "Check the Terminal input and try again.",
                            "400 Bad Request",
                        )
                        .await
                    }
                };
            let owner = client_device_id(client_credential);
            match crate::client::terminal_write(&host, &owner, &request).await {
                Ok(body) => {
                    respond(
                        &mut sock,
                        "200 OK",
                        "application/json",
                        &serde_json::to_vec(&body)?,
                    )
                    .await
                }
                Err(_) => {
                    client_error(
                        &mut sock,
                        "terminal_write_refused",
                        "This Terminal did not accept that input. Reopen it before typing again.",
                        "409 Conflict",
                    )
                    .await
                }
            }
        }
        ("POST", "/client/v1/terminals/resize") => {
            if !client_authed {
                return client_error(
                    &mut sock,
                    "signed_out",
                    "This device is no longer connected. Pair it again.",
                    "401 Unauthorized",
                )
                .await;
            }
            let request: ark_protocol::TerminalResizeRequest =
                match serde_json::from_slice(&req.body) {
                    Ok(value) => value,
                    Err(_) => {
                        return client_error(
                            &mut sock,
                            "invalid_request",
                            "Check the Terminal size and try again.",
                            "400 Bad Request",
                        )
                        .await
                    }
                };
            let owner = client_device_id(client_credential);
            match crate::client::terminal_resize(&host, &owner, &request).await {
                Ok(body) => {
                    respond(
                        &mut sock,
                        "200 OK",
                        "application/json",
                        &serde_json::to_vec(&body)?,
                    )
                    .await
                }
                Err(_) => {
                    client_error(
                        &mut sock,
                        "terminal_stale",
                        "This Terminal changed. Reopen it.",
                        "409 Conflict",
                    )
                    .await
                }
            }
        }
        ("POST", "/client/v1/terminals/end") => {
            if !client_authed {
                return client_error(
                    &mut sock,
                    "signed_out",
                    "This device is no longer connected. Pair it again.",
                    "401 Unauthorized",
                )
                .await;
            }
            let request: ark_protocol::TerminalEndRequest = match serde_json::from_slice(&req.body)
            {
                Ok(value) => value,
                Err(_) => {
                    return client_error(
                        &mut sock,
                        "invalid_request",
                        "Check the Terminal details and try again.",
                        "400 Bad Request",
                    )
                    .await
                }
            };
            let owner = client_device_id(client_credential);
            match crate::client::terminal_end(&host, &owner, &request).await {
                Ok(body) => {
                    respond(
                        &mut sock,
                        "200 OK",
                        "application/json",
                        &serde_json::to_vec(&body)?,
                    )
                    .await
                }
                Err(_) => {
                    client_error(
                        &mut sock,
                        "terminal_stale",
                        "This Terminal already ended.",
                        "409 Conflict",
                    )
                    .await
                }
            }
        }
        ("POST", "/client/v1/chats") => {
            if !client_authed {
                return client_error(
                    &mut sock,
                    "signed_out",
                    "This device is no longer connected. Pair it again.",
                    "401 Unauthorized",
                )
                .await;
            }
            let input: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
            let project = input["project"].as_str().unwrap_or("");
            let body = crate::client::create_chat(&host, project).await?;
            respond(
                &mut sock,
                "200 OK",
                "application/json",
                body.to_string().as_bytes(),
            )
            .await
        }
        ("GET", path) if path.starts_with("/client/v1/chats/") => {
            if !client_authed {
                return client_error(
                    &mut sock,
                    "signed_out",
                    "This device is no longer connected. Pair it again.",
                    "401 Unauthorized",
                )
                .await;
            }
            let id = path.trim_start_matches("/client/v1/chats/");
            let body = crate::client::conversation(&host, id).await?;
            respond(
                &mut sock,
                "200 OK",
                "application/json",
                body.to_string().as_bytes(),
            )
            .await
        }
        ("POST", "/client/v1/mutations") => {
            if !client_authed {
                return client_error(
                    &mut sock,
                    "signed_out",
                    "This device is no longer connected. Pair it again.",
                    "401 Unauthorized",
                )
                .await;
            }
            let input: ark_protocol::MutationRequest = match serde_json::from_slice(&req.body) {
                Ok(value) => value,
                Err(_) => {
                    return client_error(
                        &mut sock,
                        "invalid_request",
                        "That change could not be read. Update this device and try again.",
                        "400 Bad Request",
                    )
                    .await
                }
            };
            if input.epoch != client_mutation_epoch(client_credential) {
                return client_error(
                    &mut sock,
                    "mutation_epoch",
                    "This connection changed. Refresh before trying again.",
                    "409 Conflict",
                )
                .await;
            }
            let encoded_input = serde_json::to_vec(&input.input)?;
            let digest = format!("{:x}", Sha256::digest(encoded_input));
            if input.input_digest != digest {
                return client_error(
                    &mut sock,
                    "input_digest",
                    "That change was not accepted. Refresh and try again.",
                    "400 Bad Request",
                )
                .await;
            }
            let device_id = client_device_id(client_credential);
            match host.apply_client_mutation(&device_id, &input).await {
                Ok(receipt) => {
                    respond(
                        &mut sock,
                        "200 OK",
                        "application/json",
                        &serde_json::to_vec(&receipt)?,
                    )
                    .await
                }
                Err(crate::host::ClientMutationError::CounterConflict) => {
                    client_error(
                        &mut sock,
                        "counter_conflict",
                        "This change conflicts with an earlier one. Refresh before trying again.",
                        "409 Conflict",
                    )
                    .await
                }
                Err(crate::host::ClientMutationError::CounterGap) => {
                    client_error(
                        &mut sock,
                        "counter_gap",
                        "A previous change is still settling. Refresh before trying again.",
                        "409 Conflict",
                    )
                    .await
                }
                Err(crate::host::ClientMutationError::AlreadyApplied) => {
                    client_error(
                        &mut sock,
                        "already_applied",
                        "That change was already accepted. Refreshing will show its result.",
                        "409 Conflict",
                    )
                    .await
                }
                Err(crate::host::ClientMutationError::Unsupported) => {
                    client_error(
                        &mut sock,
                        "unsupported_operation",
                        "Update this device before using that action.",
                        "400 Bad Request",
                    )
                    .await
                }
                Err(crate::host::ClientMutationError::InvalidCounter) => {
                    client_error(
                        &mut sock,
                        "invalid_counter",
                        "Pair this device again before sending more changes.",
                        "400 Bad Request",
                    )
                    .await
                }
                Err(crate::host::ClientMutationError::Store(error)) => Err(error),
            }
        }
        ("DELETE", "/client/v1/session") => {
            if !client_authed {
                return client_error(
                    &mut sock,
                    "signed_out",
                    "This device is no longer connected.",
                    "401 Unauthorized",
                )
                .await;
            }
            let Check::Tokens(tokens) = check else {
                return client_error(
                    &mut sock,
                    "sign_out_unavailable",
                    "This connection cannot be removed here.",
                    "400 Bad Request",
                )
                .await;
            };
            host.forget_client_terminal_routes(&client_device_id(client_credential));
            tokens.end(client_credential)?;
            respond(&mut sock, "204 No Content", "application/json", b"").await
        }
        ("GET", "/client/v1/events") => {
            if !client_authed {
                return client_error(
                    &mut sock,
                    "signed_out",
                    "This device is no longer connected. Pair it again.",
                    "401 Unauthorized",
                )
                .await;
            }
            let after = req
                .query("after")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            let credential = match check {
                Check::Tokens(tokens) => (tokens.clone(), client_credential.to_string()),
                Check::OneTime(_) => unreachable!("native clients require saved tokens"),
            };
            let owner = client_device_id(client_credential);
            let mut receiver = host.subscribe();
            sock.write_all(
                format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\n{PAGE_HEADERS}Connection: keep-alive\r\n\r\n: ready\n\n")
                    .as_bytes(),
            )
            .await?;
            match host.client_event_replay(after) {
                Some(events) => {
                    for event in events {
                        if credential.0.check(&credential.1).is_none() {
                            return Ok(());
                        }
                        write_client_event(&mut sock, &host, &owner, &event).await?;
                    }
                }
                None => {
                    if credential.0.check(&credential.1).is_none() {
                        return Ok(());
                    }
                    write_resync_event(&mut sock, after).await?
                }
            }
            let mut credential_check = tokio::time::interval(std::time::Duration::from_millis(250));
            let mut ping = tokio::time::interval(std::time::Duration::from_secs(15));
            loop {
                tokio::select! {
                    event = receiver.recv() => match event {
                        Ok(event) => {
                            if credential.0.check(&credential.1).is_none() {
                                return Ok(());
                            }
                            write_client_event(&mut sock, &host, &owner, &event).await?
                        },
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            if credential.0.check(&credential.1).is_none() {
                                return Ok(());
                            }
                            write_resync_event(&mut sock, after).await?
                        }
                        Err(_) => return Ok(()),
                    },
                    _ = credential_check.tick() => {
                        if credential.0.check(&credential.1).is_none() {
                            return Ok(());
                        }
                    }
                    _ = ping.tick() => {
                        sock.write_all(b": ping\n\n").await?;
                    }
                }
            }
        }
        ("GET", "/session") => {
            // `pairing`: a device without a link signs in here with a code (ADR 0047).
            let pairing = matches!(check, Check::Tokens(_));
            // The app's accent, as its manifest says it: the sign-in page looks like the app.
            let accent = host.manifest("")["theme_color"].clone();
            let body = json!({ "signed_in": authed, "entry": entry, "pairing": pairing, "accent": accent });
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
            // A device that types a code says what it is (`Phone Safari`).
            match tokens.redeem_as(code, v["name"].as_str().unwrap_or(""))? {
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
        // A code for another device, from a signed-in one: typed on a phone's Home Screen app,
        // which cannot open a link (ADR 0047).
        ("POST", "/pair") => {
            let (Check::Tokens(tokens), true) = (check, authed) else {
                return respond(&mut sock, "401 Unauthorized", "text/plain", b"token").await;
            };
            // The new device is named under the one asking and never outlives it: revoking
            // this device signs that one out too (ADR 0040, 0047).
            let credential = session.clone().unwrap_or_else(|| req.token().to_string());
            let Some(me) = tokens.info(&credential) else {
                return respond(&mut sock, "401 Unauthorized", "text/plain", b"token").await;
            };
            let valid = std::time::Duration::from_secs(PAIR_VALID_SECS);
            let lasts = std::time::Duration::from_secs(PAIRED_SESSION_DAYS * 24 * 3600);
            let code = tokens.pair_code(&me.name, valid, lasts, me.expires_ms)?;
            let body = json!({ "code": code, "expires_ms": now_ms() + PAIR_VALID_SECS * 1000 });
            respond(
                &mut sock,
                "200 OK",
                "application/json",
                body.to_string().as_bytes(),
            )
            .await
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
            let mut params = v.get("params").cloned().unwrap_or(json!({}));
            // A device for notifications is tied to the token or session that added it, and
            // names the address its page has (ADR 0047). Never taken from the page.
            if method == "push_add" && params.is_object() {
                let by = match check {
                    Check::Tokens(tokens) => session
                        .as_deref()
                        .or(Some(req.token()))
                        .and_then(|t| tokens.check(t)),
                    Check::OneTime(_) => Some("local".to_string()),
                };
                params["_by"] = json!(by.unwrap_or_default());
                params["_origin"] = json!(req.header("origin").unwrap_or(""));
            }
            // Only the app's own window goes to another runtime's page; a browser here would
            // move that window instead of itself (it shows the item's words).
            let out = match if method == "attention_go" {
                Err(anyhow::anyhow!(
                    "only this computer's window opens another runtime's page"
                ))
            } else {
                host.call(&method, params).await
            } {
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
        // What a phone's Home Screen needs to add the page as an app; public, like the page.
        ("GET", "/manifest.webmanifest") => {
            let body = host.manifest(assets.title()).to_string();
            respond(
                &mut sock,
                "200 OK",
                "application/manifest+json",
                body.as_bytes(),
            )
            .await
        }
        ("GET", path) => match assets.get(path) {
            Some((body, mime)) if assets.is_frame_asset(path) => {
                respond_with_policy(&mut sock, "200 OK", mime, "", &body, FRAME_ASSET_HEADERS).await
            }
            Some((body, mime)) => respond(&mut sock, "200 OK", mime, &body).await,
            None => respond(&mut sock, "404 Not Found", "text/plain", b"not found").await,
        },
        _ => respond(&mut sock, "405 Method Not Allowed", "text/plain", b"").await,
    }
}

async fn client_error<S: AsyncWrite + Unpin>(
    sock: &mut S,
    code: &str,
    display: &str,
    status: &str,
) -> anyhow::Result<()> {
    let body = ark_protocol::ErrorEnvelope {
        code: code.into(),
        display: display.into(),
        retry_after_seconds: None,
    };
    respond(
        sock,
        status,
        "application/json",
        &serde_json::to_vec(&body)?,
    )
    .await
}

fn client_mutation_epoch(credential: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ark-native-mutation-epoch\0");
    digest.update(credential.as_bytes());
    format!("{:x}", digest.finalize())
}

fn client_device_id(credential: &str) -> String {
    format!("{:x}", Sha256::digest(credential.as_bytes()))
}

fn client_event(
    host: &Host,
    owner: &str,
    value: &Value,
) -> anyhow::Result<Option<ark_protocol::EventEnvelope>> {
    let sequence = value["seq"].as_u64().unwrap_or(0);
    let mut event_type = value["kind"].as_str().unwrap_or("changed").to_string();
    let mut affects = vec!["home".to_string()];
    let terminal = if event_type == "term" {
        let reference: crate::resource::ResourceRef =
            match serde_json::from_value(value["ref"].clone()) {
                Ok(reference) => reference,
                Err(_) => return Ok(None),
            };
        let crate::resource::ResourceAuthority::Project { route } = reference.authority else {
            return Ok(None);
        };
        let ended_owner = value["_client_owner"].as_str();
        if ended_owner != Some(owner) && !host.client_owns_terminal(owner, &route) {
            return Ok(None);
        }
        event_type = "terminal".into();
        affects.clear();
        affects.push(format!("terminal:{route}"));
        Some(ark_protocol::TerminalEvent {
            handle: route,
            generation: value["generation"].as_str().unwrap_or("").to_string(),
            cursor: value["cursor"].as_u64().map(|cursor| {
                ark_protocol::DecimalString::new(cursor.to_string()).expect("u64 is decimal")
            }),
            data: value["data"].as_str().map(str::to_string),
            gap: value["gap"].as_bool().unwrap_or(false),
            ended: value["exit"].as_bool().unwrap_or(false),
        })
    } else {
        None
    };
    if matches!(
        event_type.as_str(),
        "sessions" | "turn_started" | "turn_finished"
    ) {
        affects.push("chats".into());
    }
    if let Some(session) = value["session"].as_str() {
        affects.push(format!("chat:{session}"));
    }
    Ok(Some(ark_protocol::EventEnvelope {
        id: format!("event-{sequence}"),
        sequence: ark_protocol::DecimalString::new(sequence.to_string())
            .map_err(anyhow::Error::msg)?,
        event_type,
        affects,
        terminal,
    }))
}

async fn write_client_event<S: AsyncWrite + Unpin>(
    sock: &mut S,
    host: &Host,
    owner: &str,
    value: &Value,
) -> anyhow::Result<()> {
    let Some(event) = client_event(host, owner, value)? else {
        return Ok(());
    };
    let body = serde_json::to_string(&event)?;
    sock.write_all(format!("id: {}\ndata: {body}\n\n", event.id).as_bytes())
        .await?;
    Ok(())
}

async fn write_resync_event<S: AsyncWrite + Unpin>(sock: &mut S, after: u64) -> anyhow::Result<()> {
    let event = ark_protocol::EventEnvelope {
        id: format!("resync-{after}"),
        sequence: ark_protocol::DecimalString::new(after.to_string())
            .map_err(anyhow::Error::msg)?,
        event_type: "resync".into(),
        affects: vec!["home".into(), "chats".into(), "openChat".into()],
        terminal: None,
    };
    let body = serde_json::to_string(&event)?;
    sock.write_all(format!("id: {}\ndata: {body}\n\n", event.id).as_bytes())
        .await?;
    Ok(())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
