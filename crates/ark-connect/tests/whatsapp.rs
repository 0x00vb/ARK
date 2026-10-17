//! WhatsApp Business against a fake Graph API on loopback (ADR 0062, docs/AUTOMATIONS.md A3):
//! setup checked before the account is saved, secret rotation on the same route, the GET
//! challenge, the signature over the raw body before parsing, account binding, malformed,
//! oversized and duplicate bodies, distinct status keys, the 24-hour window at each send, and a
//! send without an answer left unconfirmed.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use ark_connect::services::whatsapp::{Hook, Setup};
use ark_connect::*;
use ark_tools::events::{ExternalEvent, HookError, Limits, WebhookSource};
use ark_tools::{BlobResolver, RunCtx, ToolSource};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const FIXTURE: &str =
    include_str!("../../ark-orchestrate/tests/fixtures/automations/whatsapp.json");

type Seen = Arc<Mutex<Vec<(String, String, String)>>>;
type Route = Arc<dyn Fn(&str, &str, &str) -> Option<(u16, Value)> + Send + Sync>;

/// Graph on loopback, recording each request. `None` from the route closes without answering.
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
        Err(ark_tools::AdapterError::Tool(format!("no blob {id:?}")))
    }
}

fn setup(app_secret: &str, token: &str) -> Setup {
    Setup {
        waba_id: "waba-111".into(),
        phone_id: "phone-222".into(),
        app_secret: app_secret.into(),
        verify_token: "test-verify-token".into(),
        token: token.into(),
    }
}

/// Graph with the number in the WABA and the WABA subscribed to an app.
fn graph(_token: &'static str) -> Route {
    Arc::new(move |_, path, _| {
        Some(match path {
            "/phone-222?fields=display_phone_number,verified_name" => (
                200,
                json!({ "display_phone_number": "34900000000", "verified_name": "Shop", "id": "phone-222" }),
            ),
            "/waba-111/phone_numbers?fields=id" => {
                (200, json!({ "data": [{ "id": "phone-222" }] }))
            }
            "/waba-111/subscribed_apps" => (
                200,
                json!({ "data": [{ "whatsapp_business_api_data": { "id": "app-1" } }] }),
            ),
            "/phone-222/messages" => (200, json!({ "messages": [{ "id": "wamid.OUT1" }] })),
            _ => (404, json!({ "error": { "message": "unknown path" } })),
        })
    })
}

fn conns(dir: &std::path::Path, base: &str) -> Connections {
    Connections::open(dir, Catalog::builtin(), Arc::new(MemorySecrets::default()))
        .with_base("whatsapp.business", base)
}

fn sign(secret: &str, raw: &[u8]) -> BTreeMap<String, String> {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret.as_bytes());
    let tag = ring::hmac::sign(&key, raw);
    let hex: String = tag.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    BTreeMap::from([("x-hub-signature-256".to_string(), format!("sha256={hex}"))])
}

fn fixture() -> (Value, Vec<u8>) {
    let f: Value = serde_json::from_str(FIXTURE).unwrap();
    let raw = serde_json::to_vec(&f["provider"]["body"]).unwrap();
    (f, raw)
}

#[test]
fn a_signed_fixture_body_decodes_into_the_expected_events_with_distinct_status_keys() {
    let (f, raw) = fixture();
    let hook = Hook::new(setup("test-app-secret", "t"));
    let events = hook.decode(&sign("test-app-secret", &raw), &raw).unwrap();
    let expected: Vec<ExternalEvent> = serde_json::from_value(f["expected"].clone()).unwrap();
    assert_eq!(events, expected);
    assert_eq!(hook.account(), f["account"]);
    assert_ne!(
        events[0].key, events[1].key,
        "a status never shares a message's key"
    );
    assert_eq!(events[0].key, events[2].key, "a re-delivery keeps its key");
    assert!(
        !events.iter().any(|e| e.label.contains("Ignore all rules")),
        "the profile name is never used"
    );
}

