//! An account against a fake account service on loopback: discovery, sign-in (the browser's
//! part played by the test), staying signed in across restarts, refresh with rotation, a
//! sign-in the service ended, sign-out with revoke.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ark_account::*;
use ark_connect::{MemorySecrets, Secrets};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const CLIENT: &str = "ark-desktop";
const API: &str = "https://api.example.test";

#[derive(Default)]
struct Fake {
    /// refresh token -> still valid
    refresh: HashMap<String, bool>,
    issued: u32,
    /// access token lifetime in seconds
    ttl: u64,
    entitlements: Vec<String>,
    /// discovery names another issuer
    wrong_issuer: bool,
    /// the service does not answer
    down: bool,
    seen: Vec<(String, String, String)>,
}

type Shared = Arc<Mutex<Fake>>;

fn form(body: &str) -> HashMap<String, String> {
    reqwest::Url::parse(&format!("http://x/?{body}"))
        .unwrap()
        .query_pairs()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn route(
    fake: &Shared,
    base: &str,
    method: &str,
    path: &str,
    head: &str,
    body: &str,
) -> (u16, Value) {
    let mut f = fake.lock().unwrap();
    f.seen.push((method.into(), path.into(), body.into()));
    if path == "/.well-known/openid-configuration" {
        let issuer = if f.wrong_issuer {
            "https://elsewhere.test".to_string()
        } else {
            base.to_string()
        };
        return (
            200,
            json!({
                "issuer": issuer,
                "authorization_endpoint": format!("{base}/authorize"),
                "token_endpoint": format!("{base}/token"),
                "revocation_endpoint": format!("{base}/revoke"),
            }),
        );
    }
    if path == "/token" {
        let p = form(body);
        assert_eq!(p.get("client_id").map(String::as_str), Some(CLIENT));
        assert!(
            !p.contains_key("client_secret"),
            "a public client sends no secret"
        );
        let ok = match p["grant_type"].as_str() {
            "authorization_code" => p["code"] == "c0de" && p.contains_key("code_verifier"),
            "refresh_token" => f.refresh.get(&p["refresh_token"]).copied().unwrap_or(false),
            _ => false,
        };
        if !ok {
            return (400, json!({ "error": "invalid_grant" }));
        }
        if let Some(old) = p.get("refresh_token") {
            f.refresh.insert(old.clone(), false); // rotation
        }
        f.issued += 1;
        let n = f.issued;
        f.refresh.insert(format!("refresh-{n}"), true);
        return (
            200,
            json!({
                "access_token": format!("access-{n}"), "refresh_token": format!("refresh-{n}"),
                "expires_in": f.ttl, "token_type": "Bearer",
            }),
        );
    }
    if path == "/revoke" {
        let p = form(body);
        if p.get("client_id").map(String::as_str) != Some(CLIENT) {
            return (401, json!({ "error": "invalid_client" }));
        }
        f.refresh.insert(p["token"].clone(), false);
        return (200, json!({}));
    }
    if path == "/v1/me" {
        let bearer = head
            .lines()
            .find_map(|l| {
                l.strip_prefix("authorization: Bearer ")
                    .or(l.strip_prefix("Authorization: Bearer "))
            })
            .unwrap_or("");
        if !bearer.starts_with("access-") {
            return (401, json!({ "error": "not signed in" }));
        }
        return (
            200,
            json!({
                "id": "6b1f0c1e-0000-4000-8000-000000000001", "email": "ada@example.com",
                "name": "Ada", "image": null, "entitlements": f.entitlements,
            }),
        );
    }
    (404, json!({}))
}

async fn service(ttl: u64) -> (String, Shared) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let fake: Shared = Arc::new(Mutex::new(Fake {
        ttl,
        ..Fake::default()
    }));
    let (f, b) = (fake.clone(), base.clone());
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let (f, b) = (f.clone(), b.clone());
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
                let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
                let mut first = head.split_whitespace();
                let method = first.next().unwrap_or("").to_string();
                let path = first.next().unwrap_or("").to_string();
                if f.lock().unwrap().down {
                    return; // the connection closes with no answer
                }
                let (status, reply) = route(&f, &b, &method, &path, head, body);
                let reply = reply.to_string();
                let out = format!("HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}", reply.len());
                let _ = sock.write_all(out.as_bytes()).await;
            });
        }
    });
    (base, fake)
}

