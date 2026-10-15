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

fn app_with_computer(
    dir: &std::path::Path,
    who: &'static str,
    saves: Arc<AtomicUsize>,
    screen: ark_computer::fake::FakeScreen,
) -> App {
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
        .computer_with(ark_computer::ComputerService::with_screen(Arc::new(screen)))
        .build()
        .unwrap();
    App::new("Test").ark(ark)
}

struct SetupWorkers {
    title: &'static str,
    marker: std::path::PathBuf,
}

impl ark_runtime::worker::Workers for SetupWorkers {
    fn worker(&self, _: &str) -> Option<Result<Arc<dyn ark_runtime::worker::Worker>, String>> {
        None
    }

    fn offers(&self) -> Vec<ark_runtime::worker::WorkerOffer> {
        vec![ark_runtime::worker::WorkerOffer {
            spec: "agent:test".into(),
            title: self.title.into(),
            discoverable: true,
            state: ark_runtime::worker::WorkerState::Missing,
            models: Vec::new(),
            action: Some(ark_runtime::worker::WorkerAction {
                id: "test-install".into(),
                kind: "install".into(),
                label: "Install test".into(),
            }),
        }]
    }

    fn setup(
        &self,
        spec: &str,
        action: &str,
    ) -> Option<Result<ark_runtime::worker::WorkerSetup, String>> {
        (spec == "agent:test").then(|| {
            if action != "test-install" {
                return Err("stale setup action".into());
            }
            Ok(ark_runtime::worker::WorkerSetup {
                argv: vec![
                    "sh".into(),
                    "-c".into(),
                    format!("printf remote > '{}'; sleep 2", self.marker.display()),
                ],
                cwd: self.marker.parent().unwrap().to_path_buf(),
                clear_env: true,
                env: vec![("PATH".into(), std::env::var("PATH").unwrap())],
            })
        })
    }
}