#[test]
fn the_signature_is_checked_over_the_raw_bytes_before_anything_is_parsed() {
    let (_, raw) = fixture();
    let hook = Hook::new(setup("test-app-secret", "t"));
    let none = BTreeMap::new();
    assert!(matches!(
        hook.decode(&none, &raw),
        Err(HookError::Unverified(_))
    ));
    assert!(matches!(
        hook.decode(&sign("another-secret", &raw), &raw),
        Err(HookError::Unverified(_))
    ));
    // Not JSON and not signed: refused as unverified, so nothing was parsed.
    assert!(matches!(
        hook.decode(&sign("another-secret", b"{not json"), b"{not json"),
        Err(HookError::Unverified(_))
    ));
    // The same JSON with one byte changed (a space) no longer matches.
    let mut spaced = raw.clone();
    spaced.insert(1, b' ');
    assert!(matches!(
        hook.decode(&sign("test-app-secret", &raw), &spaced),
        Err(HookError::Unverified(_))
    ));
    // Signed but not JSON: malformed.
    assert!(matches!(
        hook.decode(&sign("test-app-secret", b"{not json"), b"{not json"),
        Err(HookError::Malformed(_))
    ));
}

#[test]
fn a_signed_body_for_another_number_or_over_the_limit_is_refused() {
    let hook = Hook::new(setup("s", "t"));
    // Changes for another number beside ours are left out; ours are kept.
    let mixed = serde_json::to_vec(&json!({
        "object": "whatsapp_business_account",
        "entry": [
            { "id": "waba-999", "changes": [{ "field": "messages", "value": {
                "metadata": { "phone_number_id": "phone-999" },
                "messages": [{ "from": "34600000009", "id": "wamid.OTHER", "timestamp": "1", "type": "text" }]
            } }] },
            { "id": "waba-111", "changes": [
                { "field": "messages", "value": {
                    "metadata": { "phone_number_id": "phone-999" },
                    "messages": [{ "from": "34600000008", "id": "wamid.OTHER2", "timestamp": "1", "type": "text" }]
                } },
                { "field": "messages", "value": {
                    "metadata": { "phone_number_id": "phone-222" },
                    "messages": [{ "from": "34600000001", "id": "wamid.OURS", "timestamp": "1", "type": "text" }]
                } }
            ] }
        ]
    }))
    .unwrap();
    let kept = hook.decode(&sign("s", &mixed), &mixed).unwrap();
    assert_eq!(
        kept.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(),
        vec!["wa:msg:wamid.OURS"]
    );
    // A time past any window is malformed, not an overflow.
    let huge = serde_json::to_vec(&json!({
        "object": "whatsapp_business_account",
        "entry": [{ "id": "waba-111", "changes": [{ "field": "messages", "value": {
            "metadata": { "phone_number_id": "phone-222" },
            "messages": [{ "from": "34600000001", "id": "wamid.X", "timestamp": "18446744073709551", "type": "text" }]
        } }] }]
    }))
    .unwrap();
    assert!(matches!(
        hook.decode(&sign("s", &huge), &huge),
        Err(HookError::Malformed(_))
    ));
    for (waba, phone) in [("waba-999", "phone-222"), ("waba-111", "phone-999")] {
        let raw = serde_json::to_vec(&json!({
            "object": "whatsapp_business_account",
            "entry": [{ "id": waba, "changes": [{ "field": "messages", "value": {
                "metadata": { "phone_number_id": phone },
                "messages": [{ "from": "34600000001", "id": "wamid.X", "timestamp": "1", "type": "text", "text": { "body": "hi" } }]
            } }] }]
        }))
        .unwrap();
        assert!(
            matches!(
                hook.decode(&sign("s", &raw), &raw),
                Err(HookError::Unbound(_))
            ),
            "{waba}/{phone}"
        );
    }
    let small = Hook::new(setup("s", "t")).with_limits(Limits {
        raw_body: 64,
        ..Limits::default()
    });
    let (_, raw) = fixture();
    assert!(matches!(
        small.decode(&sign("s", &raw), &raw),
        Err(HookError::Malformed(_))
    ));
    // A message without a sender number is not an event.
    let raw = serde_json::to_vec(&json!({
        "object": "whatsapp_business_account",
        "entry": [{ "id": "waba-111", "changes": [{ "field": "messages", "value": {
            "metadata": { "phone_number_id": "phone-222" },
            "messages": [{ "from": "not a number", "id": "wamid.X", "timestamp": "1", "type": "text" }]
        } }] }]
    }))
    .unwrap();
    assert!(matches!(
        hook.decode(&sign("s", &raw), &raw),
        Err(HookError::Malformed(_))
    ));
    // Other fields (account updates) carry no events.
    let raw = serde_json::to_vec(&json!({
        "object": "whatsapp_business_account",
        "entry": [{ "id": "waba-111", "changes": [{ "field": "account_update", "value": {} }] }]
    }))
    .unwrap();
    assert_eq!(hook.decode(&sign("s", &raw), &raw).unwrap(), vec![]);
}

