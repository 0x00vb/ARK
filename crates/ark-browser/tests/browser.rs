//! The Browser capability against the Chromium-family browser on this machine and a small site
//! on loopback. Skipped when no such browser is installed.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use ark_browser::{distill, BrowserService, CLICK, OPEN, READ, SCREENSHOT, TYPE};
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

const SITE_CONNECTIONS: usize = 16;
const SITE_HEADER_BYTES: usize = 8192;
const SITE_REQUEST_LIMIT: Duration = Duration::from_secs(5);

struct Site {
    url: String,
    addr: SocketAddr,
    first_accepted: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    connection_errors: Arc<[std::sync::atomic::AtomicUsize; 2]>,
}

impl Drop for Site {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            // A closed receiver is reported by joining its failed server thread below.
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            if let Err(error) = thread.join() {
                if !std::thread::panicking() {
                    std::panic::resume_unwind(error);
                }
            }
        }
        let expired = self.connection_errors[0].load(std::sync::atomic::Ordering::Relaxed);
        let disconnected = self.connection_errors[1].load(std::sync::atomic::Ordering::Relaxed);
        if expired != 0 || disconnected != 0 {
            eprintln!("fixture connections: {expired} expired, {disconnected} disconnected");
        }
    }
}

/// A site: a form that greets, sets a cookie, and a page that shows the cookies it got.
fn site() -> Site {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let (stop, mut stopped) = tokio::sync::oneshot::channel();
    let first_accepted = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let accepted = first_accepted.clone();
    let connection_errors: Arc<[std::sync::atomic::AtomicUsize; 2]> =
        Arc::new(std::array::from_fn(|_| {
            std::sync::atomic::AtomicUsize::new(0)
        }));
    let errors = connection_errors.clone();
    let thread = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            let mut requests = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    biased;
                    _ = &mut stopped => break,
                    completed = requests.join_next(), if !requests.is_empty() => {
                        completed.unwrap().unwrap();
                    }
                    connection = listener.accept(), if requests.len() < SITE_CONNECTIONS => {
                        let (stream, _) = connection.unwrap();
                        *accepted.0.lock().unwrap() = true;
                        accepted.1.notify_all();
                        let errors = errors.clone();
                        requests.spawn(async move {
                            match tokio::time::timeout(SITE_REQUEST_LIMIT, serve_site(stream)).await {
                                Ok(Ok(())) => (),
                                Err(_) => { errors[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
                                Ok(Err(error)) if matches!(error.kind(),
                                    std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset) => {
                                    errors[1].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                }
                                Ok(Err(error)) => panic!("fixture request failed: {error}"),
                            }
                        });
                    }
                }
            }
            requests.abort_all();
            while let Some(completed) = requests.join_next().await {
                if let Err(error) = completed {
                    assert!(error.is_cancelled(), "fixture request panicked: {error}");
                }
            }
        });
    });
    Site {
        url: format!("http://{addr}"),
        addr,
        first_accepted,
        stop: Some(stop),
        thread: Some(thread),
        connection_errors,
    }
}

async fn serve_site(mut stream: tokio::net::TcpStream) -> std::io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = [0u8; SITE_HEADER_BYTES];
    let mut used = 0;
    while !buf[..used].windows(4).any(|bytes| bytes == b"\r\n\r\n") {
        if used == buf.len() {
            return stream.write_all(b"HTTP/1.1 431 Request Header Fields Too Large\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").await;
        }
        let read = stream.read(&mut buf[used..]).await?;
        if read == 0 {
            return Ok(());
        }
        used += read;
    }
    let req =
        match std::str::from_utf8(&buf[..used]) {
            Ok(req) => req,
            Err(_) => return stream
                .write_all(
                    b"HTTP/1.1 400 Bad Request\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .await,
        };
    let path = req.split_whitespace().nth(1).unwrap_or("/");
    let cookie = req
        .lines()
        .find_map(|line| {
            line.strip_prefix("Cookie: ")
                .or_else(|| line.strip_prefix("cookie: "))
        })
        .unwrap_or("none");
    let (extra, body) = if path.starts_with("/popup") {
        (
            "",
            "<title>Opening</title><button style='width:180px;height:80px' onclick=\"window.open('/hello?name=Popup')\">Open popup</button>".to_string(),
        )
    } else if let Some(q) = path.strip_prefix("/hello?name=") {
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
            "<title>Greeter</title><main><h1>Greeter</h1><form action=\"/hello\"><label for=n>Your name</label><input autofocus oninput=\"document.title=this.value\" id=n name=name style=\"position:fixed;left:10px;top:10px;width:180px;height:32px\"><button>Greet me</button></form><a href=\"/whoami\">Who am I</a></main>".to_string(),
        )
    };
    let reply = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(reply.as_bytes()).await
}