fn app_with_workers(
    dir: &std::path::Path,
    who: &'static str,
    workers: Arc<dyn ark_runtime::worker::Workers>,
) -> App {
    let provider: Arc<dyn ark_model::Provider> = model(who);
    let ark = Ark::builder()
        .store_root(dir)
        .provider(provider)
        .planner(false)
        .critic(false)
        .workers_with(workers)
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

async fn resource_call(
    host: &Arc<Host>,
    kind: &str,
    operation: &str,
    id: &str,
    input: Value,
) -> anyhow::Result<Value> {
    let location = host.call("summary", json!({})).await?["location"].clone();
    host.call(
        "resource",
        json!({
            "ref": { "location": location, "kind": kind, "id": id },
            "operation": { "kind": operation, "input": input },
        }),
    )
    .await
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
async fn a_held_chat_cannot_read_or_change_this_computers_ambient_context() {
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (_b, port, token) = server(dir_b.path(), Arc::new(AtomicUsize::new(0))).await;
    let screen = ark_computer::fake::FakeScreen::new().app("Notes", &[]);
    screen.keep_front_on("Notes");
    let computer = app_with_computer(dir_a.path(), "A", Arc::new(AtomicUsize::new(0)), screen);
    let (a, _) = ArkUi::new(computer)
        .link("vps", &format!("http://127.0.0.1:{port}"), &token)
        .open()
        .await
        .unwrap();
    until("the link to answer", async || {
        a.links().state("vps") == Some(ark_interact::links::State::Ok)
    })
    .await;

    assert_eq!(a.remember_front_app().unwrap().as_deref(), Some("Notes"));
    assert_eq!(
        a.call("computer_context", json!({})).await.unwrap()["app"],
        "Notes"
    );
    a.call("new_session", json!({ "place": "vps" }))
        .await
        .unwrap();

    assert_eq!(
        a.remember_front_app().unwrap(),
        None,
        "a held chat never samples this screen"
    );
    assert_eq!(
        a.call("computer_context", json!({})).await.unwrap(),
        Value::Null
    );
    let error = a
        .call("capability_turn", json!({ "id": "computer", "on": false }))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("running elsewhere"), "{error}");
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
    let v = resource_call(b, "project", "workspace", "", json!({}))
        .await
        .unwrap();
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
    let local_location = a.call("summary", json!({})).await.unwrap()["location"].clone();

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
    let kept = a
        .call(
            "resource",
            json!({
                "ref": { "location": local_location, "kind": "artifact", "id": "" },
                "operation": { "kind": "artifacts", "input": {} },
            }),
        )
        .await
        .unwrap()
        .to_string();
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
    let before_chat = a
        .call("place_models", json!({ "place": "vps" }))
        .await
        .unwrap();
    assert!(
        before_chat
            .as_array()
            .is_some_and(|backends| backends.iter().any(|b| b["provider"] == "claude")),
        "the selected computer supplies the pre-chat catalog: {before_chat}"
    );
    let remote_chat = a
        .call("new_session", json!({ "place": "vps" }))
        .await
        .unwrap()["session"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    // B's own shell is never another runtime's to type into; a stream id is never a path.
    let own = resource_call(&b, "terminal", "terminal_open", "", json!({}))
        .await
        .unwrap()["session"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(b
        .call("chat_term_write", json!({ "id": own, "data": "ls\n" }))
        .await
        .is_err());
    resource_call(
        &b,
        "terminal",
        "terminal_close",
        &own,
        json!({ "session": own }),
    )
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
            json!({ "place": "vps", "program": "claude", "step": "sign_in" }),
        )
        .await
        .unwrap();
    let id = opened["session"].as_str().unwrap().to_string();
    let terminal_ref = opened["ref"].clone();
    let out = || {
        seen.lock()
            .unwrap()
            .iter()
            .filter(|v| v["kind"] == "term" && v["ref"] == terminal_ref)
            .map(|v| v["data"].as_str().unwrap_or("").to_string())
            .collect::<String>()
    };
    until("the link", async || out().contains("Paste code here")).await;
    assert!(out().contains("https://example.invalid/login"));
    // Changing the window's chat cannot rebind the setup terminal to that chat or runtime.
    a.call("new_session", json!({ "place": "here" }))
        .await
        .unwrap();
    a.call(
        "resource",
        json!({
            "ref": terminal_ref,
            "operation": { "kind": "terminal_write", "input": { "session": id, "data": "abc-123\n" } },
        }),
    )
    .await
    .unwrap();
    until("the answer", async || {
        out().contains("signed in with abc-123")
    })
    .await;
    a.call(
        "resource",
        json!({
            "ref": opened["ref"],
            "operation": { "kind": "terminal_close", "input": { "session": id } },
        }),
    )
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
    a.call("open_session", json!({ "id": remote_chat }))
        .await
        .unwrap();

    // Only the catalog's programs and steps; never another terminal there.
    for (program, step) in [("rm", "install"), ("claude", "shell")] {
        assert!(a
            .call(
                "place_setup",
                json!({ "place": "vps", "program": program, "step": step })
            )
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

/// Worker offers and their opaque setup actions come from the chat's runtime. A remote chat
/// never falls back to a similarly configured worker on this computer.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn worker_setup_runs_only_on_the_chats_runtime() {
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let local_marker = dir_a.path().join("local-worker");
    let remote_marker = dir_b.path().join("remote-worker");
    let tokens = Tokens::in_store(dir_b.path());
    let token = tokens.create("laptop").unwrap();
    let remote_workers = Arc::new(SetupWorkers {
        title: "Remote worker",
        marker: remote_marker.clone(),
    });
    let (b, served) = ArkUi::new(app_with_workers(dir_b.path(), "B", remote_workers))
        .access(Access::Tokens(tokens))
        .serve_background("127.0.0.1:0")
        .await
        .unwrap();
    let local_workers = Arc::new(SetupWorkers {
        title: "Local worker",
        marker: local_marker.clone(),
    });
    let (a, _) = ArkUi::new(app_with_workers(dir_a.path(), "A", local_workers))
        .link("vps", &format!("http://127.0.0.1:{}", served.port), &token)
        .serve_background("127.0.0.1:0")
        .await
        .unwrap();
    until("the worker link to answer", async || {
        a.links().state("vps") == Some(ark_interact::links::State::Ok)
    })
    .await;
    a.call("new_session", json!({ "place": "vps" }))
        .await
        .unwrap();

    let offers = a.call("worker_offers", json!({})).await.unwrap();
    assert_eq!(offers[0]["title"], "Remote worker", "{offers}");
    assert!(a
        .call(
            "worker_setup",
            json!({ "spec": "agent:test", "action": "wrong" })
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("stale"));
    let request = || {
        a.call(
            "worker_setup",
            json!({ "spec": "agent:test", "action": "test-install" }),
        )
    };
    let (first, second) = tokio::join!(request(), request());
    let first = first.unwrap();
    let second = second.unwrap();
    assert_eq!(first["session"], second["session"]);
    assert_ne!(first["reattached"], second["reattached"]);
    assert!(first["reattached"] == true || second["reattached"] == true);
    let opened = if first["reattached"] == true {
        second
    } else {
        first
    };
    until("remote worker setup", async || remote_marker.is_file()).await;
    assert_eq!(std::fs::read_to_string(&remote_marker).unwrap(), "remote");
    assert!(!local_marker.exists(), "the local setup must not run");
    resource_call(
        &a,
        "terminal",
        "terminal_close",
        opened["session"].as_str().unwrap(),
        json!({ "session": opened["session"] }),
    )
    .await
    .unwrap();
    drop(b);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn resources_keep_the_opening_chat_location_after_the_window_moves() {
    let (dir_a, dir_b, work) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    std::fs::write(work.path().join("where.txt"), "local\n").unwrap();
    let (b, port, token) = server(dir_b.path(), Arc::new(AtomicUsize::new(0))).await;
    let a = linked(dir_a.path(), port, &token, Arc::new(AtomicUsize::new(0))).await;
    a.call(
        "add_project",
        json!({ "name": "located", "path": work.path() }),
    )
    .await
    .unwrap();
    let local = a.call("summary", json!({})).await.unwrap()["location"].clone();

    a.call("new_session", json!({ "place": "vps" }))
        .await
        .unwrap();
    let remote_one = a.call("summary", json!({})).await.unwrap()["location"].clone();
    let remote_file = json!({
        "location": remote_one,
        "kind": "file",
        "id": "where.txt",
    });
    a.call(
        "resource",
        json!({
            "ref": remote_file,
            "operation": { "kind": "file_write", "input": { "text": "remote\n" } },
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(work.path().join("where.txt")).unwrap(),
        "local\n",
        "a remote file operation never fell back to the local replica"
    );
    let there = copy_folder(&b, "located").await;
    assert_eq!(
        std::fs::read_to_string(there.join("where.txt")).unwrap(),
        "remote\n"
    );

    a.call("new_session", json!({ "place": "here" }))
        .await
        .unwrap();
    assert_eq!(
        a.call("summary", json!({})).await.unwrap()["location"]["runtime"],
        local["runtime"]
    );
    let read = a
        .call(
            "resource",
            json!({
                "ref": remote_file,
                "operation": { "kind": "file_read", "input": {} },
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        read["text"], "remote\n",
        "an open tab keeps its remote location"
    );
    let local_file = json!({ "location": local, "kind": "file", "id": "where.txt" });
    a.call(
        "resource",
        json!({ "ref": local_file, "operation": { "kind": "file_read", "input": {} } }),
    )
    .await
    .unwrap();
    let mut file_events = a.subscribe();
    a.call(
        "resource",
        json!({ "ref": local_file, "operation": { "kind": "file_write", "input": { "text": "local-two\n" } } }),
    )
    .await
    .unwrap();
    a.call(
        "resource",
        json!({ "ref": remote_file, "operation": { "kind": "file_write", "input": { "text": "remote-two\n" } } }),
    )
    .await
    .unwrap();
    let mut changed_at = std::collections::HashSet::new();
    until("file changes to carry their exact location", async || {
        while let Ok(event) = file_events.try_recv() {
            if event["kind"] == "files_changed" && event["paths"] == json!(["where.txt"]) {
                changed_at.insert(event["ref"]["location"].clone());
            }
        }
        changed_at.len() == 2
    })
    .await;
    assert!(changed_at.contains(&local));
    assert!(changed_at.contains(&remote_one));

    let mut forged = remote_file.clone();
    forged["location"]["project"] = local["project"].clone();
    let err = a
        .call(
            "resource",
            json!({
                "ref": forged,
                "operation": { "kind": "file_write", "input": { "text": "wrong\n" } },
            }),
        )
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("does not match"));
    assert_eq!(
        std::fs::read_to_string(work.path().join("where.txt")).unwrap(),
        "local-two\n"
    );

    a.call("new_session", json!({ "place": "vps" }))
        .await
        .unwrap();
    let remote_two = a.call("summary", json!({})).await.unwrap()["location"].clone();
    let open = |location: Value| {
        json!({
            "ref": { "location": location, "kind": "terminal", "id": "" },
            "operation": { "kind": "terminal_open", "input": { "cols": 80, "rows": 24 } },
        })
    };
    let first = a.call("resource", open(remote_one.clone())).await.unwrap();
    let second = a.call("resource", open(remote_two.clone())).await.unwrap();
    assert_eq!(
        first["session"], second["session"],
        "one shell is shared by the replica"
    );
    assert_ne!(
        first["ref"]["location"]["session"],
        second["ref"]["location"]["session"]
    );

    let mut rx = a.subscribe();
    let terminal = first["session"].as_str().unwrap();
    a.call(
        "resource",
        json!({
            "ref": first["ref"],
            "operation": { "kind": "terminal_write", "input": { "data": "stty -echo\n" } },
        }),
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    a.call(
        "resource",
        json!({
            "ref": first["ref"],
            "operation": { "kind": "terminal_write", "input": { "data": "printf 'ARK_LOCATION_ONCE\\n'\n" } },
        }),
    )
    .await
    .unwrap();
    let mut output: std::collections::HashMap<String, String> = Default::default();
    until(
        "both chat aliases to receive terminal output once",
        async || {
            while let Ok(event) = rx.try_recv() {
                if event["kind"] == "term" && event["ref"]["id"] == terminal {
                    let session = event["ref"]["location"]["session"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    output
                        .entry(session)
                        .or_default()
                        .push_str(event["data"].as_str().unwrap_or(""));
                }
            }
            output.len() == 2
                && output
                    .values()
                    .all(|text| text.contains("ARK_LOCATION_ONCE"))
        },
    )
    .await;
    assert!(
        output
            .values()
            .all(|text| text.matches("ARK_LOCATION_ONCE").count() == 1),
        "{output:?}"
    );
    b.call(
        "chat_forget_resources",
        json!({ "session": remote_one["session"] }),
    )
    .await
    .unwrap();
    while rx.try_recv().is_ok() {}
    a.call(
        "resource",
        json!({
            "ref": second["ref"],
            "operation": { "kind": "terminal_write", "input": { "data": "printf 'ONLY_SECOND_ALIAS\n'\n" } },
        }),
    )
    .await
    .unwrap();
    let mut aliases = Vec::new();
    until("only the surviving alias to receive output", async || {
        while let Ok(event) = rx.try_recv() {
            if event["kind"] == "term"
                && event["data"]
                    .as_str()
                    .unwrap_or("")
                    .contains("ONLY_SECOND_ALIAS")
            {
                aliases.push(
                    event["ref"]["location"]["session"]
                        .as_str()
                        .unwrap_or("")
                        .to_string(),
                );
            }
        }
        !aliases.is_empty()
    })
    .await;
    assert_eq!(
        aliases,
        vec![remote_two["session"].as_str().unwrap().to_string()]
    );
    a.call(
        "resource",
        json!({
            "ref": first["ref"],
            "operation": { "kind": "terminal_close", "input": {} },
        }),
    )
    .await
    .unwrap();
    let reopened = a
        .call(
            "resource",
            json!({
                "ref": { "location": remote_two, "kind": "terminal", "id": "" },
                "operation": { "kind": "terminal_open", "input": {} },
            }),
        )
        .await
        .unwrap();
    assert_ne!(reopened["generation"], first["generation"]);
    a.call(
        "resource",
        json!({
            "ref": second["ref"],
            "operation": { "kind": "terminal_close", "input": {} },
        }),
    )
    .await
    .unwrap();
    let second_reopened = a
        .call(
            "resource",
            json!({
                "ref": {
                    "location": second["ref"]["location"],
                    "kind": "terminal", "id": "",
                },
                "operation": { "kind": "terminal_open", "input": {} },
            }),
        )
        .await
        .unwrap();
    assert_eq!(second_reopened["session"], reopened["session"]);
    assert_eq!(second_reopened["generation"], reopened["generation"]);
    a.call(
        "resource",
        json!({
            "ref": second_reopened["ref"],
            "operation": { "kind": "terminal_close", "input": {} },
        }),
    )
    .await
    .unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn project_terminal_opens_on_the_chosen_computer_without_a_chat() {
    let (dir_a, dir_b, work) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    std::fs::write(work.path().join("where.txt"), "remote project\n").unwrap();
    let (_b, port, token) = server(dir_b.path(), Arc::new(AtomicUsize::new(0))).await;
    let a = linked(dir_a.path(), port, &token, Arc::new(AtomicUsize::new(0))).await;
    let summary = a
        .call(
            "add_project",
            json!({ "name": "terminal-only", "path": work.path() }),
        )
        .await
        .unwrap();
    let project = summary["project"]["id"].as_str().unwrap();
    let session = summary["session"]["id"].as_str().unwrap().to_string();
    let missing = a
        .call(
            "project_terminal_open",
            json!({ "project": project, "place": "missing", "cols": 80, "rows": 24, "big": true }),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(missing.contains("not set up"), "{missing}");
    let local = a
        .call(
            "project_terminal_open",
            json!({ "project": project, "place": "", "here": "This computer", "cols": 80, "rows": 24 }),
        )
        .await
        .unwrap();
    assert_eq!(
        local["reattached"], false,
        "a refused remote choice must not have fallen back to a local shell"
    );
    a.call(
        "resource",
        json!({ "ref": local["ref"], "operation": { "kind": "terminal_close", "input": {} } }),
    )
    .await
    .unwrap();
    let opened = a
        .call(
            "project_terminal_open",
            json!({ "project": project, "place": "vps", "cols": 80, "rows": 24, "big": true }),
        )
        .await
        .unwrap();
    assert_eq!(opened["place"], "vps");
    assert_eq!(opened["ref"]["authority"]["scope"], "project");
    assert_eq!(opened["ref"]["location"]["session"], "");
    assert_eq!(
        a.call("summary", json!({})).await.unwrap()["session"]["id"],
        session
    );
    let local_runtime = a.call("hello", json!({})).await.unwrap()["runtime"].clone();
    assert_ne!(opened["ref"]["location"]["runtime"], local_runtime);

    let mut other_project = opened["ref"].clone();
    other_project["location"]["project"] = json!("another-project-copy");
    assert!(a
        .call(
            "resource",
            json!({
                "ref": other_project,
                "operation": { "kind": "terminal_read", "input": { "generation": opened["generation"], "after": 0 } },
            }),
        )
        .await
        .is_err());

    a.call(
        "resource",
        json!({
            "ref": opened["ref"],
            "operation": { "kind": "terminal_write", "input": { "data": "printf remote\\n", "sequence": 1 } },
        }),
    )
    .await
    .unwrap();
    let duplicate = a
        .call(
            "resource",
            json!({
                "ref": opened["ref"],
                "operation": { "kind": "terminal_write", "input": { "data": "printf duplicate\\n", "sequence": 1 } },
            }),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        duplicate.contains("duplicate or out of order"),
        "{duplicate}"
    );
    a.call(
        "resource",
        json!({ "ref": opened["ref"], "operation": { "kind": "terminal_close", "input": {} } }),
    )
    .await
    .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn a_partial_project_transfer_resumes_and_its_final_receipt_is_idempotent() {
    use base64::Engine as _;
    use sha2::Digest as _;

    let (store, source) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::write(source.path().join("chapter.txt"), "once\n").unwrap();
    let host = ArkUi::new(app(store.path(), "B", Arc::new(AtomicUsize::new(0))))
        .open()
        .await
        .unwrap()
        .0;
    let mut archive = Vec::new();
    ark_ui::copy::pack(
        source.path(),
        &["chapter.txt".into()],
        &[],
        false,
        &mut archive,
    )
    .unwrap();
    let stream = format!("{:x}", sha2::Sha256::digest(&archive));
    let cut = archive.len() / 2;
    let part = |offset: usize, end: usize, last: bool| {
        json!({
            "origin": "runtime-a",
            "origin_project": "project-a",
            "name": "resume",
            "stream": stream,
            "offset": offset,
            "data": base64::engine::general_purpose::STANDARD.encode(&archive[offset..end]),
            "last": last,
            "first": true,
            "epoch": 0,
            "manifest": ark_ui::copy::scan(source.path(), &["chapter.txt".into()]).unwrap(),
        })
    };
    let first = host.call("copy_put", part(0, cut, false)).await.unwrap();
    assert_eq!(first["next"], cut);
    let retry = host.call("copy_put", part(0, cut, false)).await.unwrap();
    assert_eq!(
        retry["next"], cut,
        "the same part is acknowledged, not appended"
    );
    let done = host
        .call("copy_put", part(cut, archive.len(), true))
        .await
        .unwrap();
    assert_eq!(done["done"], true);
    let receipt = host
        .call("copy_put", part(cut, archive.len(), true))
        .await
        .unwrap();
    assert_eq!(
        receipt, done,
        "a lost final response replays the stored receipt"
    );
    let folder = copy_folder(&host, "resume").await;
    assert_eq!(
        std::fs::read_to_string(folder.join("chapter.txt")).unwrap(),
        "once\n"
    );
    // The same content after an intervening transfer is a new lifecycle, not the permanent
    // receipt of the first send.
    for (epoch, text) in [(1, "between\n"), (2, "once\n")] {
        std::fs::write(source.path().join("chapter.txt"), text).unwrap();
        let mut next = Vec::new();
        ark_ui::copy::pack(
            source.path(),
            &["chapter.txt".into()],
            &[],
            false,
            &mut next,
        )
        .unwrap();
        let id = format!("{:x}", sha2::Sha256::digest(&next));
        host.call(
            "copy_put",
            json!({
                "origin": "runtime-a", "origin_project": "project-a", "name": "resume",
                "stream": id, "offset": 0,
                "data": base64::engine::general_purpose::STANDARD.encode(&next),
                "last": true, "first": false, "epoch": epoch,
                "manifest": ark_ui::copy::scan(source.path(), &["chapter.txt".into()]).unwrap(),
            }),
        )
        .await
        .unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(folder.join("chapter.txt")).unwrap(),
        "once\n"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_writers_to_one_replica_cannot_share_an_epoch() {
    use base64::Engine as _;
    use sha2::Digest as _;

    let (store, source) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let host = ArkUi::new(app(store.path(), "B", Arc::new(AtomicUsize::new(0))))
        .open()
        .await
        .unwrap()
        .0;
    let archive = |text: &str| {
        std::fs::write(source.path().join("same.txt"), text).unwrap();
        let mut bytes = Vec::new();
        ark_ui::copy::pack(source.path(), &["same.txt".into()], &[], false, &mut bytes).unwrap();
        let manifest = ark_ui::copy::scan(source.path(), &["same.txt".into()]).unwrap();
        (bytes, manifest)
    };
    let (left, left_manifest) = archive("left\n");
    let (right, right_manifest) = archive("right\n");
    let params = |bytes: &[u8], manifest: &ark_ui::copy::Manifest| {
        json!({
            "origin": "runtime-a", "origin_project": "project-a", "name": "race",
            "stream": format!("{:x}", sha2::Sha256::digest(bytes)), "offset": 0,
            "data": base64::engine::general_purpose::STANDARD.encode(bytes),
            "last": true, "first": false, "epoch": 0,
            "manifest": manifest,
        })
    };
    let (one, two) = tokio::join!(
        host.call("copy_put", params(&left, &left_manifest)),
        host.call("copy_put", params(&right, &right_manifest)),
    );
    assert_ne!(
        one.is_ok(),
        two.is_ok(),
        "only one writer may consume epoch 0"
    );
    let state = host
        .call(
            "copy_state",
            json!({
                "origin": "runtime-a", "origin_project": "project-a", "name": "race",
            }),
        )
        .await
        .unwrap();
    assert_eq!(state["epoch"], 1);
    let text = std::fs::read_to_string(copy_folder(&host, "race").await.join("same.txt")).unwrap();
    assert!(text == "left\n" || text == "right\n", "{text:?}");
    let folder = copy_folder(&host, "race").await;
    std::fs::write(folder.join("same.txt"), "written-after-apply\n").unwrap();
    let state = host
        .call(
            "copy_state",
            json!({
                "origin": "runtime-a", "origin_project": "project-a", "name": "race",
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        state["changes"]["changed"],
        json!(["same.txt"]),
        "the destination records the transferred manifest, not a later live scan"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_bring_plan_is_stable_and_settlement_does_not_hide_a_later_change() {
    use base64::Engine as _;
    use sha2::Digest as _;

    let (store, source) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::write(source.path().join("chapter.txt"), "base\n").unwrap();
    let host = ArkUi::new(app(store.path(), "B", Arc::new(AtomicUsize::new(0))))
        .open()
        .await
        .unwrap()
        .0;
    let mut archive = Vec::new();
    ark_ui::copy::pack(
        source.path(),
        &["chapter.txt".into()],
        &[],
        false,
        &mut archive,
    )
    .unwrap();
    let stream = format!("{:x}", sha2::Sha256::digest(&archive));
    host.call(
        "copy_put",
        json!({
            "origin": "runtime-a", "origin_project": "project-a", "name": "stable",
            "stream": stream, "offset": 0,
            "data": base64::engine::general_purpose::STANDARD.encode(&archive),
            "last": true, "first": true,
            "epoch": 0,
            "manifest": ark_ui::copy::scan(source.path(), &["chapter.txt".into()]).unwrap(),
        }),
    )
    .await
    .unwrap();
    let folder = copy_folder(&host, "stable").await;
    std::fs::write(folder.join("chapter.txt"), "packed\n").unwrap();
    let plan = json!({
        "origin": "runtime-a", "origin_project": "project-a", "name": "stable",
        "files": ["chapter.txt"], "deleted": [],
    });
    let first = host.call("copy_pack", plan.clone()).await.unwrap();
    let retry = host.call("copy_pack", plan.clone()).await.unwrap();
    assert_eq!(
        first["stream"], retry["stream"],
        "retry reuses the same content plan"
    );
    let archive = store
        .path()
        .join("uploads")
        .join(format!("copy-{}.tar", first["stream"].as_str().unwrap()));
    std::fs::write(&archive, b"cut short").unwrap();
    let repaired = host.call("copy_pack", plan.clone()).await.unwrap();
    assert_eq!(repaired["stream"], first["stream"]);
    assert_eq!(
        std::fs::metadata(&archive).unwrap().len(),
        repaired["size"].as_u64().unwrap(),
        "a truncated final archive is packed atomically again"
    );

    // A change after packing must still be reported after settlement. Settlement records the
    // packed version, not whatever happens to be in the folder when its acknowledgement arrives.
    std::fs::write(folder.join("chapter.txt"), "later\n").unwrap();
    let settle = json!({
        "origin": "runtime-a", "origin_project": "project-a", "name": "stable",
        "stream": first["stream"], "paths": ["chapter.txt"],
    });
    assert_eq!(
        host.call("copy_settle", settle.clone()).await.unwrap()["ok"],
        true
    );
    assert_eq!(host.call("copy_settle", settle).await.unwrap()["ok"], true);
    let state = host.call("copy_state", plan).await.unwrap();
    assert_eq!(state["changes"]["changed"], json!(["chapter.txt"]));

    let plan = json!({
        "origin": "runtime-a", "origin_project": "project-a", "name": "stable",
        "files": ["chapter.txt"], "deleted": [],
    });
    let middle = host.call("copy_pack", plan.clone()).await.unwrap();
    host.call(
        "copy_settle",
        json!({
            "origin": "runtime-a", "origin_project": "project-a", "name": "stable",
            "stream": middle["stream"], "paths": ["chapter.txt"],
        }),
    )
    .await
    .unwrap();
    std::fs::write(folder.join("chapter.txt"), "packed\n").unwrap();
    let again = host.call("copy_pack", plan.clone()).await.unwrap();
    assert_ne!(
        again["stream"], first["stream"],
        "an identical plan after an intervening settlement has a new lifecycle"
    );
    host.call(
        "copy_settle",
        json!({
            "origin": "runtime-a", "origin_project": "project-a", "name": "stable",
            "stream": again["stream"], "paths": ["chapter.txt"],
        }),
    )
    .await
    .unwrap();
    let final_state = host.call("copy_state", plan).await.unwrap();
    assert_eq!(final_state["changes"]["changed"], json!([]));
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_bring_resumes_after_apply_before_its_local_manifest_advanced() {
    let (dir_a, dir_b, work) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    std::fs::write(work.path().join("chapter.txt"), "base\n").unwrap();
    let (b, port, token) = server(dir_b.path(), Arc::new(AtomicUsize::new(0))).await;
    let a = linked(dir_a.path(), port, &token, Arc::new(AtomicUsize::new(0))).await;
    a.call(
        "add_project",
        json!({ "name": "resume-bring", "path": work.path() }),
    )
    .await
    .unwrap();
    let local = a.call("summary", json!({})).await.unwrap()["location"].clone();
    a.call("new_session", json!({ "place": "vps" }))
        .await
        .unwrap();
    let remote = a.call("summary", json!({})).await.unwrap()["location"].clone();
    let folder = copy_folder(&b, "resume-bring").await;
    std::fs::write(folder.join("chapter.txt"), "remote\n").unwrap();
    let packed = b
        .call(
            "copy_pack",
            json!({
                "origin": local["runtime"], "origin_project": local["project"],
                "name": "resume-bring", "files": ["chapter.txt"], "deleted": [],
            }),
        )
        .await
        .unwrap();
    let stream = packed["stream"].as_str().unwrap();

    // This is the durable cut point: bytes are already applied and the receipt is stored, but
    // the process stopped before advancing this side's manifest or settling the sender.
    std::fs::write(work.path().join("chapter.txt"), "remote\n").unwrap();
    let key = format!(
        "{}:{}",
        local["project"].as_str().unwrap(),
        remote["runtime"].as_str().unwrap()
    );
    let store = ark_store::Store::open(dir_a.path()).unwrap();
    store
        .put_record(
            "copy_bring_pending",
            &key,
            &key,
            &json!({
                "stream": stream,
                "epoch": packed["epoch"],
                "manifest": packed["manifest"],
                "deleted": packed["deleted"],
            }),
        )
        .unwrap();
    store
        .put_record(
            "copy_receipt",
            &format!("{key}:bring:{stream}"),
            &key,
            &json!({ "written": ["chapter.txt"], "deleted": [], "refused": [] }),
        )
        .unwrap();

    let resumed = a
        .call("place_bring", json!({ "choices": {} }))
        .await
        .unwrap();
    assert_eq!(resumed["written"], json!(["chapter.txt"]));
    let remote_state = b
        .call(
            "copy_state",
            json!({
                "origin": local["runtime"], "origin_project": local["project"],
                "name": "resume-bring",
            }),
        )
        .await
        .unwrap();
    assert_eq!(remote_state["changes"]["changed"], json!([]));

    // If this side changed again after the bytes were applied, resuming must not bless that
    // different content as the remote version. The interrupted plan is abandoned and the next
    // Bring reports an ordinary two-sided conflict.
    std::fs::write(folder.join("chapter.txt"), "remote-again\n").unwrap();
    let packed = b
        .call(
            "copy_pack",
            json!({
                "origin": local["runtime"], "origin_project": local["project"],
                "name": "resume-bring", "files": ["chapter.txt"], "deleted": [],
            }),
        )
        .await
        .unwrap();
    let stream = packed["stream"].as_str().unwrap();
    std::fs::write(work.path().join("chapter.txt"), "remote-again\n").unwrap();
    store
        .put_record(
            "copy_bring_pending",
            &key,
            &key,
            &json!({
                "stream": stream, "epoch": packed["epoch"],
                "manifest": packed["manifest"], "deleted": packed["deleted"],
            }),
        )
        .unwrap();
    store
        .put_record(
            "copy_receipt",
            &format!("{key}:bring:{stream}"),
            &key,
            &json!({ "written": ["chapter.txt"], "deleted": [], "refused": [] }),
        )
        .unwrap();
    std::fs::write(work.path().join("chapter.txt"), "local-later\n").unwrap();
    let error = a
        .call("place_bring", json!({ "choices": {} }))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("changed here after"), "{error}");
    let conflict = a
        .call("place_bring", json!({ "choices": {} }))
        .await
        .unwrap();
    assert_eq!(conflict["waiting"], json!(["chapter.txt"]), "{conflict}");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn an_offline_settlement_keeps_the_bring_plan_and_receipt() {
    let (dir_a, dir_b, work) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    std::fs::write(work.path().join("chapter.txt"), "base\n").unwrap();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let remote_store = dir_b.path().to_path_buf();
    let server_thread = std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async move {
            let (host, port, token) = server(&remote_store, Arc::new(AtomicUsize::new(0))).await;
            ready_tx.send((host, port, token)).unwrap();
            stopped.await.ok();
        });
        runtime.shutdown_background();
    });
    let (b, port, token) = ready_rx.recv().unwrap();
    let a = linked(dir_a.path(), port, &token, Arc::new(AtomicUsize::new(0))).await;
    a.call(
        "add_project",
        json!({ "name": "offline-settle", "path": work.path() }),
    )
    .await
    .unwrap();
    let local = a.call("summary", json!({})).await.unwrap()["location"].clone();
    a.call("new_session", json!({ "place": "vps" }))
        .await
        .unwrap();
    let remote = a.call("summary", json!({})).await.unwrap()["location"].clone();
    let folder = copy_folder(&b, "offline-settle").await;
    std::fs::write(folder.join("chapter.txt"), "remote\n").unwrap();
    let packed = b
        .call(
            "copy_pack",
            json!({
                "origin": local["runtime"], "origin_project": local["project"],
                "name": "offline-settle", "files": ["chapter.txt"], "deleted": [],
            }),
        )
        .await
        .unwrap();
    let stream = packed["stream"].as_str().unwrap();
    std::fs::write(work.path().join("chapter.txt"), "remote\n").unwrap();
    let key = format!(
        "{}:{}",
        local["project"].as_str().unwrap(),
        remote["runtime"].as_str().unwrap()
    );
    let receipt = format!("{key}:bring:{stream}");
    let store = ark_store::Store::open(dir_a.path()).unwrap();
    store
        .put_record(
            "copy_bring_pending",
            &key,
            &key,
            &json!({
                "stream": stream, "epoch": packed["epoch"],
                "manifest": packed["manifest"], "deleted": packed["deleted"],
            }),
        )
        .unwrap();
    store
        .put_record(
            "copy_receipt",
            &receipt,
            &key,
            &json!({ "written": ["chapter.txt"], "deleted": [], "refused": [] }),
        )
        .unwrap();

    stop.send(()).unwrap();
    server_thread.join().unwrap();
    until("the settlement runtime to be offline", async || {
        a.links().state("vps") != Some(ark_interact::links::State::Ok)
    })
    .await;
    assert!(a
        .call("place_bring", json!({ "choices": {} }))
        .await
        .is_err());
    assert!(store
        .get_record("copy_bring_pending", &key)
        .unwrap()
        .is_some());
    assert!(store
        .get_record("copy_receipt", &receipt)
        .unwrap()
        .is_some());
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
