//! The Browser capability against the Chromium-family browser on this machine and a small site
//! on loopback. Skipped when no such browser is installed.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use ark_browser::{distill, BrowserService, CLICK, OPEN, READ, TYPE};
use ark_tools::{Adapter, BlobResolver, RunCtx};
use serde_json::{json, Value};

struct NoBlobs;
impl BlobResolver for NoBlobs {
    fn path(&self, _: &ark_core::ArtifactId) -> Option<std::path::PathBuf> {
        None
    }
    fn read(&self, id: &ark_core::ArtifactId) -> ark_tools::Result<Vec<u8>> {
        Err(ark_tools::AdapterError::ArtifactNotFound(id.to_string()))
    }
}

/// A site: a form that greets, sets a cookie, and a page that shows the cookies it got.
fn site() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            let mut buf = [0u8; 8192];
            let n = s.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            let cookie = req
                .lines()
                .find_map(|l| {
                    l.strip_prefix("Cookie: ")
                        .or_else(|| l.strip_prefix("cookie: "))
                })
                .unwrap_or("none")
                .to_string();
            let (extra, body) = if let Some(q) = path.strip_prefix("/hello?name=") {
                (
                    "Set-Cookie: seen=yes\r\n",
                    format!(
                        "<title>Hello</title><h1>Hello, {}</h1><a href=\"/\">Home</a>",
                        q.replace('+', " ")
                    ),
                )
            } else if path.starts_with("/whoami") {
                ("", format!("<title>Who</title><p>cookies: {cookie}</p>"))
            } else {
                (
                    "",
                    "<title>Greeter</title><main><h1>Greeter</h1><form action=\"/hello\"><label for=n>Your name</label><input id=n name=name><button>Greet me</button></form><a href=\"/whoami\">Who am I</a></main>".to_string(),
                )
            };
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    base
}

fn element(out: &Value, role: &str, name: &str) -> u64 {
    out["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["role"] == role && e["name"].as_str().unwrap_or("").contains(name))
        .unwrap_or_else(|| panic!("no {role} \"{name}\" in {out}"))["ref"]
        .as_u64()
        .unwrap()
}

#[tokio::test]
async fn agents_browse_apart_and_take_turns_on_the_signed_in_browser() {
    let dir = tempfile::tempdir().unwrap();
    let svc = BrowserService::new(dir.path())
        .allow_private(true)
        .signed_in_headless(true)
        .lease_idle(Duration::from_secs(1));
    if svc.browser().is_none() {
        eprintln!("skipped: no Chrome, Edge or Brave on this machine");
        return;
    }
    let base = site();
    let ctx = RunCtx::new(&NoBlobs);
    let alice = svc.for_owner("alice");
    let bob = svc.for_owner("bob");

    // Open, see the elements, type and submit, follow the result.
    let page = alice
        .run(OPEN, json!({ "url": base }), &ctx)
        .await
        .map_err(|e| e.to_string())
        .unwrap()
        .output;
    assert_eq!(page["title"], "Greeter");
    let field = element(&page, "textbox", "Your name");
    let (card, _) = distill(OPEN, &page).unwrap();
    assert!(card.starts_with("\"Greeter\" http://127.0.0.1:"), "{card}");
    assert!(
        card.contains(&format!("[{field}] textbox \"Your name\" = \"\"")),
        "{card}"
    );
    let done = alice
        .run(
            TYPE,
            json!({ "ref": field, "text": "Ada", "submit": true }),
            &ctx,
        )
        .await
        .map_err(|e| e.to_string())
        .unwrap()
        .output;
    assert!(
        done["text"].as_str().unwrap().contains("Hello, Ada"),
        "{done}"
    );
    let home = element(&done, "link", "Home");
    let back = alice
        .run(CLICK, json!({ "ref": home }), &ctx)
        .await
        .map_err(|e| e.to_string())
        .unwrap()
        .output;
    assert_eq!(back["title"], "Greeter");
    let again = alice
        .run(READ, json!({}), &ctx)
        .await
        .map_err(|e| e.to_string())
        .unwrap()
        .output;
    assert_eq!(again["url"], back["url"]);
    assert!(alice.run(CLICK, json!({ "ref": 999 }), &ctx).await.is_err());

    // Each agent has its own cookies.
    let who = |o: Value| o["text"].as_str().unwrap_or("").to_string();
    let a = alice
        .run(OPEN, json!({ "url": format!("{base}/whoami") }), &ctx)
        .await
        .map_err(|e| e.to_string())
        .unwrap()
        .output;
    let b = bob
        .run(OPEN, json!({ "url": format!("{base}/whoami") }), &ctx)
        .await
        .map_err(|e| e.to_string())
        .unwrap()
        .output;
    assert!(who(a).contains("seen=yes"));
    assert!(
        who(b).contains("cookies: none"),
        "bob shares nothing with alice"
    );

    // The signed-in browser: one at a time; the other waits until the first is idle.
    alice
        .run(OPEN, json!({ "url": base, "signed_in": true }), &ctx)
        .await
        .map_err(|e| e.to_string())
        .unwrap();
    assert_eq!(svc.signed_in_user().await.as_deref(), Some("alice"));
    let started = std::time::Instant::now();
    let b = bob
        .run(OPEN, json!({ "url": base, "signed_in": true }), &ctx)
        .await
        .map_err(|e| e.to_string())
        .unwrap()
        .output;
    assert!(
        started.elapsed() >= Duration::from_millis(800),
        "bob waited for alice"
    );
    assert_eq!(b["signed_in"], true);
    assert_eq!(svc.signed_in_user().await.as_deref(), Some("bob"));
}

#[tokio::test]
async fn this_machine_is_refused_unless_allowed() {
    let dir = tempfile::tempdir().unwrap();
    let svc = BrowserService::new(dir.path());
    let page = svc.for_owner("a");
    let e = page
        .run(
            OPEN,
            json!({ "url": "http://127.0.0.1:9/" }),
            &RunCtx::new(&NoBlobs),
        )
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(e.contains("refused"), "{e}");
    let _ = Arc::strong_count(&svc);
}