#[test]
fn the_subscription_check_answers_only_with_the_verify_token() {
    let hook = Hook::new(setup("s", "t"));
    let q = |token: &str, challenge: &str| {
        BTreeMap::from([
            ("hub.mode".to_string(), "subscribe".to_string()),
            ("hub.verify_token".to_string(), token.to_string()),
            ("hub.challenge".to_string(), challenge.to_string()),
        ])
    };
    assert_eq!(
        hook.challenge(&q("test-verify-token", "1158201444"))
            .unwrap(),
        "1158201444"
    );
    assert!(matches!(
        hook.challenge(&q("wrong", "1158201444")),
        Err(HookError::Unverified(_))
    ));
    assert!(matches!(
        hook.challenge(&q("test-verify-token", "<script>")),
        Err(HookError::Malformed(_))
    ));
}

#[tokio::test]
async fn connecting_checks_the_setup_and_rotating_a_secret_keeps_the_route() {
    let (base, _) = server(graph("t1")).await;
    let dir = tempfile::tempdir().unwrap();
    let c = conns(dir.path(), &base);
    let wa = c
        .status(&[])
        .into_iter()
        .find(|s| s.id == "whatsapp")
        .unwrap();
    assert_eq!(wa.state, State::Connect);
    let (account, hookup) = c.connect_whatsapp(setup("secret-1", "t1")).await.unwrap();
    assert_eq!(account.id, "whatsapp:waba-111/phone-222");
    assert_eq!(account.name, "+34900000000");
    let wa = c
        .status(&[])
        .into_iter()
        .find(|s| s.id == "whatsapp")
        .unwrap();
    assert_eq!(
        (wa.state, wa.account.as_deref()),
        (State::Ready, Some("+34900000000"))
    );
    let route = hookup
        .path
        .strip_prefix("/hooks/whatsapp/")
        .unwrap()
        .to_string();
    assert_eq!(route.len(), 32);

    let feeds = c.event_sources();
    assert_eq!(feeds.len(), 1);
    assert_eq!(
        (feeds[0].source.as_str(), feeds[0].account.as_str()),
        ("whatsapp", "waba-111/phone-222")
    );
    assert!(feeds[0].poll.is_none(), "pushed, not polled");
    assert_eq!(feeds[0].actions, vec!["whatsapp.reply".to_string()]);

    assert!(c.webhook("whatsapp", "not-the-route").is_none());
    assert!(c.webhook("gmail", &route).is_none());
    let (_, raw) = fixture();
    let hook = c.webhook("whatsapp", &route).unwrap();
    assert!(hook.decode(&sign("secret-1", &raw), &raw).is_ok());

    // A new app secret: the same route, and only the new secret verifies.
    let (again, hookup2) = c.connect_whatsapp(setup("secret-2", "t1")).await.unwrap();
    assert_eq!(again.id, account.id);
    assert_eq!(hookup2, hookup);
    assert_eq!(c.accounts().len(), 1);
    let hook = c.webhook("whatsapp", &route).unwrap();
    assert!(matches!(
        hook.decode(&sign("secret-1", &raw), &raw),
        Err(HookError::Unverified(_))
    ));
    assert!(hook.decode(&sign("secret-2", &raw), &raw).is_ok());

    c.disconnect("whatsapp").await.unwrap();
    assert!(c.webhook("whatsapp", &route).is_none());
    assert!(c.event_sources().is_empty());
}

