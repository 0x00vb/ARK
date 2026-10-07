//! What waits for the person (ADR 0043 step 1): one list in every project, the same in the
//! window (`attention`) and the terminal (`/waiting`). Questions come from live agents; failures
//! and results are records written as the orchestrator writes them.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ark_interact::ark_orchestrate::Orchestrator;
use ark_interact::App;
use ark_sdk::prelude::ark_model::{Completion, MockProvider, ToolCall, Usage};
use ark_sdk::prelude::*;
use ark_ui::{ArkUi, Host};
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

/// An agent asked to file the notes calls `notes.save`, then says "Saved."; a chat says "Hello.".
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

async fn open(dir: &std::path::Path, saves: Arc<AtomicUsize>) -> Arc<Host> {
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
    let app = App::new("Test").ark(ark).orchestrate(|ark, project, gate| {
        Ok(Orchestrator::builder(ark, project)
            .approvals(gate)
            .build()?)
    });
    ArkUi::new(app).open().await.unwrap().0
}

async fn wait_for(
    rx: &mut tokio::sync::broadcast::Receiver<Value>,
    what: &str,
    f: impl Fn(&Value) -> bool,
) -> Value {
    loop {
        let v = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .unwrap_or_else(|_| panic!("no {what}"))
            .unwrap();
        if std::env::var("SHOW").is_ok() {
            eprintln!("{v}");
        }
        if f(&v) {
            return v;
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

const DAY: u64 = 24 * 3600 * 1000;

/// A finished task's record, as the orchestrator stores it.
fn task(id: &str, project: &str, state: &str, finished_ms: u64, extra: Value) -> Value {
    let mut t = json!({
        "id": id, "project": project, "owner": "helper", "objective": format!("work {id}"),
        "origin": { "by": "external" }, "state": state, "created_ms": finished_ms - 1000,
        "started_ms": finished_ms - 900, "finished_ms": finished_ms,
        "error": format!("{id} broke"), "result": format!("{id} result"),
    });
    for (k, v) in extra.as_object().unwrap() {
        t[k] = v.clone();
    }
    t
}

fn put(dir: &std::path::Path, t: &Value) {
    let ark = Ark::builder().store_root(dir).build().unwrap();
    let id = t["id"].as_str().unwrap();
    ark.engine()
        .store
        .put_record("orch_task", id, t["project"].as_str().unwrap(), t)
        .unwrap();
}

fn post(dir: &std::path::Path, chat: &str, work: &str) {
    let ark = Ark::builder().store_root(dir).build().unwrap();
    let p = ark_runtime::Post::new("helper", format!("work {work}"), "text", true).work(work);
    ark_runtime::session::post(ark.engine(), chat, p).unwrap();
}

async fn list(host: &Arc<Host>) -> Vec<Value> {
    host.call("attention", json!({}))
        .await
        .unwrap()
        .as_array()
        .unwrap()
        .clone()
}

fn ids(items: &[Value]) -> Vec<String> {
    items
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect()
}

async fn command(host: &Arc<Host>, line: &str) -> String {
    host.call("command", json!({ "line": line }))
        .await
        .unwrap()
        .to_string()
}

/// A chat in project `name` with one turn; returns (project id, chat id).
async fn chat_in(host: &Arc<Host>, name: &str) -> (String, String) {
    host.call("switch_project", json!({ "name": name }))
        .await
        .unwrap();
    let mut rx = host.subscribe();
    host.call("send", json!({ "text": "hi" })).await.unwrap();
    wait_for(&mut rx, "the turn's end", |v| v["kind"] == "turn_finished").await;
    let s = host.call("summary", json!({})).await.unwrap();
    (
        s["project"]["id"].as_str().unwrap().to_string(),
        s["session"]["id"].as_str().unwrap().to_string(),
    )
}

/// What counts: a question in one project; in another, a failure and a result posted while
/// nobody was in the chat. What does not: a result a waiting turn took, a delegated task's
/// failure, a cancelled task, a failure older than a week, a schedule's failure its next run
/// fixed. The window and the terminal list the same items.
#[tokio::test]
async fn one_list_across_projects_in_both_front_ends() {
    let dir = tempfile::tempdir().unwrap();
    let saves = Arc::new(AtomicUsize::new(0));
    let host = open(dir.path(), saves.clone()).await;
    let (b, chat) = chat_in(&host, "books").await;
    let now = now_ms();
    for t in [
        task("fail1", &b, "failed", now - 60_000, json!({})),
        task(
            "done1",
            &b,
            "done",
            now - 30_000,
            json!({ "report_to": chat }),
        ),
        // Taken by the chat turn that waited for it: no post.
        task(
            "took1",
            &b,
            "done",
            now - 20_000,
            json!({ "report_to": chat }),
        ),
        task(
            "sub1",
            &b,
            "failed",
            now - 10_000,
            json!({ "origin": { "by": "agent", "agent": "lead", "task": "x" }, "parent": "x" }),
        ),
        task("cancel1", &b, "cancelled", now - 5_000, json!({})),
        task("old1", &b, "failed", now - 8 * DAY, json!({})),
        task(
            "sched1",
            &b,
            "failed",
            now - 9_000,
            json!({ "schedule": "s1" }),
        ),
        task(
            "sched2",
            &b,
            "done",
            now - 8_000,
            json!({ "schedule": "s1" }),
        ),
    ] {
        put(dir.path(), &t);
    }
    post(dir.path(), &chat, "done1");

    // A question in the first project.
    let (a, _) = chat_in(&host, "test").await;
    let mut rx = host.subscribe();
    host.call(
        "spawn_agent",
        json!({ "spec": { "name": "scribe", "tools": ["notes.*"] } }),
    )
    .await
    .unwrap();
    host.call(
        "submit_task",
        json!({ "agent": "scribe", "task": { "objective": "file the notes" } }),
    )
    .await
    .unwrap();
    wait_for(&mut rx, "the question", |v| v["kind"] == "attention").await;

    let items = list(&host).await;
    assert_eq!(items.len(), 3, "{items:#?}");
    assert_eq!(items[0]["kind"], "question");
    assert_eq!(items[0]["project"], a.as_str());
    assert_eq!(
        ids(&items[1..]),
        vec![
            format!("finished/done1/{}", now - 30_000),
            format!("failed/fail1/{}", now - 60_000)
        ]
    );
    assert_eq!(items[1]["place"], "books · hi");
    assert_eq!(items[2]["place"], "books · helper");

    let waiting = command(&host, "/waiting").await;
    for it in &items {
        assert!(waiting.contains(it["line"].as_str().unwrap()), "{waiting}");
        assert!(waiting.contains(it["place"].as_str().unwrap()), "{waiting}");
    }
    assert_eq!(saves.load(Ordering::SeqCst), 0);
}

/// A question in a project that is not current, answered from the terminal by its number. The
/// window hears about it without reading anything: the card goes, the list changes.
#[tokio::test]
async fn the_terminal_answers_a_question_in_another_project() {
    let dir = tempfile::tempdir().unwrap();
    let saves = Arc::new(AtomicUsize::new(0));
    let host = open(dir.path(), saves.clone()).await;
    chat_in(&host, "books").await;
    let mut rx = host.subscribe();
    host.call(
        "spawn_agent",
        json!({ "spec": { "name": "scribe", "tools": ["notes.*"] } }),
    )
    .await
    .unwrap();
    host.call(
        "submit_task",
        json!({ "agent": "scribe", "task": { "objective": "file the notes" } }),
    )
    .await
    .unwrap();
    let asked = wait_for(&mut rx, "the question", |v| v["kind"] == "approval").await;
    // It says where it comes from, for a window in another project ("Always in this project").
    assert_eq!(asked["project"]["name"], "books", "{asked}");
    chat_in(&host, "test").await;
    let hello = host.call("hello", json!({})).await.unwrap();
    assert_eq!(hello["approvals"][0]["project"]["name"], "books", "{hello}");
    let question = list(&host).await[0]["id"].as_str().unwrap().to_string();

    // A number answers only the list the person saw.
    assert!(command(&host, "/waiting 1 once")
        .await
        .contains("/waiting shows it"));
    command(&host, "/waiting").await;
    let opened = command(&host, "/waiting 1").await;
    assert!(opened.contains("/waiting 1 once"), "{opened}");
    let mut rx = host.subscribe();
    let answered = command(&host, "/waiting 1 once").await;
    assert!(answered.contains("Answered"), "{answered}");
    wait_for(&mut rx, "the card to go", |v| {
        v["kind"] == "approval_closed" && v["id"] == question.as_str()
    })
    .await;
    wait_for(&mut rx, "the list to change", |v| v["kind"] == "attention").await;
    for _ in 0..100 {
        if saves.load(Ordering::SeqCst) == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(saves.load(Ordering::SeqCst), 1, "the call ran once");
    assert!(list(&host).await.is_empty());
}

/// Clearing is kept across a restart; a retry that fails again is a new item. Numbers answer
/// the list the person saw, also when newer work arrives. Opening the chat a result was posted
/// into clears it.
#[test]
fn cleared_stays_cleared_and_opening_a_chat_clears_what_was_posted_there() {
    let dir = tempfile::tempdir().unwrap();
    let saves = Arc::new(AtomicUsize::new(0));
    let now = now_ms();
    let (b, chat) = {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let path = dir.path().to_path_buf();
        let s = saves.clone();
        rt.block_on(async move {
            let host = open(&path, s).await;
            let (b, chat) = chat_in(&host, "books").await;
            put(&path, &task("fail1", &b, "failed", now - 60_000, json!({})));
            command(&host, "/waiting").await;
            // Newer work would be number 1 now; 1 is still what the person saw.
            put(&path, &task("fail2", &b, "failed", now - 1_000, json!({})));
            let opened = command(&host, "/waiting 1").await;
            assert!(opened.contains("fail1 broke"), "{opened}");
            assert_eq!(
                ids(&list(&host).await),
                vec![format!("failed/fail2/{}", now - 1_000)]
            );
            host.call(
                "attention_clear",
                json!({ "id": format!("failed/fail2/{}", now - 1_000) }),
            )
            .await
            .unwrap();
            assert!(list(&host).await.is_empty());
            (b, chat)
        })
    };

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let host = open(dir.path(), saves.clone()).await;
        assert!(list(&host).await.is_empty(), "cleared stays cleared");
        // Retried and failed again: a new item.
        put(
            dir.path(),
            &task("fail1", &b, "failed", now - 500, json!({})),
        );
        assert_eq!(
            ids(&list(&host).await),
            vec![format!("failed/fail1/{}", now - 500)]
        );
        host.call(
            "attention_clear",
            json!({ "id": format!("failed/fail1/{}", now - 500) }),
        )
        .await
        .unwrap();

        // A result posted into a chat while the person is elsewhere waits, until they open it.
        put(
            dir.path(),
            &task("done1", &b, "done", now - 100, json!({ "report_to": chat })),
        );
        post(dir.path(), &chat, "done1");
        host.call("new_session", json!({})).await.unwrap();
        assert_eq!(list(&host).await.len(), 1);
        host.call("open_session", json!({ "id": chat }))
            .await
            .unwrap();
        assert!(list(&host).await.is_empty(), "opening the chat cleared it");
        // Entering a project opens its latest chat: what was posted there is seen too.
        put(
            dir.path(),
            &task("done2", &b, "done", now - 50, json!({ "report_to": chat })),
        );
        host.call("switch_project", json!({ "name": "elsewhere" }))
            .await
            .unwrap();
        post(dir.path(), &chat, "done2");
        assert_eq!(list(&host).await.len(), 1);
        host.call("open_project", json!({ "id": b })).await.unwrap();
        assert!(
            list(&host).await.is_empty(),
            "entering the project cleared it"
        );
        // A question cannot be cleared.
        assert!(host
            .call("attention_clear", json!({ "id": "q-none" }))
            .await
            .is_err());
    });
}

/// A result posted into the chat the host has open is not seen until a window says it shows
/// that chat: the host has a current chat also when no window is open (a closed tab).
#[tokio::test]
async fn a_post_waits_until_a_window_shows_its_chat() {
    let dir = tempfile::tempdir().unwrap();
    let host = open(dir.path(), Arc::new(AtomicUsize::new(0))).await;
    let (_, chat) = chat_in(&host, "books").await;
    host.call(
        "spawn_agent",
        json!({ "spec": { "name": "scribe", "tools": ["notes.*"] } }),
    )
    .await
    .unwrap();
    let mut rx = host.subscribe();
    host.call(
        "submit_task",
        json!({ "agent": "scribe", "task": { "objective": "say hello", "report_to": chat } }),
    )
    .await
    .unwrap();
    wait_for(&mut rx, "the post", |v| v["kind"] == "post").await;
    wait_for(&mut rx, "the list to change", |v| v["kind"] == "attention").await;
    let items = list(&host).await;
    assert_eq!(items.len(), 1, "{items:#?}");
    assert_eq!(items[0]["kind"], "finished");
    assert_eq!(items[0]["chat"], chat.as_str());

    host.call("attention_seen", json!({ "session": chat }))
        .await
        .unwrap();
    assert!(list(&host).await.is_empty(), "the window showed it");
}