/// The browser's part: the service sends it back to the app's loopback address with a code.
/// Returns where the app then sends the browser.
async fn browser(url: &str) -> String {
    let url = reqwest::Url::parse(url).unwrap();
    let q: HashMap<String, String> = url
        .query_pairs()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let back = format!("{}?code=c0de&state={}", q["redirect_uri"], q["state"]);
    let no_follow = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let r = no_follow.get(back).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 303);
    r.headers()["location"].to_str().unwrap().to_string()
}

async fn signed_in(base: &str, secrets: Arc<MemorySecrets>) -> Account {
    let account = Account::new(Service::new(base, API, CLIENT), secrets);
    let s = account.sign_in().await.unwrap();
    let url = s.url().to_string();
    let (profile, _) = tokio::join!(s.finish(Duration::from_secs(5)), browser(&url));
    assert_eq!(profile.unwrap().email, "ada@example.com");
    account
}

#[tokio::test]
async fn signed_out_until_someone_signs_in() {
    let (base, fake) = service(900).await;
    let account = Account::new(
        Service::new(&base, API, CLIENT),
        Arc::new(MemorySecrets::default()),
    );
    assert_eq!(account.status(), Status::SignedOut);
    assert_eq!(account.can("cloud.agents"), Access::SignIn);
    assert!(matches!(
        account.access_token().await,
        Err(AccountError::NotSignedIn)
    ));
    assert!(matches!(account.refresh().await, Ok(Status::SignedOut)));
    assert!(
        fake.lock().unwrap().seen.is_empty(),
        "nothing asks the network while signed out"
    );
}

#[tokio::test]
async fn sign_in_through_the_browser_and_stay_signed_in() {
    let (base, fake) = service(900).await;
    let secrets = Arc::new(MemorySecrets::default());
    let account = Account::new(Service::new(&base, API, CLIENT), secrets.clone());
    let changes = Arc::new(Mutex::new(Vec::new()));
    let c = changes.clone();
    account.on_change(move |s| c.lock().unwrap().push(s.signed_in()));

    let s = account.sign_in().await.unwrap();
    let url = reqwest::Url::parse(s.url()).unwrap();
    let q: HashMap<String, String> = url
        .query_pairs()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    assert_eq!(url.path(), "/authorize");
    assert_eq!(q["client_id"], CLIENT);
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(q["resource"], API);
    assert_eq!(q["prompt"], "login");
    assert!(q["redirect_uri"].starts_with("http://127.0.0.1:"));
    assert!(q["scope"].split(' ').any(|s| s == "offline_access"));

    let url = s.url().to_string();
    let (profile, landing) = tokio::join!(s.finish(Duration::from_secs(5)), browser(&url));
    assert_eq!(
        landing,
        format!("{base}/signed-in"),
        "the browser lands on the service's page"
    );
    let profile = profile.unwrap();
    assert_eq!(
        (profile.name.as_str(), profile.email.as_str()),
        ("Ada", "ada@example.com")
    );
    assert_eq!(account.status().profile(), Some(&profile));
    assert_eq!(account.can("cloud.agents"), Access::NotIncluded);
    assert_eq!(*changes.lock().unwrap(), vec![true]);

    // The keychain keeps the refresh token; the access token is never written.
    let saved = secrets.get(&format!("account:{base}")).unwrap().unwrap();
    assert!(saved.contains("refresh-1") && !saved.contains("access-1"));

    // A restart: the account is known before any network call.
    let seen = fake.lock().unwrap().seen.len();
    let again = Account::new(Service::new(&base, API, CLIENT), secrets.clone());
    assert_eq!(
        again.status().profile().map(|p| p.email.as_str()),
        Some("ada@example.com")
    );
    assert_eq!(fake.lock().unwrap().seen.len(), seen);
    // Then it checks in: a refresh (rotating the token) and the profile.
    assert!(again.refresh().await.unwrap().signed_in());
    let saved = secrets.get(&format!("account:{base}")).unwrap().unwrap();
    assert!(
        saved.contains("refresh-2"),
        "the rotated token is kept: {saved}"
    );
}

#[tokio::test]
async fn short_access_tokens_refresh_once_even_when_asked_together() {
    let (base, fake) = service(30).await; // under a minute: never fresh
    let account = signed_in(&base, Arc::new(MemorySecrets::default())).await;
    let issued = fake.lock().unwrap().issued;
    let (a, b) = tokio::join!(account.access_token(), account.access_token());
    assert!(a.is_ok() && b.is_ok());
    // Each call refreshes in turn (the tokens are never fresh), but never with a spent token.
    let f = fake.lock().unwrap();
    assert_eq!(f.issued, issued + 2);
    assert!(
        f.refresh.values().filter(|v| **v).count() == 1,
        "one live refresh token"
    );
}