#[tokio::test]
async fn a_setup_that_graph_does_not_confirm_is_not_saved() {
    let cases: Vec<(Route, &str)> = vec![
        (
            Arc::new(|_, _, _| {
                Some((
                    401,
                    json!({ "error": { "message": "Invalid OAuth access token" } }),
                ))
            }),
            "refused the access token",
        ),
        (
            Arc::new(|_, _, _| {
                Some((
                    400,
                    json!({ "error": { "message": "Error validating access token", "code": 190 } }),
                ))
            }),
            "refused the access token",
        ),
        (
            Arc::new(|_, path, _| {
                Some(match path {
                    p if p.starts_with("/phone-222?") => {
                        (200, json!({ "display_phone_number": "1" }))
                    }
                    p if p.starts_with("/waba-111/phone_numbers") => {
                        (200, json!({ "data": [{ "id": "phone-333" }] }))
                    }
                    _ => (200, json!({ "data": [{}] })),
                })
            }),
            "is not in WhatsApp Business Account",
        ),
        (
            Arc::new(|_, path, _| {
                Some(match path {
                    p if p.starts_with("/phone-222?") => {
                        (200, json!({ "display_phone_number": "1" }))
                    }
                    p if p.starts_with("/waba-111/phone_numbers") => {
                        (200, json!({ "data": [{ "id": "phone-222" }] }))
                    }
                    _ => (200, json!({ "data": [] })),
                })
            }),
            "subscribe your Meta app",
        ),
    ];
    for (route, says) in cases {
        let (base, _) = server(route).await;
        let dir = tempfile::tempdir().unwrap();
        let c = conns(dir.path(), &base);
        let e = c
            .connect_whatsapp(setup("s", "t"))
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains(says), "{e}");
        assert!(c.accounts().is_empty());
    }
    let dir = tempfile::tempdir().unwrap();
    let c = conns(dir.path(), "http://127.0.0.1:9");
    let mut bad = setup("s", "t");
    bad.phone_id = "../me".into();
    assert!(c.connect_whatsapp(bad).await.is_err());
}

/// A connected number whose sends get `send` (`None`: no answer).
async fn connected(send: Option<(u16, Value)>) -> (tempfile::TempDir, Connections, Seen) {
    let setup_routes = graph("t");
    let route: Route = Arc::new(move |m, path, body| {
        if path == "/phone-222/messages" {
            return send.clone();
        }
        setup_routes(m, path, body)
    });
    let (base, seen) = server(route).await;
    let dir = tempfile::tempdir().unwrap();
    let c = conns(dir.path(), &base);
    c.connect_whatsapp(setup("s", "t")).await.unwrap();
    (dir, c, seen)
}

fn sends(seen: &Seen) -> Vec<Value> {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|(_, p, _)| p == "/phone-222/messages")
        .map(|(_, _, b)| serde_json::from_str(b).unwrap())
        .collect()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn reply_tools(c: &Connections) -> Arc<dyn ark_tools::Adapter> {
    c.adapters()
        .into_iter()
        .find(|a| a.name() == "whatsapp")
        .unwrap()
}

async fn reply(c: &Connections, until: u64) -> Result<ark_tools::ActionOutput, String> {
    reply_tools(c)
        .run(
            "whatsapp.reply",
            json!({ "to": "34600000001", "text": "The large box is 12 euros.", "reply_until_ms": until.to_string(), "message_id": "wamid.IN1" }),
            &RunCtx::new(&NoBlobs),
        )
        .await
        .map_err(|e| e.to_string())
}

