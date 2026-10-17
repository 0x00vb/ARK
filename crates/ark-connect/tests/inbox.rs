//! The Inbox capability against a fake Gmail on loopback (ADR 0062, docs/AUTOMATIONS.md A2):
//! the feed's baseline, history paging with a duplicate across pages, sent mail left out, an
//! expired history as a gap, reconciling, a lost sign-in, rate limits, and replies bound to
//! someone who wrote in the thread, with a send that got no answer left unconfirmed.

use std::sync::{Arc, Mutex};

use ark_connect::*;
use ark_tools::events::{EventSource, SourceError};
use ark_tools::{BlobResolver, RunCtx, ToolSource};
use base64::Engine as _;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const READ: &str = "https://www.googleapis.com/auth/gmail.readonly";
const SEND: &str = "https://www.googleapis.com/auth/gmail.send";

type Seen = Arc<Mutex<Vec<(String, String, String)>>>;
/// A reply to a request: a status and a body, or `None` to close without answering.
type Route = Arc<dyn Fn(&str, &str, &str) -> Option<(u16, Value)> + Send + Sync>;

/// Gmail and Google's token endpoint on loopback, recording each request.
async fn server(route: Route) -> (String, Seen) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen: Seen = Arc::default();
    let log = seen.clone();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let (log, route) = (log.clone(), route.clone());
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
                let reply = route(&method, &path, &body);
                log.lock().unwrap().push((method, path, body));
                let Some((status, reply)) = reply else {
                    return;
                };
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

struct NoBlobs;
impl BlobResolver for NoBlobs {
    fn path(&self, _: &ark_core::ArtifactId) -> Option<std::path::PathBuf> {
        None
    }
    fn read(&self, id: &ark_core::ArtifactId) -> ark_tools::Result<Vec<u8>> {
        Err(ark_tools::AdapterError::ArtifactNotFound(id.to_string()))
    }
}

/// Connections with office@example.com signed in with `scopes`, Google at `base`.
fn connected(dir: &std::path::Path, base: &str, scopes: &[&str]) -> Connections {
    std::fs::write(
        dir.join("connections.json"),
        json!({ "accounts": [{
            "id": "google:office@example.com", "provider": "google",
            "name": "office@example.com", "scopes": scopes, "connected_at": 0
        }] })
        .to_string(),
    )
    .unwrap();
    let secrets = Arc::new(MemorySecrets::default());
    secrets
        .set("account:google:office@example.com", "refresh-1")
        .unwrap();
    let mut c = Catalog::builtin();
    let g = c.providers.iter_mut().find(|p| p.id == "google").unwrap();
    g.oauth.token_url = format!("{base}/token");
    g.oauth.client_id = "ark-test-client".into();
    g.oauth.client_id_env = None;
    Connections::open(dir, c, secrets).with_base("google.inbox", base)
}

fn token(path: &str) -> Option<(u16, Value)> {
    path.starts_with("/token").then(|| {
        (
            200,
            json!({ "access_token": "access-1", "expires_in": 3600 }),
        )
    })
}

fn b64(s: &str) -> String {
    base64::engine::general_purpose::URL_SAFE.encode(s)
}

fn full(id: &str, labels: &[&str], from: &str) -> Value {
    json!({
        "id": id, "threadId": format!("t-{id}"), "labelIds": labels,
        "internalDate": "1700000000000",
        "payload": {
            "mimeType": "text/plain",
            "headers": [
                { "name": "From", "value": from },
                { "name": "Subject", "value": "Q4" }
            ],
            "body": { "data": b64("Can you send me the Q4 totals?") }
        }
    })
}

fn feed(conns: &Connections) -> Arc<dyn EventSource> {
    let mut all = conns.event_sources();
    assert_eq!(all.len(), 1);
    all.remove(0).source
}

