//! What waits across runtimes (ADR 0043 step 4): this computer (A) shows what waits on the
//! person's own server (B) beside its own, answered there; B's own list and notifications stay
//! B's; A never hands B's token to anyone.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ark_interact::ark_orchestrate::Orchestrator;
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
        vec![ActionDescriptor::new(
            "notes.save",
            "save the notes",
            RiskTier::Mutate,
        )]
    }
    async fn run(&self, _: &str, _: Value, _: &RunCtx<'_>) -> ark_tools::Result<ActionOutput> {
        self.saves.fetch_add(1, Ordering::SeqCst);
        Ok(ActionOutput::json(json!({ "ok": true })))
    }
}

/// An agent asked to file the notes calls `notes.save`, then says "Saved.".
fn model() -> Arc<MockProvider> {
    Arc::new(MockProvider::new("m", vec![]).with_fallback(|req| {
        let saving = req
            .messages
            .iter()
            .any(|m| m.content.contains("file the notes"));
        let called = req.messages.iter().any(|m| m.tool_call_id.is_some());
        let (content, tool_calls) = match (saving, called) {
            (true, false) => (
                String::new(),
                vec![ToolCall {
                    id: "c".into(),
                    name: "notes.save".into(),
                    arguments: json!({}),
                }],
            ),
            (true, true) => ("Saved.".to_string(), vec![]),
            (false, _) => ("Hello.".to_string(), vec![]),
        };
        Completion {
            content,
            tool_calls,
            usage: Usage::default(),
        }
    }))
}

fn app(dir: &std::path::Path, saves: Arc<AtomicUsize>) -> App {
    let provider: Arc<dyn ark_model::Provider> = model();
    let ark = Ark::builder()
        .store_root(dir)
        .tool(Arc::new(Notes { saves }))
        .profile(AgentProfile::builder().core_actions(["notes.save"]).build())
        .provider(provider)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    App::new("Test").ark(ark).orchestrate(|ark, project, gate| {
        Ok(Orchestrator::builder(ark, project)
            .approvals(gate)
            .build()?)
    })
}