#[tokio::test]
async fn a_fresh_access_token_is_reused() {
    let (base, fake) = service(900).await;
    let account = signed_in(&base, Arc::new(MemorySecrets::default())).await;
    let before = fake.lock().unwrap().issued;
    assert_eq!(account.access_token().await.unwrap(), "access-1");
    assert_eq!(account.access_token().await.unwrap(), "access-1");
    assert_eq!(fake.lock().unwrap().issued, before);
}

#[tokio::test]
async fn a_sign_in_the_service_ended_signs_this_computer_out() {
    let (base, fake) = service(0).await;
    let secrets = Arc::new(MemorySecrets::default());
    let account = signed_in(&base, secrets.clone()).await;
    // Revoked from elsewhere (another device's "sign out everywhere", an admin).
    fake.lock()
        .unwrap()
        .refresh
        .values_mut()
        .for_each(|v| *v = false);
    assert!(matches!(
        account.access_token().await,
        Err(AccountError::NotSignedIn)
    ));
    assert_eq!(account.status(), Status::SignedOut);
    assert_eq!(secrets.get(&format!("account:{base}")).unwrap(), None);
}

#[tokio::test]
async fn sign_out_revokes_this_computers_token_only() {
    let (base, fake) = service(900).await;
    let secrets = Arc::new(MemorySecrets::default());
    let account = signed_in(&base, secrets.clone()).await;
    account.sign_out().await.unwrap();
    assert_eq!(account.status(), Status::SignedOut);
    assert_eq!(secrets.get(&format!("account:{base}")).unwrap(), None);
    let f = fake.lock().unwrap();
    let revoke = f
        .seen
        .iter()
        .find(|(_, p, _)| p == "/revoke")
        .expect("revoked at the service");
    assert!(revoke.2.contains("token=refresh-1") && revoke.2.contains("client_id=ark-desktop"));
    assert_eq!(f.refresh.get("refresh-1"), Some(&false));
}

#[tokio::test]
async fn without_the_service_the_account_stays_known_and_sign_out_still_works() {
    let (base, fake) = service(0).await;
    let secrets = Arc::new(MemorySecrets::default());
    signed_in(&base, secrets.clone()).await;
    fake.lock().unwrap().down = true;
    let account = Account::new(Service::new(&base, API, CLIENT), secrets.clone());
    // Restoring while the service is unreachable keeps the last known status.
    assert!(matches!(
        account.refresh().await,
        Err(AccountError::Unreachable)
    ));
    assert!(account.status().signed_in());
    // Signing out needs no network: the tokens on this computer are gone.
    account.sign_out().await.unwrap();
    assert!(!account.status().signed_in());
    assert_eq!(secrets.get(&format!("account:{base}")).unwrap(), None);
}

#[tokio::test]
async fn entitlements_come_from_the_service() {
    let (base, fake) = service(900).await;
    fake.lock().unwrap().entitlements = vec!["cloud.agents".into()];
    let account = signed_in(&base, Arc::new(MemorySecrets::default())).await;
    assert_eq!(account.can("cloud.agents"), Access::Allowed);
    assert_eq!(account.can("cloud.gpu"), Access::NotIncluded);
}

#[tokio::test]
async fn a_service_that_names_another_issuer_is_refused() {
    let (base, fake) = service(900).await;
    fake.lock().unwrap().wrong_issuer = true;
    let account = Account::new(
        Service::new(&base, API, CLIENT),
        Arc::new(MemorySecrets::default()),
    );
    let err = account.sign_in().await.err().expect("refused");
    assert!(err.to_string().contains("another issuer"), "{err}");
}

#[tokio::test]
async fn signing_in_again_revokes_the_sign_in_it_replaces() {
    let (base, fake) = service(900).await;
    let secrets = Arc::new(MemorySecrets::default());
    let account = signed_in(&base, secrets.clone()).await;
    let s = account.sign_in().await.unwrap();
    let url = s.url().to_string();
    let (p, _) = tokio::join!(s.finish(Duration::from_secs(5)), browser(&url));
    p.unwrap();
    let f = fake.lock().unwrap();
    assert_eq!(
        f.refresh.get("refresh-1"),
        Some(&false),
        "the old token is revoked"
    );
    assert_eq!(f.refresh.get("refresh-2"), Some(&true));
}