#[tokio::test]
async fn the_feed_starts_at_a_baseline_pages_history_and_keeps_only_inbound_mail() {
    let route: Route = Arc::new(|_, path, _| {
        token(path).or(Some(match path {
            "/users/me/profile" => (200, json!({ "historyId": "100" })),
            p if p.starts_with("/users/me/history?startHistoryId=100") && !p.contains("pageToken") => (
                200,
                json!({
                    "history": [
                        { "id": "101", "messagesAdded": [{ "message": { "id": "m1", "labelIds": ["INBOX", "UNREAD"] } }] },
                        { "id": "102", "messagesAdded": [{ "message": { "id": "m2", "labelIds": ["SENT", "INBOX"] } }] }
                    ],
                    "nextPageToken": "p2", "historyId": "110"
                }),
            ),
            p if p.contains("pageToken=p2") => (
                200,
                json!({
                    "history": [{ "id": "103", "messagesAdded": [
                        { "message": { "id": "m1", "labelIds": ["INBOX"] } },
                        { "message": { "id": "m3", "labelIds": ["INBOX"] } }
                    ] }],
                    "historyId": "110"
                }),
            ),
            "/users/me/messages/m1?format=full" => (200, full("m1", &["INBOX"], "Lara <Lara@Example.com>")),
            // Deleted between the history and the fetch.
            "/users/me/messages/m3?format=full" => (404, json!({ "error": { "message": "gone" } })),
            _ => (404, json!({ "error": { "message": "no such thing" } })),
        }))
    });
    let (base, seen) = server(route).await;
    let dir = tempfile::tempdir().unwrap();
    let conns = connected(dir.path(), &base, &[READ, SEND]);
    let all = conns.event_sources();
    assert_eq!(all[0].label, "Inbox (office@example.com)");
    assert_eq!(all[0].actions, vec!["inbox.read", "inbox.reply"]);
    let f = feed(&conns);
    assert_eq!((f.source(), f.account()), ("gmail", "office@example.com"));

    let b = f.poll(None, 100).await.unwrap();
    assert_eq!((b.cursor.as_str(), b.events.len()), ("100", 0));

    let b = f.poll(Some("100"), 100).await.unwrap();
    assert_eq!((b.cursor.as_str(), b.more), ("110", false));
    assert_eq!(b.events.len(), 1, "{:?}", b.events);
    let e = &b.events[0];
    assert_eq!(e.key, "m1");
    assert_eq!(e.actor.as_deref(), Some("lara@example.com"));
    assert_eq!(e.subject.as_deref(), Some("t-m1"));
    assert_eq!(e.payload.as_deref(), Some("Can you send me the Q4 totals?"));
    assert!(
        !seen
            .lock()
            .unwrap()
            .iter()
            .any(|(_, p, _)| p.contains("messages/m2")),
        "sent mail is never fetched"
    );

    // Stopping at `max` keeps the cursor at the last whole page read.
    let b = f.poll(Some("100"), 1).await.unwrap();
    assert_eq!(
        (b.cursor.as_str(), b.more, b.events.len()),
        ("102", true, 1)
    );

    // An account signed in for sending only delivers no events.
    let other = tempfile::tempdir().unwrap();
    assert!(connected(other.path(), &base, &[SEND])
        .event_sources()
        .is_empty());
}