#[tokio::test]
async fn a_reply_goes_to_the_customer_inside_the_window_quoting_their_message() {
    let (_d, c, seen) =
        connected(Some((200, json!({ "messages": [{ "id": "wamid.OUT1" }] })))).await;
    let out = reply(&c, now_ms() + 60_000).await.unwrap();
    assert_eq!(out.output["sent"], true);
    assert_eq!(out.output["id"], "wamid.OUT1");
    let sent = sends(&seen);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["to"], "34600000001");
    assert_eq!(sent[0]["text"]["body"], "The large box is 12 euros.");
    assert_eq!(sent[0]["context"]["message_id"], "wamid.IN1");
}

#[tokio::test]
async fn after_the_window_a_reply_fails_closed_and_nothing_is_sent() {
    let (_d, c, seen) = connected(Some((200, json!({ "messages": [{ "id": "x" }] })))).await;
    let e = reply(&c, now_ms() - 1).await.err().unwrap();
    assert!(e.contains("Reply window ended"), "{e}");
    assert!(e.contains("approved template"), "{e}");
    // A deadline further than 24 hours ahead did not come from a customer's message.
    let e = reply(&c, now_ms() + 25 * 3_600_000).await.err().unwrap();
    assert!(e.contains("more than 24 hours ahead"), "{e}");
    assert!(sends(&seen).is_empty());

    // Meta closed the window first (clock skew): its re-engagement refusal says the same.
    let (_d, c, _) = connected(Some((
        400,
        json!({ "error": { "message": "Re-engagement message", "code": 131047 } }),
    )))
    .await;
    let e = reply(&c, now_ms() + 60_000).await.err().unwrap();
    assert!(e.contains("Reply window ended"), "{e}");
}

#[tokio::test]
async fn a_send_without_an_answer_or_with_a_server_error_is_unconfirmed_and_not_retried() {
    for send in [None, Some((503, json!({ "error": { "message": "busy" } })))] {
        let (_d, c, seen) = connected(send.clone()).await;
        let out = reply(&c, now_ms() + 60_000).await.unwrap();
        let effect = out.effect.expect("an unconfirmed effect");
        assert_eq!(
            effect.state,
            ark_core::ExternalEffectState::Unconfirmed,
            "{send:?}"
        );
        assert!(!effect.retry_allowed);
        assert!(effect
            .user_message
            .unwrap()
            .contains("may or may not have been sent"));
        assert_eq!(sends(&seen).len(), 1, "sent once, not again");
    }
    // A refusal is certain: an error, and nothing may have been sent.
    let (_d, c, _) = connected(Some((
        400,
        json!({ "error": { "message": "bad number", "code": 100 } }),
    )))
    .await;
    let e = reply(&c, now_ms() + 60_000).await.err().unwrap();
    assert!(e.contains("bad number"), "{e}");
}

#[tokio::test]
async fn a_reply_takes_only_a_number_and_text_of_meta_size() {
    let (_d, c, seen) = connected(Some((200, json!({ "messages": [{ "id": "x" }] })))).await;
    let until = (now_ms() + 60_000).to_string();
    for input in [
        json!({ "to": "34600000001,34600000002", "text": "hi", "reply_until_ms": until }),
        json!({ "to": "lara@example.com", "text": "hi", "reply_until_ms": until }),
        json!({ "to": "34600000001", "text": " ", "reply_until_ms": until }),
        json!({ "to": "34600000001", "text": "x".repeat(4097), "reply_until_ms": until }),
        json!({ "to": "34600000001", "text": "hi" }),
    ] {
        assert!(
            reply_tools(&c)
                .run("whatsapp.reply", input.clone(), &RunCtx::new(&NoBlobs))
                .await
                .is_err(),
            "{input}"
        );
    }
    assert!(sends(&seen).is_empty());
}
