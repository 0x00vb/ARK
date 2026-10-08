//! A chat runs on the computer the person picks (ADR 0052, step 1): this computer (A) places a
//! chat on the person's server (B). A keeps a record of it, shows its turns as they run, stops
//! and steers it, and answers its questions; B's own windows stay on their own chats; B's token
//! never leaves A's process; the chat stays listed while B does not answer and after A restarts.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ark_interact::App;
use ark_sdk::prelude::ark_model::{Completion, MockProvider, ToolCall, Usage};
use ark_sdk::prelude::*;
use ark_ui::access::Tokens;
use ark_ui::{Access, ArkUi, Host};
use serde_json::{json, Value};

struct Notes {
    saves: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Adapter for Notes {
    fn name(&self) -> &str {
        "notes"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new("notes.save", "save the notes", RiskTier::Mutate),
            ActionDescriptor::new("notes.read", "read the notes slowly", RiskTier::Read),
            ActionDescriptor::new("notes.jot", "jot a line in the project", RiskTier::Read),
        ]
    }
    async fn run(
        &self,
        action: &str,
        _: Value,
        ctx: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        if action == "notes.jot" {
            let dir = ctx.workspace.clone().expect("a project folder");
            std::fs::write(dir.join("jot.md"), "a line\n").unwrap();
            return Ok(ActionOutput::json(json!({ "ok": true })));
        }
        if action == "notes.read" {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            return Ok(ActionOutput::json(json!({ "notes": "three lines" })));
        }
        self.saves.fetch_add(1, Ordering::SeqCst);
        Ok(ActionOutput::json(json!({ "ok": true })))
    }
}

/// `who` answers "Hello from <who>."; asked to file the notes it calls `notes.save`; asked to
/// read slowly it calls `notes.read` and then says what it heard in between.
fn model(who: &'static str) -> Arc<MockProvider> {
    Arc::new(MockProvider::new("m", vec![]).with_fallback(move |req| {
        let said = |t: &str| req.messages.iter().any(|m| m.content.contains(t));
        let called = req.messages.iter().any(|m| m.tool_call_id.is_some());
        let call = |name: &str| ToolCall {
            id: "c".into(),
            name: name.into(),
            arguments: json!({}),
        };
        let (content, tool_calls) = if said("file the notes") && !called {
            (String::new(), vec![call("notes.save")])
        } else if said("jot it down") && !called {
            (String::new(), vec![call("notes.jot")])
        } else if said("read slowly") && !called {
            (String::new(), vec![call("notes.read")])
        } else if said("also the date") {
            ("Heard: also the date.".to_string(), vec![])
        } else if called {
            ("Done.".to_string(), vec![])
        } else {
            (format!("Hello from {who}."), vec![])
        };
        Completion {
            content,
            tool_calls,
            usage: Usage::default(),
        }
    }))
}

fn app(dir: &std::path::Path, who: &'static str, saves: Arc<AtomicUsize>) -> App {
    let provider: Arc<dyn ark_model::Provider> = model(who);
    let ark = Ark::builder()
        .store_root(dir)
        .tool(Arc::new(Notes { saves }))
        .profile(
            AgentProfile::builder()
                .core_actions(["notes.save", "notes.read", "notes.jot"])
                .build(),
        )
        .provider(provider)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    App::new("Test").ark(ark)
}