#[test]
fn an_idle_site_socket_does_not_block_another_request() {
    let server = site();
    let mut idle = TcpStream::connect(server.addr).unwrap();
    idle.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    idle.set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let accepted = server.first_accepted.0.lock().unwrap();
    let (accepted, timeout) = server
        .first_accepted
        .1
        .wait_timeout_while(accepted, Duration::from_secs(2), |accepted| !*accepted)
        .unwrap();
    assert!(
        *accepted && !timeout.timed_out(),
        "first idle socket was not accepted"
    );
    drop(accepted);

    let mut second = TcpStream::connect(server.addr).unwrap();
    second
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    second
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    second
        .write_all(b"GET /hello?name=Second HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    second.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response}");
    assert!(response.contains("Hello, Second"), "{response}");

    idle.write_all(b"GET /hello?name=Idle HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    response.clear();
    idle.read_to_string(&mut response).unwrap();
    assert!(
        response.contains("Hello, Idle"),
        "the first socket stayed open: {response}"
    );
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
    let server = site();
    let base = server.url.clone();
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
async fn a_person_surface_has_its_own_context_frames_navigation_and_popups() {
    let dir = tempfile::tempdir().unwrap();
    let svc = BrowserService::new(dir.path()).allow_private(true);
    if svc.browser().is_none() {
        eprintln!("skipped: no Chrome, Edge or Brave on this machine");
        return;
    }
    let server = site();
    let base = server.url.clone();
    let first = svc.surface("chat-a");
    let second = svc.surface("chat-b");
    first.resize(640, 480, 1.0).await.unwrap();
    first.open(&base).await.unwrap();
    let frame = first.snapshot(640, 480).await.unwrap();
    assert_eq!(frame.title, "Greeter");
    assert_eq!(frame.media_type, "image/jpeg");
    assert!(frame.image.len() > 100, "a rendered frame is returned");

    second.open(&format!("{base}/whoami")).await.unwrap();
    assert_eq!(second.snapshot(640, 480).await.unwrap().title, "Who");
    assert_eq!(first.snapshot(640, 480).await.unwrap().title, "Greeter");

    first
        .open(&format!("{base}/hello?name=Surface"))
        .await
        .unwrap();
    assert_eq!(first.snapshot(640, 480).await.unwrap().title, "Hello");
    first.back().await.unwrap();
    for _ in 0..20 {
        if first.snapshot(640, 480).await.unwrap().title == "Greeter" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(first.snapshot(640, 480).await.unwrap().title, "Greeter");
    first.open(&base).await.unwrap();
    for _ in 0..20 {
        if !first.snapshot(640, 480).await.unwrap().loading {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    first.text("Keys").await.unwrap();
    assert_eq!(first.snapshot(640, 480).await.unwrap().title, "Keys");
    first.key("keyDown", "Enter", "Enter", 0).await.unwrap();
    first.key("keyUp", "Enter", "Enter", 0).await.unwrap();
    for _ in 0..20 {
        if first.snapshot(640, 480).await.unwrap().title == "Hello" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(first.snapshot(640, 480).await.unwrap().title, "Hello");

    first.open(&format!("{base}/popup")).await.unwrap();
    first
        .pointer("mousePressed", 60.0, 40.0, "left", 1)
        .await
        .unwrap();
    first
        .pointer("mouseReleased", 60.0, 40.0, "left", 1)
        .await
        .unwrap();
    let mut title = String::new();
    for _ in 0..40 {
        title = first.snapshot(640, 480).await.unwrap().title;
        if title == "Hello" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(title, "Hello", "the newest popup replaces the tracked page");
    first.stop().await.unwrap();
    first.reload().await.unwrap();
    first.close().await.unwrap();
    second.close().await.unwrap();
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

/// A page server: `/` links to `other`, `/away` redirects there (302), anything else says `name`.
fn pages(name: &'static str, other: Arc<std::sync::Mutex<String>>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            let mut buf = [0u8; 8192];
            let n = s.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            let other = other.lock().unwrap().clone();
            let reply = if path == "/later" {
                let body = format!(
                    "<title>later</title><script>setTimeout(() => location = \"{other}\", 4000)</script>"
                );
                format!("HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
            } else if path == "/away" {
                format!("HTTP/1.1 302 Found\r\nlocation: {other}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
            } else {
                let body = format!(
                    "<title>{name}</title><h1>This is {name}</h1><a href=\"{other}\">Elsewhere</a>"
                );
                format!("HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
            };
            let _ = s.write_all(reply.as_bytes());
        }
    });
    port
}

/// The pages a project's own processes serve open (ADR 0046); nothing else on this computer
/// does, also when a link, a redirect or Back leads there, nor in the signed-in browser.
#[tokio::test]
async fn a_project_opens_the_pages_its_processes_serve_and_nothing_else_here() {
    let dir = tempfile::tempdir().unwrap();
    let svc = BrowserService::new(dir.path().join("b"));
    if svc.browser().is_none() {
        eprintln!("skipped: no Chromium-family browser");
        return;
    }
    let to_theirs = Arc::new(std::sync::Mutex::new(String::new()));
    let ours = pages("ours", to_theirs.clone());
    let theirs = pages("theirs", Arc::new(std::sync::Mutex::new("/".into())));
    *to_theirs.lock().unwrap() = format!("http://127.0.0.1:{theirs}/");
    let project = dir.path().join("project");
    let p = project.clone();
    assert!(svc.serve_local_pages(Arc::new(move |ws| if ws == p {
        vec![ours]
    } else {
        vec![]
    })));
    assert!(
        !svc.serve_local_pages(Arc::new(|_| vec![])),
        "the first wins"
    );

    let agent = svc.for_owner("a");
    let in_project = RunCtx::new(&NoBlobs).with_workspace(project.clone());
    let elsewhere = RunCtx::new(&NoBlobs).with_workspace(dir.path().join("other"));
    let open = |url: String, ctx: &'static RunCtx<'static>| {
        let agent = agent.clone();
        async move { agent.run(OPEN, json!({ "url": url }), ctx).await }
    };
    let in_project: &'static RunCtx<'static> = Box::leak(Box::new(in_project));
    let elsewhere: &'static RunCtx<'static> = Box::leak(Box::new(elsewhere));

    let page = open(format!("http://localhost:{ours}/"), in_project)
        .await
        .unwrap();
    assert!(
        page.output["text"]
            .as_str()
            .unwrap()
            .contains("This is ours"),
        "{}",
        page.output
    );
    let page = open(format!("http://0.0.0.0:{ours}/"), in_project)
        .await
        .unwrap();
    assert!(page.output["text"]
        .as_str()
        .unwrap()
        .contains("This is ours"));
    for typed in [
        format!("http://127.0.0.1:{ours}/"),
        format!("localhost:{ours}"),
    ] {
        // The second as a person types it, without a scheme: http on this computer.
        let page = open(typed, in_project).await.unwrap();
        assert!(page.output["text"]
            .as_str()
            .unwrap()
            .contains("This is ours"));
    }

    let refused = |r: ark_tools::Result<ark_tools::ActionOutput>| {
        let e = r.err().expect("refused").to_string();
        assert!(e.contains("refused"), "{e}");
        e
    };
    let e = refused(open(format!("http://127.0.0.1:{theirs}/"), in_project).await);
    assert!(e.contains("process of this project serves itself"), "{e}");
    refused(open(format!("http://localhost:{ours}/"), elsewhere).await);
    let e = refused(
        agent
            .run(
                OPEN,
                json!({ "url": format!("http://localhost:{ours}/"), "signed_in": true }),
                in_project,
            )
            .await,
    );
    assert!(
        e.contains("signed-in browser opens public pages only"),
        "{e}"
    );

    // A redirect from our page to theirs: refused, and the page is left blank.
    refused(open(format!("http://localhost:{ours}/away"), in_project).await);
    let now = agent.run(READ, json!({}), in_project).await.unwrap();
    assert!(
        !now.output["text"].as_str().unwrap_or("").contains("theirs"),
        "{}",
        now.output
    );

    // A link to theirs, then Back into it from the blank page.
    let page = open(format!("http://localhost:{ours}/"), in_project)
        .await
        .unwrap();
    let link = element(&page.output, "link", "Elsewhere");
    refused(agent.run(CLICK, json!({ "ref": link }), in_project).await);
    refused(agent.run(ark_browser::BACK, json!({}), in_project).await);

    // A page that moves itself to theirs after it opened: a screenshot is refused with no
    // picture, and the page is left blank like after any other refusal.
    open(format!("http://localhost:{ours}/later"), in_project)
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(6000)).await;
    let e = refused(agent.run(SCREENSHOT, json!({}), in_project).await);
    assert!(e.contains("process of this project serves itself"), "{e}");
    let now = agent.run(READ, json!({}), in_project).await.unwrap();
    assert!(
        !now.output["text"].as_str().unwrap_or("").contains("theirs"),
        "{}",
        now.output
    );
}

#[test]
fn screenshot_is_listed_as_an_observe_action() {
    let dir = tempfile::tempdir().unwrap();
    let svc = BrowserService::new(dir.path());
    let actions = svc.for_owner("alice").actions();
    let shot = actions.iter().find(|a| a.name == SCREENSHOT).unwrap();
    assert_eq!(shot.risk, ark_core::RiskTier::Observe);
    assert!(shot.summary.contains("model provider"));
}

#[tokio::test]
async fn a_screenshot_of_a_local_page_is_a_jpeg() {
    let dir = tempfile::tempdir().unwrap();
    let svc = BrowserService::new(dir.path()).allow_private(true);
    if svc.browser().is_none() {
        eprintln!("skipped: no Chrome, Edge or Brave on this machine");
        return;
    }
    let server = site();
    let ctx = RunCtx::new(&NoBlobs);
    let alice = svc.for_owner("alice");
    alice
        .run(OPEN, json!({ "url": server.url.clone() }), &ctx)
        .await
        .map_err(|e| e.to_string())
        .unwrap();
    let out = alice
        .run(SCREENSHOT, json!({}), &ctx)
        .await
        .map_err(|e| e.to_string())
        .unwrap();
    assert_eq!(out.output["title"], "Greeter");
    assert_eq!(out.pictures.len(), 1);
    assert_eq!(out.pictures[0].media_type, "image/jpeg");
    assert!(out.pictures[0].bytes.starts_with(b"\xff\xd8\xff"));
}