#[tokio::test]
async fn an_expired_history_is_a_gap_and_reconciling_lists_inbox_mail_since_then() {
    let route: Route = Arc::new(|_, path, _| {
        token(path).or(Some(match path {
            "/users/me/profile" => (200, json!({ "historyId": "500" })),
            p if p.starts_with("/users/me/history") => (
                404,
                json!({ "error": { "message": "Requested entity was not found." } }),
            ),
            p if p.starts_with("/users/me/messages?labelIds=INBOX") => (
                200,
                json!({ "messages": [{ "id": "m1" }, { "id": "old" }] }),
            ),
            "/users/me/messages/m1?format=full" => {
                (200, full("m1", &["INBOX"], "lara@example.com"))
            }
            "/users/me/messages/old?format=full" => {
                let mut m = full("old", &["INBOX"], "lara@example.com");
                m["internalDate"] = json!("1000");
                (200, m)
            }
            _ => (404, json!({})),
        }))
    });
    let (base, seen) = server(route).await;
    let dir = tempfile::tempdir().unwrap();
    let f = feed(&connected(dir.path(), &base, &[READ, SEND]));
    let Err(SourceError::Gap { since_ms, detail }) = f.poll(Some("5"), 100).await else {
        panic!("an expired history id is a gap")
    };
    assert_eq!(since_ms, None);
    assert!(detail.contains("no longer list changes"), "{detail}");

    let since = 1_699_999_000_000;
    let b = f.reconcile(since, 100).await.unwrap();
    assert_eq!(b.cursor, "500");
    assert_eq!(
        b.events.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(),
        vec!["m1"],
        "only mail since the gap began"
    );
    let paths: Vec<String> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|(_, p, _)| p.clone())
        .collect();
    let profile = paths.iter().position(|p| p == "/users/me/profile").unwrap();
    let list = paths
        .iter()
        .position(|p| p.starts_with("/users/me/messages?"))
        .unwrap();
    assert!(
        profile < list,
        "the cursor is taken before the mail is listed"
    );
    assert!(
        paths[list].contains("q=after%3A1699999000"),
        "{}",
        paths[list]
    );
}

#[tokio::test]
async fn a_lost_sign_in_is_disconnected_and_too_many_requests_is_rate_limited() {
    let route: Route = Arc::new(|_, path, _| {
        Some(match path {
            p if p.starts_with("/token") => (400, json!({ "error": "invalid_grant" })),
            _ => (200, json!({})),
        })
    });
    let (base, _) = server(route).await;
    let dir = tempfile::tempdir().unwrap();
    let f = feed(&connected(dir.path(), &base, &[READ, SEND]));
    assert!(matches!(
        f.poll(Some("1"), 10).await,
        Err(SourceError::Disconnected(_))
    ));

    let route: Route = Arc::new(|_, path, _| {
        token(path).or(Some((429, json!({ "error": { "message": "slow down" } }))))
    });
    let (base, _) = server(route).await;
    let dir = tempfile::tempdir().unwrap();
    let f = feed(&connected(dir.path(), &base, &[READ, SEND]));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let Err(SourceError::RateLimited { retry_at_ms }) = f.poll(Some("1"), 10).await else {
        panic!("429 is a rate limit")
    };
    assert!(retry_at_ms >= now + 59_000, "waits a minute when not told");
}

fn thread_routes(send: Option<(u16, Value)>) -> Route {
    Arc::new(move |_, path, _| {
        token(path).or(match path {
            p if p.starts_with("/users/me/threads/t1?") => Some((
                200,
                json!({ "messages": [
                    { "payload": { "headers": [
                        { "name": "From", "value": "Lara <lara@example.com>" },
                        { "name": "Subject", "value": "Q4" },
                        { "name": "Message-ID", "value": "<a@mail.example>" }
                    ] } },
                    { "payload": { "headers": [
                        { "name": "From", "value": "office@example.com" },
                        { "name": "Subject", "value": "Re: Q4" },
                        { "name": "Message-ID", "value": "<b@mail.example>" },
                        { "name": "References", "value": "<a@mail.example>" }
                    ] } }
                ] }),
            )),
            "/users/me/messages/send" => send.clone(),
            _ => Some((404, json!({}))),
        })
    })
}

fn inbox(conns: &Connections) -> Arc<dyn ark_tools::Adapter> {
    conns
        .adapters()
        .into_iter()
        .find(|a| a.name() == "inbox")
        .unwrap()
}