async fn until(what: &str, mut f: impl AsyncFnMut() -> bool) {
    for _ in 0..300 {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("never: {what}");
}

/// B: the person's server, with a token for A.
async fn server(dir: &std::path::Path, saves: Arc<AtomicUsize>) -> (Arc<Host>, u16, String) {
    let tokens = Tokens::in_store(dir);
    let token = tokens.create("laptop").unwrap();
    let (b, served) = ArkUi::new(app(dir, "B", saves))
        .access(Access::Tokens(tokens))
        .serve_background("127.0.0.1:0")
        .await
        .unwrap();
    (b, served.port, token)
}

async fn linked(
    dir: &std::path::Path,
    port: u16,
    token: &str,
    saves: Arc<AtomicUsize>,
) -> Arc<Host> {
    let (a, _) = ArkUi::new(app(dir, "A", saves))
        .link("vps", &format!("http://127.0.0.1:{port}"), token)
        .open()
        .await
        .unwrap();
    until("the link to answer", async || {
        a.links().state("vps") == Some(ark_interact::links::State::Ok)
    })
    .await;
    a
}

/// Every event A sends its windows, kept as it comes.
fn record(host: &Arc<Host>) -> Arc<std::sync::Mutex<Vec<Value>>> {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (mut rx, keep) = (host.subscribe(), seen.clone());
    tokio::spawn(async move {
        while let Ok(v) = rx.recv().await {
            keep.lock().unwrap().push(v);
        }
    });
    seen
}

fn finished(seen: &std::sync::Mutex<Vec<Value>>, session: &str) -> Option<Value> {
    seen.lock()
        .unwrap()
        .iter()
        .find(|v| v["kind"] == "turn_finished" && v["session"] == session)
        .cloned()
}

async fn session_of(host: &Arc<Host>) -> String {
    host.call("summary", json!({})).await.unwrap()["session"]["id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_placed_on_the_server_runs_there_and_shows_here() {
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (b, port, token) = server(dir_b.path(), Arc::new(AtomicUsize::new(0))).await;
    let a = linked(dir_a.path(), port, &token, Arc::new(AtomicUsize::new(0))).await;
    let seen = record(&a);
    let b_window = session_of(&b).await;
    let a_own = session_of(&a).await;

    let placed = a
        .call("new_session", json!({ "place": "vps" }))
        .await
        .unwrap();
    assert_eq!(placed["place"]["name"], "vps", "{placed}");
    let s = placed["session"]["id"].as_str().unwrap().to_string();
    assert_ne!(s, a_own);
    assert_eq!(session_of(&a).await, s, "A's windows show the placed chat");
    assert_eq!(session_of(&b).await, b_window, "B's windows did not move");

    a.call("send", json!({ "text": "hi" })).await.unwrap();
    until("the turn to finish on A", async || {
        finished(&seen, &s).is_some()
    })
    .await;
    let text: String = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|v| v["kind"] == "text" && v["session"] == s.as_str())
        .map(|v| v["text"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(text.contains("Hello from B."), "streamed from B: {text:?}");
    let done = finished(&seen, &s).unwrap();
    assert_eq!(done["ok"], true, "{done}");
    assert_eq!(done["summary"]["place"]["name"], "vps", "{done}");
    // Numbered into A's own stream: one sequence, no gaps from B's.
    let seqs: Vec<u64> = seen
        .lock()
        .unwrap()
        .iter()
        .filter_map(|v| v["seq"].as_u64())
        .collect();
    assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "{seqs:?}");

    let transcript = a.call("transcript", json!({})).await.unwrap();
    assert!(
        transcript.to_string().contains("Hello from B."),
        "{transcript}"
    );
    assert_eq!(
        session_of(&b).await,
        b_window,
        "B's windows still did not move"
    );

    // Listed with its place on A; on B it is one of B's chats.
    let rows = a.call("sessions", json!({})).await.unwrap();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == s.as_str())
        .unwrap_or_else(|| panic!("listed on A: {rows}"));
    assert_eq!(row["place"]["name"], "vps");
    assert_eq!(row["current"], true);
    assert!(rows
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["id"] == s.as_str() || r["current"] == false));
    let on_b = b.call("sessions", json!({ "all": true })).await.unwrap();
    assert!(on_b.to_string().contains(&s), "{on_b}");

    // B's own window works on its own chat meanwhile, and A's chat is untouched by it.
    b.call("send", json!({ "text": "hi" })).await.unwrap();
    let bseen = record(&b);
    until("B's own turn", async || {
        finished(&bseen, &b_window).is_some()
    })
    .await;
    assert!(finished(&seen, &b_window).is_none(), "A shows B's own chat");

    // Back to a chat of A's own, and to the placed one again.
    a.call("open_session", json!({ "id": a_own }))
        .await
        .unwrap();
    assert_eq!(session_of(&a).await, a_own);
    // The terminal lists it with its place and opens it the same way.
    let listed = a
        .call("command", json!({ "line": "/chats" }))
        .await
        .unwrap()
        .to_string();
    assert!(listed.contains("on vps"), "{listed}");
    a.call("command", json!({ "line": format!("/resume {}", &s[..8]) }))
        .await
        .unwrap();
    assert_eq!(session_of(&a).await, s, "/resume opened the placed chat");
    a.call("open_session", json!({ "id": a_own }))
        .await
        .unwrap();
    let again = a.call("open_session", json!({ "id": s })).await.unwrap();
    assert_eq!(again["place"]["name"], "vps");
    assert!(again["session"]["title"].as_str().is_some());

    // B's token is in nothing A answers or sends.
    for m in ["hello", "summary", "sessions", "transcript", "working"] {
        let out = a.call(m, json!({})).await.unwrap().to_string();
        assert!(!out.contains(&token), "{m} carries B's token");
    }
    let all = serde_json::to_string(&*seen.lock().unwrap()).unwrap();
    assert!(!all.contains(&token), "an event carries B's token");
    assert!(
        !in_files(dir_a.path(), &token),
        "B's token is in a file of A's store or log"
    );
    // Only the chat's calls cross: anything else on the held chat is refused here.
    let e = a
        .call("command", json!({ "line": "/model" }))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("runs on vps"), "{e}");
}