async fn until(what: &str, mut f: impl AsyncFnMut() -> bool) {
    for _ in 0..200 {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("never: {what}");
}

async fn all(host: &Arc<Host>) -> Value {
    host.call("attention_all", json!({})).await.unwrap()
}

async fn project_call(host: &Arc<Host>, operation: &str, input: Value) -> anyhow::Result<Value> {
    let location = host.call("summary", json!({})).await?["location"].clone();
    host.call(
        "resource",
        json!({
            "ref": { "location": location, "kind": "project", "id": "" },
            "operation": { "kind": operation, "input": input },
        }),
    )
    .await
}

/// B (the person's server) with an agent's question waiting.
async fn server_with_a_question(
    dir: &std::path::Path,
    saves: Arc<AtomicUsize>,
) -> (Arc<Host>, u16, String) {
    let tokens = Tokens::in_store(dir);
    let token = tokens.create("laptop").unwrap();
    let (b, served) = ArkUi::new(app(dir, saves))
        .access(Access::Tokens(tokens))
        .serve_background("127.0.0.1:0")
        .await
        .unwrap();
    b.call("switch_project", json!({ "name": "novel" }))
        .await
        .unwrap();
    project_call(
        &b,
        "spawn_agent",
        json!({ "spec": { "name": "scribe", "tools": ["notes.*"] } }),
    )
    .await
    .unwrap();
    project_call(
        &b,
        "submit_task",
        json!({ "agent": "scribe", "task": { "objective": "file the notes" } }),
    )
    .await
    .unwrap();
    until("B's question", async || {
        b.call("attention", json!({}))
            .await
            .unwrap()
            .as_array()
            .is_some_and(|a| !a.is_empty())
    })
    .await;
    (b, served.port, token)
}

#[tokio::test]
async fn a_question_on_my_machine_is_answered_from_this_computer() {
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let saves = Arc::new(AtomicUsize::new(0));
    let (b, port, token) = server_with_a_question(dir_b.path(), saves.clone()).await;
    let url = format!("http://127.0.0.1:{port}");
    let (a, _) = ArkUi::new(app(dir_a.path(), Arc::new(AtomicUsize::new(0))))
        .link("My machine", &url, &token)
        .open()
        .await
        .unwrap();

    // A shows B's question, named; A's own list and B's stay their own.
    until("B's question on A", async || {
        all(&a).await["items"]
            .as_array()
            .is_some_and(|i| i.len() == 1)
    })
    .await;
    let v = all(&a).await;
    let q = &v["items"][0];
    assert_eq!(q["runtime"], "My machine");
    assert_eq!(q["kind"], "question");
    assert!(
        q["place"]
            .as_str()
            .unwrap()
            .starts_with("My machine · novel"),
        "{q}"
    );
    assert_eq!(
        a.call("attention", json!({})).await.unwrap(),
        json!([]),
        "A's own list"
    );
    let on_b = b.call("attention", json!({})).await.unwrap();
    assert!(on_b[0].get("runtime").is_none(), "B's list is B's alone");

    // B's token is never in what A answers.
    for m in ["attention_all", "hello", "summary"] {
        let out = a.call(m, json!({})).await.unwrap().to_string();
        assert!(!out.contains(&token), "{m} carries B's token");
    }
    let e = a
        .call(
            "attention_open",
            json!({ "runtime": "My machine", "id": "q-none" }),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(!e.contains(&token) && !e.contains(&url), "{e}");
    // Only these calls reach B: an unknown runtime is refused.
    assert!(a
        .call(
            "attention_answer",
            json!({ "runtime": "Elsewhere", "id": "x", "decision": "allow_once" })
        )
        .await
        .is_err());

    // What A tells its windows because of B is marked, so a runtime following A ignores it.
    let mut events = a.subscribe();
    // The terminal of A lists it and answers it there.
    let listed = a
        .call("command", json!({ "line": "/waiting" }))
        .await
        .unwrap()
        .to_string();
    assert!(listed.contains("My machine · novel"), "{listed}");
    let answered = a
        .call("command", json!({ "line": "/waiting 1 once" }))
        .await
        .unwrap()
        .to_string();
    assert!(answered.contains("Answered"), "{answered}");
    until("B's task to continue", async || {
        saves.load(Ordering::SeqCst) == 1
    })
    .await;
    until("A's list to follow", async || {
        all(&a).await["items"]
            .as_array()
            .is_some_and(|i| i.is_empty())
    })
    .await;
    let mut seen = Vec::new();
    while let Ok(v) = events.try_recv() {
        if v["kind"] == "attention" {
            seen.push(v);
        }
    }
    assert!(
        !seen.is_empty() && seen.iter().all(|v| v["linked"] == true),
        "{seen:?}"
    );
}

/// A link that does not answer adds nothing and does not hold A up; one that refuses A's token
/// says to pair again.
#[tokio::test]
async fn a_link_that_is_down_or_refuses_does_not_hold_this_computer_up() {
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let saves = Arc::new(AtomicUsize::new(0));
    let (_b, port, _token) = server_with_a_question(dir_b.path(), saves).await;
    let (a, _) = ArkUi::new(app(dir_a.path(), Arc::new(AtomicUsize::new(0))))
        .link(
            "My machine",
            &format!("http://127.0.0.1:{port}"),
            "ark_wrong",
        )
        .link("Old server", "http://127.0.0.1:9", "ark_x")
        .open()
        .await
        .unwrap();
    let started = std::time::Instant::now();
    let v = all(&a).await;
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "reads never wait on a link"
    );
    assert_eq!(v["items"], json!([]));
    until("the refusal to show", async || {
        all(&a).await["notes"]
            .to_string()
            .contains("My machine signed this computer out")
    })
    .await;
    let notes = all(&a).await["notes"].to_string();
    assert!(
        !notes.contains("Old server"),
        "a link that never had items stays quiet: {notes}"
    );
}

/// A linked runtime that had a question and stops answering: the question stays, as last read,
/// and the window marks it rather than adding a line; the terminal says so in a line.
#[tokio::test]
async fn a_question_stays_while_its_computer_does_not_answer() {
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let saves = Arc::new(AtomicUsize::new(0));
    // B on a runtime of its own, so that it can go away: shutting that runtime down ends the
    // server and its streams, as a computer that sleeps.
    let (tx, rx) = std::sync::mpsc::channel();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let dir = dir_b.path().to_path_buf();
    let b = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let (_b, port, token) = server_with_a_question(&dir, saves).await;
            tx.send((port, token)).unwrap();
            stopped.await.ok();
        });
        rt.shutdown_background();
    });
    let (port, token) = rx.recv().unwrap();
    let (a, _) = ArkUi::new(app(dir_a.path(), Arc::new(AtomicUsize::new(0))))
        .link("My machine", &format!("http://127.0.0.1:{port}"), &token)
        .open()
        .await
        .unwrap();
    until("B's question on A", async || {
        all(&a).await["items"]
            .as_array()
            .is_some_and(|i| i.len() == 1)
    })
    .await;
    assert_eq!(all(&a).await["away"], json!([]));
    stop.send(()).unwrap();
    b.join().unwrap();
    until("B to count as away", async || {
        all(&a).await["away"] == json!(["My machine"])
    })
    .await;
    let v = all(&a).await;
    assert_eq!(v["items"].as_array().unwrap().len(), 1, "kept as last read");
    assert_eq!(v["notes"], json!([]), "the window marks the item instead");
    let listed = a
        .call("command", json!({ "line": "/waiting" }))
        .await
        .unwrap()
        .to_string();
    assert!(listed.contains("My machine is not answering"), "{listed}");
}

