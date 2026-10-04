//! A whole connection against a provider and an API on loopback: sign in (the browser's part
//! played by the test), keep the account, use the tools, refresh a token, disconnect.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ark_connect::*;
use ark_tools::{BlobResolver, RunCtx, ToolSource};
use base64::Engine as _;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Every request the fake servers saw: (method, path with query, body).
type Seen = Arc<Mutex<Vec<(String, String, String)>>>;

/// A server on loopback that records requests and answers with `route`.
async fn server(route: fn(&str, &str, &str) -> (u16, Value)) -> (String, Seen) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 8192];
                loop {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let len = text[..end]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if buf.len() >= end + 4 + len {
                            break;
                        }
                    }
                }
                let text = String::from_utf8_lossy(&buf).to_string();
                let mut parts = text.split_whitespace();
                let method = parts.next().unwrap_or("").to_string();
                let path = parts.next().unwrap_or("").to_string();
                let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
                let (status, reply) = route(&method, &path, &body);
                log.lock().unwrap().push((method, path, body));
                let reply = reply.to_string();
                let out = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                let _ = sock.write_all(out.as_bytes()).await;
            });
        }
    });
    (base, seen)
}

fn id_token(email: &str) -> String {
    let b = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    format!(
        "{}.{}.sig",
        b.encode(r#"{"alg":"none"}"#),
        b.encode(json!({ "email": email }).to_string())
    )
}

fn provider_routes(_m: &str, path: &str, body: &str) -> (u16, Value) {
    if path.starts_with("/token") {
        if body.contains("grant_type=authorization_code") {
            return (
                200,
                json!({
                    "access_token": "access-1", "refresh_token": "refresh-1", "expires_in": 3600,
                    "scope": "openid email https://www.googleapis.com/auth/gmail.send",
                    "id_token": id_token("ada@example.com")
                }),
            );
        }
        return (
            200,
            json!({ "access_token": "access-2", "expires_in": 3600 }),
        );
    }
    if path.starts_with("/revoke") {
        return (200, json!({}));
    }
    (404, json!({}))
}

fn api_routes(_m: &str, path: &str, _body: &str) -> (u16, Value) {
    if path.ends_with("/messages/send") {
        return (200, json!({ "id": "m1" }));
    }
    (404, json!({ "error": { "message": "no such thing" } }))
}

fn catalog(provider: &str) -> Catalog {
    let mut c = Catalog::builtin();
    let g = c.providers.iter_mut().find(|p| p.id == "google").unwrap();
    g.oauth.auth_url = format!("{provider}/auth");
    g.oauth.token_url = format!("{provider}/token");
    g.oauth.revoke_url = Some(format!("{provider}/revoke"));
    g.oauth.client_id = "ark-test-client".into();
    g.oauth.client_id_env = None;
    c
}

struct NoBlobs;
impl BlobResolver for NoBlobs {
    fn path(&self, _: &ark_core::ArtifactId) -> Option<std::path::PathBuf> {
        None
    }
    fn read(&self, id: &ark_core::ArtifactId) -> ark_tools::Result<Vec<u8>> {
        Err(ark_tools::AdapterError::ArtifactNotFound(id.to_string()))
    }
}

/// Play the browser: follow the sign-in page to the redirect the provider would send.
async fn sign_in_as_the_person(url: &str, approve: bool) {
    let url = reqwest::Url::parse(url).unwrap();
    let get = |k: &str| {
        url.query_pairs()
            .find(|(n, _)| n == k)
            .unwrap()
            .1
            .to_string()
    };
    let back = if approve {
        format!("{}?code=c0de&state={}", get("redirect_uri"), get("state"))
    } else {
        format!(
            "{}?error=access_denied&state={}",
            get("redirect_uri"),
            get("state")
        )
    };
    let page = reqwest::get(back).await.unwrap().text().await.unwrap();
    assert!(
        page.contains(if approve {
            "Connected"
        } else {
            "Not connected"
        }),
        "{page}"
    );
}

#[tokio::test]
async fn connect_use_refresh_and_disconnect_mail() {
    let (provider, provider_seen) = server(provider_routes).await;
    let (api, api_seen) = server(api_routes).await;
    let dir = tempfile::tempdir().unwrap();
    let secrets = Arc::new(MemorySecrets::default());
    let conns = Connections::open(dir.path(), catalog(&provider), secrets.clone())
        .with_base("google.mail", &api);

    let before = conns.status(&["web".into()]);
    let mail = before.iter().find(|c| c.id == "mail").unwrap();
    assert_eq!(mail.state, State::Connect);
    assert!(before
        .iter()
        .any(|c| c.id == "web" && c.state == State::Ready));
    assert!(
        !before.iter().any(|c| c.id == "files"),
        "the app has no files tools"
    );
    assert!(conns.adapters().is_empty());

    // Sign in: the page asks for Mail's scope plus who the account is, with PKCE.
    let connecting = conns.begin("Mail").await.unwrap();
    let url = connecting.url().to_string();
    assert!(url.contains("code_challenge_method=S256"), "{url}");
    assert!(url.contains("gmail.send"), "{url}");
    assert!(url.contains("scope=openid+email"), "{url}");
    let (account, ()) = tokio::join!(
        connecting.finish(Duration::from_secs(5)),
        sign_in_as_the_person(&url, true)
    );
    let account = account.unwrap();
    assert_eq!(account.id, "google:ada@example.com");
    let exchange = provider_seen.lock().unwrap()[0].2.clone();
    assert!(exchange.contains("code_verifier="), "{exchange}");
    assert_eq!(
        secrets
            .get("account:google:ada@example.com")
            .unwrap()
            .as_deref(),
        Some("refresh-1")
    );
    let file = std::fs::read_to_string(dir.path().join("connections.json")).unwrap();
    assert!(
        !file.contains("refresh-1") && !file.contains("access-1"),
        "{file}"
    );

    // Ready: the Mail tools join, Calendar still needs connecting.
    let after = conns.status(&[]);
    let mail = after.iter().find(|c| c.id == "mail").unwrap();
    assert_eq!(mail.state, State::Ready);
    assert_eq!(mail.account.as_deref(), Some("ada@example.com"));
    assert_eq!(
        after.iter().find(|c| c.id == "calendar").unwrap().state,
        State::Connect
    );
    let tools = conns.adapters();
    assert_eq!(tools.len(), 1);
    let send = tools[0]
        .actions()
        .into_iter()
        .find(|d| d.name == "mail.send")
        .unwrap();
    assert_eq!(send.risk, ark_core::RiskTier::Mutate);

    // Send: a well-formed message with the account's token.
    let out = tools[0]
        .run(
            "mail.send",
            json!({ "to": ["bob@example.com"], "subject": "Report", "body": "Hello, Bob." }),
            &RunCtx::new(&NoBlobs),
        )
        .await
        .map_err(|e| e.to_string())
        .unwrap();
    assert_eq!(out.output["sent"], true);
    let (_, path, body) = api_seen.lock().unwrap()[0].clone();
    assert_eq!(path, "/users/me/messages/send");
    let raw: Value = serde_json::from_str(&body).unwrap();
    let mime = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(raw["raw"].as_str().unwrap())
        .unwrap();
    let mime = String::from_utf8(mime).unwrap();
    assert!(
        mime.starts_with("To: bob@example.com\r\nSubject: Report\r\n"),
        "{mime}"
    );
    let (card, _) = services::distill("mail.send", &out.output).unwrap();
    assert_eq!(card, "sent \"Report\" to bob@example.com");

    // A header cannot be smuggled in.
    let bad = tools[0]
        .run(
            "mail.send",
            json!({ "to": "bob@example.com\r\nBcc: eve@example.com", "subject": "x", "body": "y" }),
            &RunCtx::new(&NoBlobs),
        )
        .await;
    assert!(bad.is_err());

    // An expired token is refreshed from the keychain's refresh token.
    conns.expire("google:ada@example.com");
    assert_eq!(
        conns.access_token("google:ada@example.com").await.unwrap(),
        "access-2"
    );

    // Opened again from disk, it is still connected.
    let reopened = Connections::open(dir.path(), catalog(&provider), secrets.clone());
    assert_eq!(reopened.accounts(), vec![account.clone()]);

    // Disconnect: revoked at the provider, token deleted, tools gone.
    conns.disconnect("mail").await.unwrap();
    assert!(provider_seen
        .lock()
        .unwrap()
        .iter()
        .any(|(_, p, b)| p.starts_with("/revoke") && b.contains("refresh-1")));
    assert!(secrets
        .get("account:google:ada@example.com")
        .unwrap()
        .is_none());
    assert!(conns.adapters().is_empty());
    assert_eq!(conns.status(&[])[0].state, State::Connect);
}

#[tokio::test]
async fn a_refused_sign_in_connects_nothing() {
    let (provider, _) = server(provider_routes).await;
    let dir = tempfile::tempdir().unwrap();
    let conns = Connections::open(
        dir.path(),
        catalog(&provider),
        Arc::new(MemorySecrets::default()),
    );
    let connecting = conns.begin("calendar").await.unwrap();
    let url = connecting.url().to_string();
    let (r, ()) = tokio::join!(
        connecting.finish(Duration::from_secs(5)),
        sign_in_as_the_person(&url, false)
    );
    assert!(r.unwrap_err().to_string().contains("not allowed"));
    assert!(conns.accounts().is_empty());
}

#[tokio::test]
async fn without_a_client_id_a_capability_says_it_cannot_connect() {
    let mut c = Catalog::builtin();
    for p in &mut c.providers {
        p.oauth.client_id.clear();
        p.oauth.client_id_env = None;
    }
    let dir = tempfile::tempdir().unwrap();
    let conns = Connections::open(dir.path(), c, Arc::new(MemorySecrets::default()));
    let mail = conns
        .status(&[])
        .into_iter()
        .find(|s| s.id == "mail")
        .unwrap();
    assert!(matches!(mail.state, State::Unavailable(_)), "{mail:?}");
    assert!(conns.begin("mail").await.is_err());
}