#[tokio::test(flavor = "multi_thread")]
async fn questions_from_both_computers_are_told_apart() {
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (saves_a, saves_b) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (_b, port, token) = server(dir_b.path(), saves_b.clone()).await;
    let a = linked(dir_a.path(), port, &token, saves_a.clone()).await;
    let seen = record(&a);

    // A's own chat asks first: its question is A's #1.
    let mine = session_of(&a).await;
    a.call("send", json!({ "text": "file the notes" }))
        .await
        .unwrap();
    until("A's own question", async || {
        seen.lock()
            .unwrap()
            .iter()
            .any(|v| v["kind"] == "approval" && v["session"] == mine.as_str())
    })
    .await;
    // Then a chat on B asks: B's #1 too.
    let placed = a
        .call("new_session", json!({ "place": "vps" }))
        .await
        .unwrap();
    let s = placed["session"]["id"].as_str().unwrap().to_string();
    a.call("send", json!({ "text": "file the notes" }))
        .await
        .unwrap();
    until("B's question on A", async || {
        seen.lock()
            .unwrap()
            .iter()
            .any(|v| v["kind"] == "approval" && v["session"] == s.as_str())
    })
    .await;
    let hello = a.call("hello", json!({})).await.unwrap();
    let ids: Vec<Value> = hello["approvals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|q| q["id"].clone())
        .collect();
    assert_eq!(ids.len(), 2, "{hello}");
    let theirs = ids.iter().find(|i| i.is_string()).unwrap().clone();
    let own = ids.iter().find(|i| i.is_number()).unwrap().clone();
    assert!(
        theirs.as_str().unwrap().ends_with(&format!(":{own}")),
        "{ids:?}"
    );
    assert_eq!(hello["summary"]["place"]["name"], "vps");

    // Answering B's question runs B's tool, not A's; A's question stays.
    a.call("approve", json!({ "id": theirs, "decision": "allow_once" }))
        .await
        .unwrap();
    until("B's save", async || saves_b.load(Ordering::SeqCst) == 1).await;
    until("B's turn to end", async || finished(&seen, &s).is_some()).await;
    assert_eq!(saves_a.load(Ordering::SeqCst), 0);
    let left = a.call("hello", json!({})).await.unwrap()["approvals"].clone();
    assert_eq!(left.as_array().unwrap().len(), 1, "{left}");
    assert_eq!(left[0]["id"], own);
    a.call("approve", json!({ "id": own, "decision": "allow_once" }))
        .await
        .unwrap();
    until("A's save", async || saves_a.load(Ordering::SeqCst) == 1).await;
    assert_eq!(saves_b.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_turn_on_the_server_is_steered_and_stopped_from_here() {
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (_b, port, token) = server(dir_b.path(), Arc::new(AtomicUsize::new(0))).await;
    let a = linked(dir_a.path(), port, &token, Arc::new(AtomicUsize::new(0))).await;
    let seen = record(&a);
    let s = a
        .call("new_session", json!({ "place": "vps" }))
        .await
        .unwrap()["session"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    a.call("send", json!({ "text": "read slowly" }))
        .await
        .unwrap();
    until("the turn to start", async || {
        a.call("working", json!({}))
            .await
            .unwrap()
            .to_string()
            .contains(&s)
    })
    .await;
    let w = a.call("working", json!({})).await.unwrap();
    assert_eq!(w[0]["place"], "vps", "{w}");
    let hello = a.call("hello", json!({})).await.unwrap();
    assert_eq!(hello["running"], "read slowly", "{hello}");
    a.call("steer", json!({ "text": "also the date" }))
        .await
        .unwrap();
    until("the turn to finish", async || finished(&seen, &s).is_some()).await;
    let text: String = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|v| v["kind"] == "text" && v["session"] == s.as_str())
        .map(|v| v["text"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(text.contains("Heard: also the date."), "{text:?}");

    seen.lock().unwrap().clear();
    a.call("send", json!({ "text": "read slowly" }))
        .await
        .unwrap();
    until("the turn to start", async || {
        seen.lock()
            .unwrap()
            .iter()
            .any(|v| v["kind"] == "turn_started" && v["session"] == s.as_str())
    })
    .await;
    a.call("cancel", json!({})).await.unwrap();
    until("the turn to stop", async || finished(&seen, &s).is_some()).await;
    assert_eq!(finished(&seen, &s).unwrap()["cancelled"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_placed_chat_stays_listed_while_the_server_is_away_and_after_a_restart() {
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    // B on a runtime of its own, so that it can go away.
    let (tx, rx) = std::sync::mpsc::channel();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let dir = dir_b.path().to_path_buf();
    let b = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let (_b, port, token) = server(&dir, Arc::new(AtomicUsize::new(0))).await;
            tx.send((port, token)).unwrap();
            stopped.await.ok();
        });
        rt.shutdown_background();
    });
    let (port, token) = rx.recv().unwrap();
    let a = linked(dir_a.path(), port, &token, Arc::new(AtomicUsize::new(0))).await;
    let seen = record(&a);
    let s = a
        .call("new_session", json!({ "place": "vps" }))
        .await
        .unwrap()["session"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    a.call("send", json!({ "text": "hi" })).await.unwrap();
    until("the turn", async || finished(&seen, &s).is_some()).await;

    stop.send(()).unwrap();
    b.join().unwrap();
    until("B to count as away", async || {
        a.links().state("vps") != Some(ark_interact::links::State::Ok)
    })
    .await;
    let rows = a.call("sessions", json!({})).await.unwrap();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == s.as_str())
        .cloned()
        .unwrap_or_else(|| panic!("still listed: {rows}"));
    assert_eq!(row["place"]["state"], "down", "{row}");
    let e = a
        .call("send", json!({ "text": "again" }))
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(e, "vps is not answering");
    assert!(!e.contains(&token));

    // A again, from its store: the chat is still listed and opens, reading only.
    drop(a);
    let (a, _) = ArkUi::new(app(dir_a.path(), "A", Arc::new(AtomicUsize::new(0))))
        .link("vps", &format!("http://127.0.0.1:{port}"), &token)
        .open()
        .await
        .unwrap();
    let rows = a.call("sessions", json!({})).await.unwrap();
    assert!(rows.to_string().contains(&s), "{rows}");
    let opened = a.call("open_session", json!({ "id": s })).await.unwrap();
    assert_eq!(opened["place"]["name"], "vps", "{opened}");
    assert!(a.call("send", json!({ "text": "x" })).await.is_err());
}

/// The folder of B's copy of A's project `name`: B's own window opens it to ask.
async fn copy_folder(b: &Arc<Host>, name: &str) -> std::path::PathBuf {
    let projects = b.call("projects", json!({})).await.unwrap();
    let id = projects
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == name)
        .unwrap_or_else(|| panic!("B has {name}: {projects}"))["id"]
        .clone();
    b.call("open_project", json!({ "id": id })).await.unwrap();
    let v = b.call("workspace", json!({})).await.unwrap();
    std::path::PathBuf::from(v["path"].as_str().unwrap())
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn the_project_goes_with_the_chat_and_its_changes_come_back() {
    use std::os::unix::fs::PermissionsExt;
    let (dir_a, dir_b, work) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    let w = work.path();
    std::fs::write(w.join("README.md"), "hello\n").unwrap();
    std::fs::write(w.join("run.sh"), "#!/bin/sh\necho hi\n").unwrap();
    std::fs::set_permissions(w.join("run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::create_dir_all(w.join("target/debug")).unwrap();
    std::fs::write(w.join("target/debug/big.bin"), "build output").unwrap();
    let (b, port, token) = server(dir_b.path(), Arc::new(AtomicUsize::new(0))).await;
    let a = linked(dir_a.path(), port, &token, Arc::new(AtomicUsize::new(0))).await;
    a.call("add_project", json!({ "name": "site", "path": w }))
        .await
        .unwrap();

    // The first chat there takes the project: modes kept, build output left here.
    let placed = a
        .call("new_session", json!({ "place": "vps" }))
        .await
        .unwrap();
    assert_eq!(placed["copy"]["sent"], 2, "{placed}");
    let there = copy_folder(&b, "site").await;
    assert_eq!(
        std::fs::read_to_string(there.join("README.md")).unwrap(),
        "hello\n"
    );
    let mode = std::fs::metadata(there.join("run.sh"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o111, 0o111, "+x kept: {mode:o}");
    assert!(!there.join("target").exists(), "build output stays here");

    // Work there: a file a process wrote, one in a hidden folder, a build, a link out, and a
    // file changed on both sides.
    std::fs::write(there.join("notes.txt"), "from the server\n").unwrap();
    std::fs::create_dir_all(there.join(".config")).unwrap();
    std::fs::write(there.join(".config/app.toml"), "x = 1\n").unwrap();
    std::fs::create_dir_all(there.join("target/release")).unwrap();
    std::fs::write(there.join("target/release/app"), "binary").unwrap();
    std::os::unix::fs::symlink("/etc", there.join("keys")).unwrap();
    std::fs::write(there.join("README.md"), "hello from vps\n").unwrap();
    std::fs::write(w.join("README.md"), "hello from this Mac\n").unwrap();

    let c = a.call("place_changes", json!({})).await.unwrap();
    let added = c["changes"]["added"].to_string();
    assert!(
        added.contains("notes.txt") && added.contains(".config/app.toml"),
        "{c}"
    );
    assert!(
        !c.to_string().contains("target/release"),
        "a build brings nothing: {c}"
    );
    assert_eq!(c["both"], json!(["README.md"]), "{c}");

    let r = a.call("place_bring", json!({})).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(w.join("notes.txt")).unwrap(),
        "from the server\n"
    );
    assert!(w.join(".config/app.toml").exists());
    assert!(
        w.join("keys").symlink_metadata().is_err(),
        "a link out is refused: {r}"
    );
    assert!(r["refused"].to_string().contains("keys"), "{r}");
    assert_eq!(
        r["waiting"],
        json!(["README.md"]),
        "both sides changed: asked"
    );
    assert_eq!(
        std::fs::read_to_string(w.join("README.md")).unwrap(),
        "hello from this Mac\n",
        "nothing replaced before the choice"
    );

    // Take vps's: this Mac's version is kept as a version before.
    let r = a
        .call(
            "place_bring",
            json!({ "choices": { "README.md": "theirs" } }),
        )
        .await
        .unwrap();
    assert_eq!(r["waiting"], json!([]), "{r}");
    assert_eq!(
        std::fs::read_to_string(w.join("README.md")).unwrap(),
        "hello from vps\n"
    );
    let kept = a.call("artifacts", json!({})).await.unwrap().to_string();
    assert!(
        kept.contains("README.md"),
        "the version before is kept: {kept}"
    );
    let c = a.call("place_changes", json!({})).await.unwrap();
    assert!(
        !c["both"].to_string().contains("README.md")
            && !c["changes"]["changed"].to_string().contains("README.md"),
        "not asked again: {c}"
    );

    // A later chat there carries only what changed here since, deletions too.
    std::fs::write(w.join("run.sh"), "#!/bin/sh\necho changed\n").unwrap();
    std::fs::remove_file(w.join("notes.txt")).unwrap();
    let again = a
        .call("new_session", json!({ "place": "vps" }))
        .await
        .unwrap();
    assert_eq!(again["copy"]["sent"], 2, "{again}");
    assert!(std::fs::read_to_string(there.join("run.sh"))
        .unwrap()
        .contains("changed"));
    assert!(!there.join("notes.txt").exists(), "the deletion went too");
}

/// The window (WebKit): an empty chat offers the server on a chip; the first message starts the
/// chat there; it is marked where it runs; a file it wrote comes back with one button; the chat
/// of a server that went away reads only. `ARK_UI_PAGE_TESTS=1`, as `tests/page.rs`;
/// `ARK_UI_SHOTS=<folder>` keeps pictures (light, dark, 390 wide).
#[tokio::test(flavor = "multi_thread")]
async fn the_window_places_a_chat_on_the_server() {
    if std::env::var_os("ARK_UI_PAGE_TESTS").is_none() {
        eprintln!("skipped: set ARK_UI_PAGE_TESTS=1 to run the page tests");
        return;
    }
    let (dir_a, dir_b, work) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    std::fs::write(work.path().join("README.md"), "hello\n").unwrap();
    let bin = tempfile::tempdir().unwrap();
    fake_sign_in(bin.path());
    let (tx, rx) = std::sync::mpsc::channel();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let dir = dir_b.path().to_path_buf();
    let b = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let (_b, port, token) = server(&dir, Arc::new(AtomicUsize::new(0))).await;
            tx.send((port, token)).unwrap();
            stopped.await.ok();
        });
        rt.shutdown_background();
    });
    let (port, token) = rx.recv().unwrap();
    let (a, served) = ArkUi::new(app(dir_a.path(), "A", Arc::new(AtomicUsize::new(0))))
        .link("vps", &format!("http://127.0.0.1:{port}"), &token)
        .serve_background("127.0.0.1:0")
        .await
        .unwrap();
    a.call(
        "add_project",
        json!({ "name": "site", "path": work.path() }),
    )
    .await
    .unwrap();
    until("the link to answer", async || {
        a.links().state("vps") == Some(ark_interact::links::State::Ok)
    })
    .await;
    let page = |scenario: &'static str| {
        let url = served.url.clone();
        async move {
            let out = tokio::process::Command::new("node")
                .arg("page.mjs")
                .arg(scenario)
                .arg(&url)
                .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/web"))
                .output()
                .await
                .expect("node is on PATH");
            let text = String::from_utf8_lossy(&out.stdout).to_string()
                + &String::from_utf8_lossy(&out.stderr);
            assert!(out.status.success(), "{scenario} failed:\n{text}");
        }
    };
    page("places").await;
    assert_eq!(
        std::fs::read_to_string(work.path().join("jot.md")).unwrap(),
        "a line\n",
        "brought to this Mac"
    );
    stop.send(()).unwrap();
    b.join().unwrap();
    until("the server to count as away", async || {
        a.links().state("vps") != Some(ark_interact::links::State::Ok)
    })
    .await;
    page("places_away").await;
}

/// Who answers on the server (ADR 0052 step 5): the model menu of a chat there lists the
/// server's models; a program's sign-in runs there in a terminal shown here, answered here; only
/// the catalog's programs and steps run.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_program_signs_in_on_the_server_from_here() {
    let (dir_a, dir_b, bin) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    fake_sign_in(bin.path());
    let (b, port, token) = server(dir_b.path(), Arc::new(AtomicUsize::new(0))).await;
    let a = linked(dir_a.path(), port, &token, Arc::new(AtomicUsize::new(0))).await;
    let seen = record(&a);
    a.call("new_session", json!({ "place": "vps" }))
        .await
        .unwrap();
    // B's own shell is never another runtime's to type into; a stream id is never a path.
    let own = b.call("term_open", json!({})).await.unwrap()["session"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(b
        .call("chat_term_write", json!({ "id": own, "data": "ls\n" }))
        .await
        .is_err());
    b.call("term_close", json!({ "session": own }))
        .await
        .unwrap();
    assert!(b
        .call("copy_part", json!({ "stream": "../../etc/passwd" }))
        .await
        .is_err());

    let models = a.call("models", json!({})).await.unwrap();
    let claude = models
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["provider"] == "claude")
        .unwrap_or_else(|| panic!("{models}"))
        .clone();
    assert_eq!(claude["ready"], false, "not signed in there: {claude}");

    let opened = a
        .call(
            "place_setup",
            json!({ "program": "claude", "step": "sign_in" }),
        )
        .await
        .unwrap();
    let id = opened["session"].as_str().unwrap().to_string();
    let out = || {
        seen.lock()
            .unwrap()
            .iter()
            .filter(|v| v["kind"] == "term" && v["session"] == id.as_str())
            .map(|v| v["data"].as_str().unwrap_or("").to_string())
            .collect::<String>()
    };
    until("the link", async || out().contains("Paste code here")).await;
    assert!(out().contains("https://example.invalid/login"));
    a.call("term_write", json!({ "session": id, "data": "abc-123\n" }))
        .await
        .unwrap();
    until("the answer", async || {
        out().contains("signed in with abc-123")
    })
    .await;
    a.call("term_close", json!({ "session": id }))
        .await
        .unwrap();
    // What was typed and shown there is kept on neither computer.
    for dir in [dir_a.path(), dir_b.path()] {
        assert!(
            !in_files(dir, "abc-123"),
            "the sign-in is in {}",
            dir.display()
        );
    }

    // Only the catalog's programs and steps; never another terminal there.
    for (program, step) in [("rm", "install"), ("claude", "shell")] {
        assert!(a
            .call("place_setup", json!({ "program": program, "step": step }))
            .await
            .is_err());
    }
    assert!(a
        .call(
            "term_write",
            json!({ "session": "term-other", "data": "ls\n" })
        )
        .await
        .is_err());
    // No key here to send, and the server keeps none: said in words.
    let e = a
        .call("place_key", json!({ "provider": "openai" }))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("no openai key is saved here"), "{e}");
}