/// The window (WebKit): B's question as a card in A's tray, answered from there; B's page opened
/// with a token and an item keeps the item. `ARK_UI_PAGE_TESTS=1`, as `tests/page.rs`.
#[tokio::test(flavor = "multi_thread")]
async fn the_window_answers_a_question_on_my_machine() {
    if std::env::var_os("ARK_UI_PAGE_TESTS").is_none() {
        eprintln!("skipped: set ARK_UI_PAGE_TESTS=1 to run the page tests");
        return;
    }
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let saves = Arc::new(AtomicUsize::new(0));
    let (b, port, token) = server_with_a_question(dir_b.path(), saves.clone()).await;
    let url = format!("http://127.0.0.1:{port}");
    let (_a, served) = ArkUi::new(app(dir_a.path(), Arc::new(AtomicUsize::new(0))))
        .link("My machine", &url, &token)
        .serve_background("127.0.0.1:0")
        .await
        .unwrap();
    let item = b.call("attention", json!({})).await.unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let out = tokio::process::Command::new("node")
        .arg("page.mjs")
        .arg("linked")
        .arg(&served.url)
        .env(
            "ARK_TEST_ELSEWHERE",
            format!("{url}/?token={token}#item={item}"),
        )
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/web"))
        .output()
        .await
        .expect("node is on PATH");
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "linked failed:\n{text}");
    until("B's task to continue", async || {
        saves.load(Ordering::SeqCst) == 1
    })
    .await;
}

/// Two runtimes that link each other settle: what one emits because of its link does not make
/// the other read it again, and a read that finds nothing new tells nobody.
#[tokio::test]
async fn two_runtimes_linked_both_ways_settle() {
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let saves = Arc::new(AtomicUsize::new(0));
    let (b, port_b, token_b) = server_with_a_question(dir_b.path(), saves).await;
    let tokens_a = Tokens::in_store(dir_a.path());
    let token_a = tokens_a.create("server").unwrap();
    let (a, served_a) = ArkUi::new(app(dir_a.path(), Arc::new(AtomicUsize::new(0))))
        .access(Access::Tokens(tokens_a))
        .link("Server", &format!("http://127.0.0.1:{port_b}"), &token_b)
        .serve_background("127.0.0.1:0")
        .await
        .unwrap();
    b.links().set(vec![ark_interact::links::Link::new(
        "Laptop",
        format!("http://127.0.0.1:{}", served_a.port),
        token_a,
    )]);
    until("B's question on A", async || {
        all(&a).await["items"]
            .as_array()
            .is_some_and(|i| i.len() == 1)
    })
    .await;
    // Let both followers connect, then count what each tells its windows.
    tokio::time::sleep(Duration::from_millis(800)).await;
    let (mut ra, mut rb) = (a.subscribe(), b.subscribe());
    let count = |rx: &mut tokio::sync::broadcast::Receiver<Value>| {
        let mut n = 0;
        while let Ok(v) = rx.try_recv() {
            n += usize::from(v["kind"] == "attention");
        }
        n
    };
    tokio::time::sleep(Duration::from_secs(2)).await;
    let (na, nb) = (count(&mut ra), count(&mut rb));
    assert!(na <= 1 && nb <= 1, "they read each other: A {na}, B {nb}");
    let on_b = all(&b).await["items"].clone();
    assert_eq!(on_b.as_array().unwrap().len(), 1, "B's own, once: {on_b}");
    assert!(
        on_b[0].get("runtime").is_none(),
        "A has nothing to add: {on_b}"
    );
}