#[tokio::test]
async fn a_reply_goes_in_the_thread_and_only_to_someone_who_wrote_in_it() {
    let (base, seen) = server(thread_routes(Some((200, json!({ "id": "r1" }))))).await;
    let dir = tempfile::tempdir().unwrap();
    let tools = inbox(&connected(dir.path(), &base, &[READ, SEND]));
    let reply = tools
        .actions()
        .into_iter()
        .find(|d| d.name == "inbox.reply")
        .unwrap();
    assert_eq!(reply.risk, ark_core::RiskTier::Mutate);

    let ctx = RunCtx::new(&NoBlobs);
    let out = tools
        .run(
            "inbox.reply",
            json!({ "to": "LARA@example.com", "thread": "t1", "body": "Here are the totals." }),
            &ctx,
        )
        .await
        .map_err(|e| e.to_string())
        .unwrap();
    assert_eq!(out.output["sent"], true);
    assert!(out.effect.is_none());
    let (_, _, body) = seen
        .lock()
        .unwrap()
        .iter()
        .find(|(_, p, _)| p == "/users/me/messages/send")
        .cloned()
        .unwrap();
    let sent: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(sent["threadId"], "t1");
    let mime = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(sent["raw"].as_str().unwrap())
        .unwrap();
    let mime = String::from_utf8(mime).unwrap();
    assert!(
        mime.starts_with("To: lara@example.com\r\nIn-Reply-To: <a@mail.example>\r\nReferences: <a@mail.example>\r\nSubject: Re: Q4\r\n"),
        "{mime}"
    );

    let sends = |s: &Seen| {
        s.lock()
            .unwrap()
            .iter()
            .filter(|(_, p, _)| p.ends_with("/send"))
            .count()
    };
    let before = sends(&seen);
    for to in [
        "eve@example.com",
        "lara@example.com\r\nBcc: eve@example.com",
        "a@b.com, lara@example.com",
    ] {
        let err = tools
            .run(
                "inbox.reply",
                json!({ "to": to, "thread": "t1", "body": "x" }),
                &ctx,
            )
            .await
            .err()
            .unwrap_or_else(|| panic!("{to} was allowed"));
        assert!(!err.to_string().is_empty());
    }
    let bad_thread = tools
        .run(
            "inbox.reply",
            json!({ "to": "lara@example.com", "thread": "../x", "body": "x" }),
            &ctx,
        )
        .await;
    assert!(bad_thread.is_err());
    assert_eq!(sends(&seen), before, "nothing else was sent");
}

#[tokio::test]
async fn a_send_that_got_no_answer_is_unconfirmed_and_not_to_be_retried() {
    let (base, _) = server(thread_routes(None)).await;
    let dir = tempfile::tempdir().unwrap();
    let tools = inbox(&connected(dir.path(), &base, &[READ, SEND]));
    let out = tools
        .run(
            "inbox.reply",
            json!({ "to": "lara@example.com", "thread": "t1", "body": "Totals." }),
            &RunCtx::new(&NoBlobs),
        )
        .await
        .map_err(|e| e.to_string())
        .unwrap();
    let effect = out.effect.expect("an unconfirmed effect");
    assert_eq!(effect.state, ark_core::ExternalEffectState::Unconfirmed);
    assert!(!effect.retry_allowed);
    assert!(effect
        .user_message
        .unwrap()
        .contains("may or may not have been sent"));

    // A refusal from Gmail is certain: nothing was sent, and it is an error.
    let (base, _) = server(thread_routes(Some((
        400,
        json!({ "error": { "message": "bad" } }),
    ))))
    .await;
    let dir = tempfile::tempdir().unwrap();
    let tools = inbox(&connected(dir.path(), &base, &[READ, SEND]));
    assert!(tools
        .run(
            "inbox.reply",
            json!({ "to": "lara@example.com", "thread": "t1", "body": "Totals." }),
            &RunCtx::new(&NoBlobs),
        )
        .await
        .is_err());
}