/// A stand-in for Claude Code's own sign-in in `dir`, used by the server: a link, then the code
/// pasted back.
#[cfg(unix)]
fn fake_sign_in(dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let fake = dir.join("claude");
    std::fs::write(
        &fake,
        "#!/bin/sh\nif [ \"$1\" = auth ] && [ \"$2\" = login ]; then\n  echo 'visit https://example.invalid/login'\n  printf 'Paste code here > '\n  read code\n  echo \"signed in with $code\"\nelse\n  echo '{\"loggedIn\": false}'\nfi\n",
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("ARK_CLAUDE_BIN", &fake);
}

/// The app's program on a computer, for the window's Settings › Computers test: never installed.
struct NoProgram;

impl ark_ui::ark_reach::ServeContract for NoProgram {
    fn name(&self) -> &str {
        "test"
    }
    fn serve_args(&self, _: &str) -> Vec<String> {
        Vec::new()
    }
    fn token_args(&self, _: &str) -> Vec<String> {
        Vec::new()
    }
    fn revoke_args(&self, _: &str) -> Vec<String> {
        Vec::new()
    }
    fn version_args(&self) -> Vec<String> {
        Vec::new()
    }
}

/// Settings › Computers (WebKit): the hosts of the `ssh` config, a setup that cannot connect says
/// which step and offers Try again; an address asks for a code instead.
#[tokio::test(flavor = "multi_thread")]
async fn the_window_adds_a_computer() {
    if std::env::var_os("ARK_UI_PAGE_TESTS").is_none() {
        eprintln!("skipped: set ARK_UI_PAGE_TESTS=1 to run the page tests");
        return;
    }
    let (dir_a, ssh) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let cfg = ssh.path().join("config");
    std::fs::write(
        &cfg,
        "Host closed-box\n  HostName 127.0.0.1\n  Port 9\n  User nobody\n  ConnectTimeout 2\n",
    )
    .unwrap();
    let (_a, served) = ArkUi::new(app(dir_a.path(), "A", Arc::new(AtomicUsize::new(0))))
        .computers(ark_ui::computers::ComputerSetup {
            contract: Arc::new(NoProgram),
            build: Arc::new(|_| anyhow::bail!("no build")),
            secrets: Arc::new(ark_sdk::prelude::ark_connect::MemorySecrets::default()),
            ssh_config: Some(cfg),
        })
        .serve_background("127.0.0.1:0")
        .await
        .unwrap();
    let out = tokio::process::Command::new("node")
        .arg("page.mjs")
        .arg("computers")
        .arg(&served.url)
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/web"))
        .output()
        .await
        .expect("node is on PATH");
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "computers failed:\n{text}");
}

/// Whether any file under `dir` holds `needle` (a store, a log).
fn in_files(dir: &std::path::Path, needle: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|e| {
        let p = e.path();
        if p.is_dir() {
            in_files(&p, needle)
        } else {
            std::fs::read(&p)
                .map(|b| b.windows(needle.len()).any(|w| w == needle.as_bytes()))
                .unwrap_or(false)
        }
    })
}

/// A chat started while work runs in the copy there: nothing is sent into the copy under that
/// work, and the answer says so (the next chat sends).
#[tokio::test(flavor = "multi_thread")]
async fn a_send_waits_for_work_in_the_copy() {
    let (dir_a, dir_b, work) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    std::fs::write(work.path().join("README.md"), "hello\n").unwrap();
    let (_b, port, token) = server(dir_b.path(), Arc::new(AtomicUsize::new(0))).await;
    let a = linked(dir_a.path(), port, &token, Arc::new(AtomicUsize::new(0))).await;
    let seen = record(&a);
    a.call(
        "add_project",
        json!({ "name": "site", "path": work.path() }),
    )
    .await
    .unwrap();
    let first = a
        .call("new_session", json!({ "place": "vps" }))
        .await
        .unwrap();
    let s = first["session"]["id"].as_str().unwrap().to_string();
    a.call("send", json!({ "text": "read slowly" }))
        .await
        .unwrap();
    until("the turn to start", async || {
        seen.lock()
            .unwrap()
            .iter()
            .any(|v| v["kind"] == "turn_started" && v["session"] == s.as_str())
    })
    .await;
    std::fs::write(work.path().join("README.md"), "changed here\n").unwrap();
    let second = a
        .call("new_session", json!({ "place": "vps" }))
        .await
        .unwrap();
    assert_eq!(second["copy"]["busy"], true, "{second}");
    until("the first turn", async || finished(&seen, &s).is_some()).await;
    a.call("open_session", json!({ "id": s })).await.unwrap();
    let third = a
        .call("new_session", json!({ "place": "vps" }))
        .await
        .unwrap();
    assert_eq!(third["copy"]["sent"], 1, "{third}");
}

/// A chat keeps the model chosen for it after a turn, whatever model the server's own window
/// uses: the computer opens the chat again with it.
#[tokio::test(flavor = "multi_thread")]
async fn a_held_chat_keeps_its_model_after_a_turn() {
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (_b, port, token) = server(dir_b.path(), Arc::new(AtomicUsize::new(0))).await;
    let a = linked(dir_a.path(), port, &token, Arc::new(AtomicUsize::new(0))).await;
    let seen = record(&a);
    let placed = a
        .call(
            "new_session",
            json!({ "place": "vps", "model": "ollama:chosen-model" }),
        )
        .await
        .unwrap();
    let s = placed["session"]["id"].as_str().unwrap().to_string();
    assert!(
        placed["model"].to_string().contains("chosen-model"),
        "{placed}"
    );
    // The model is not there: the turn fails, and ends.
    a.call("send", json!({ "text": "hi" })).await.unwrap();
    until("the turn", async || finished(&seen, &s).is_some()).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let after = a.call("summary", json!({})).await.unwrap();
    assert!(
        after["model"].to_string().contains("chosen-model"),
        "{after}"
    );
}

/// Removing a computer while its chat is on screen and asks: the chat leaves the screen and its
/// question the tray, and nothing typed goes into another chat.
#[tokio::test(flavor = "multi_thread")]
async fn removing_a_computer_takes_its_chat_and_questions_away() {
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (_b, port, token) = server(dir_b.path(), Arc::new(AtomicUsize::new(0))).await;
    let (a, _) = ArkUi::new(app(dir_a.path(), "A", Arc::new(AtomicUsize::new(0))))
        .computers(ark_ui::computers::ComputerSetup {
            contract: Arc::new(NoProgram),
            build: Arc::new(|_| anyhow::bail!("no build")),
            secrets: Arc::new(ark_sdk::prelude::ark_connect::MemorySecrets::default()),
            ssh_config: None,
        })
        .keep_computer("vps", &format!("http://127.0.0.1:{port}"), &token)
        .open()
        .await
        .unwrap();
    until("the link to answer", async || {
        a.links().state("vps") == Some(ark_interact::links::State::Ok)
    })
    .await;
    let seen = record(&a);
    let s = a
        .call("new_session", json!({ "place": "vps" }))
        .await
        .unwrap()["session"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    a.call("send", json!({ "text": "file the notes" }))
        .await
        .unwrap();
    until("the question", async || {
        !a.call("hello", json!({})).await.unwrap()["approvals"]
            .as_array()
            .unwrap()
            .is_empty()
    })
    .await;
    a.call("computer_remove", json!({ "name": "vps" }))
        .await
        .unwrap();
    let hello = a.call("hello", json!({})).await.unwrap();
    assert_eq!(hello["approvals"], json!([]), "{hello}");
    assert!(hello["summary"]["place"].is_null(), "{hello}");
    assert_ne!(hello["summary"]["session"]["id"], s.as_str());
    assert!(seen
        .lock()
        .unwrap()
        .iter()
        .any(|v| v["kind"] == "approval_closed"));
    assert!(a.links().state("vps").is_none());
    // An app that moved it here does not move it again.
    assert!(ark_ui::computers::kept(dir_a.path(), "vps"));
    let rows = a.call("sessions", json!({ "all": true })).await.unwrap();
    assert!(!rows.to_string().contains(&s), "{rows}");
}
