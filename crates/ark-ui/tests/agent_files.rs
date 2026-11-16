//! Files in an agent's chat resolve in that agent's own folder (ADR 0124): a chat's file
//! location names the chat, and the host reads, lists, uploads and watches in the folder of the
//! agent whose chat it is. Anything that is not an agent chat of the location's project keeps
//! the refusal.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ark_interact::ark_orchestrate::Orchestrator;
use ark_interact::App;
use ark_sdk::prelude::ark_model::{Completion, MockProvider, ToolCall, Usage};
use ark_sdk::prelude::*;
use ark_ui::{ArkUi, Host};
use base64::Engine as _;
use serde_json::{json, Value};

struct Notes;

#[async_trait::async_trait]
impl Adapter for Notes {
    fn name(&self) -> &str {
        "notes"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            "notes.jot",
            "jot a line in the working folder",
            RiskTier::Read,
        )]
    }
    async fn run(&self, _: &str, _: Value, ctx: &RunCtx<'_>) -> ark_tools::Result<ActionOutput> {
        let dir = ctx.workspace.clone().expect("a working folder");
        std::fs::write(dir.join("jot.md"), "a line\n").unwrap();
        Ok(ActionOutput::json(json!({ "ok": true })))
    }
}

fn model() -> Arc<MockProvider> {
    Arc::new(MockProvider::new("m", vec![]).with_fallback(|req| {
        let called = req.messages.iter().any(|m| m.tool_call_id.is_some());
        let jot = req
            .messages
            .iter()
            .any(|m| m.content.contains("jot it down"));
        if jot && !called {
            Completion {
                content: String::new(),
                tool_calls: vec![ToolCall {
                    id: "c".into(),
                    name: "notes.jot".into(),
                    arguments: json!({}),
                }],
                usage: Usage::default(),
            }
        } else {
            Completion {
                content: "Done.".into(),
                tool_calls: vec![],
                usage: Usage::default(),
            }
        }
    }))
}

async fn open(dir: &Path) -> Arc<Host> {
    let provider: Arc<dyn ark_model::Provider> = model();
    let ark = Ark::builder()
        .store_root(dir)
        .tool(Arc::new(Notes))
        .profile(AgentProfile::builder().core_actions(["notes.jot"]).build())
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

async fn window(host: &Arc<Host>) -> Value {
    host.call("summary", json!({})).await.unwrap()["location"].clone()
}

async fn project_call(host: &Arc<Host>, operation: &str, input: Value) -> anyhow::Result<Value> {
    let location = window(host).await;
    host.call(
        "resource",
        json!({
            "ref": { "location": location, "kind": "project", "id": "" },
            "operation": { "kind": operation, "input": input },
        }),
    )
    .await
}

/// A new agent with its own folder (when `folder`), and a new chat of it. The chat's file
/// location is the window's, naming that chat.
async fn agent_chat(host: &Arc<Host>, name: &str, folder: Option<&Path>) -> Value {
    project_call(host, "spawn_agent", json!({ "spec": { "name": name } }))
        .await
        .unwrap();
    if let Some(folder) = folder {
        let project = window(host).await["project"].clone();
        host.call(
            "agent_folder",
            json!({ "agent": name, "project": project, "path": folder }),
        )
        .await
        .unwrap();
    }
    let started = project_call(host, "agent_conversation_start", json!({ "agent": name }))
        .await
        .unwrap();
    at(
        &window(host).await,
        started["conversation"].as_str().unwrap(),
    )
}

fn at(location: &Value, session: &str) -> Value {
    let mut location = location.clone();
    location["session"] = json!(session);
    location
}

async fn file_op(
    host: &Arc<Host>,
    location: &Value,
    operation: &str,
    id: &str,
    input: Value,
) -> anyhow::Result<Value> {
    host.call(
        "resource",
        json!({
            "ref": { "location": location, "kind": "file", "id": id },
            "operation": { "kind": operation, "input": input },
        }),
    )
    .await
}

fn folder(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::create_dir_all(&path).unwrap();
    std::fs::canonicalize(path).unwrap()
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

#[tokio::test(flavor = "multi_thread")]
async fn files_of_an_agent_chat_open_list_and_upload_in_the_agents_folder() {
    let dir = tempfile::tempdir().unwrap();
    let own = folder(dir.path(), "ec");
    let host = open(&dir.path().join("store")).await;
    std::fs::write(own.join("clients.csv"), "name,city\nAda,London\n").unwrap();
    let chat = agent_chat(&host, "scout", Some(&own)).await;

    let read = file_op(&host, &chat, "file_read", "clients.csv", json!({}))
        .await
        .unwrap();
    assert_eq!(read["kind"], "sheet", "{read}");

    let listed = file_op(&host, &chat, "files", "", json!({})).await.unwrap();
    assert_eq!(listed["root"], json!(own), "{listed}");
    assert!(listed["entries"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["path"] == "clients.csv"));

    let upload = "up-agent-chat-1";
    let data = base64::engine::general_purpose::STANDARD.encode("a,b\n1,2\n");
    file_op(
        &host,
        &chat,
        "upload_parts",
        "",
        json!({ "upload": upload, "parts": [] }),
    )
    .await
    .unwrap();
    file_op(
        &host,
        &chat,
        "upload_parts",
        "",
        json!({ "upload": upload, "parts": [{ "path": "orders.csv", "offset": 0, "data": data }] }),
    )
    .await
    .unwrap();
    let added = file_op(&host, &chat, "upload_add", "", json!({ "upload": upload }))
        .await
        .unwrap();
    assert_eq!(added["added"], json!(["orders.csv"]), "{added}");
    assert!(
        own.join("orders.csv").is_file(),
        "the upload landed in the agent's folder"
    );
    let top = host.call("summary", json!({})).await.unwrap()["project"]["workspace"]
        .as_str()
        .map(PathBuf::from);
    if let Some(top) = top {
        assert!(!top.join("orders.csv").exists(), "not in the top folder");
    }

    let hello = host.call("hello", json!({})).await.unwrap();
    assert_eq!(hello["agent_chat_files"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_without_a_folder_works_in_the_project_folder() {
    let dir = tempfile::tempdir().unwrap();
    let top = folder(dir.path(), "top");
    let host = open(&dir.path().join("store")).await;
    let window_location = window(&host).await;
    host.call(
        "resource",
        json!({
            "ref": { "location": window_location, "kind": "project", "id": "" },
            "operation": { "kind": "workspace", "input": { "path": top } },
        }),
    )
    .await
    .unwrap();
    std::fs::write(top.join("plan.md"), "plan\n").unwrap();
    let chat = agent_chat(&host, "plain", None).await;
    let read = file_op(&host, &chat, "file_read", "plan.md", json!({}))
        .await
        .unwrap();
    assert_eq!(read["text"], "plan\n", "{read}");
    let listed = file_op(&host, &chat, "files", "", json!({})).await.unwrap();
    assert_eq!(listed["root"], json!(top));
}

#[tokio::test(flavor = "multi_thread")]
async fn only_an_agent_chat_of_the_locations_project_names_an_agent_folder() {
    let dir = tempfile::tempdir().unwrap();
    let own = folder(dir.path(), "own");
    let host = open(&dir.path().join("store")).await;
    std::fs::write(own.join("secret.txt"), "own\n").unwrap();
    let chat = agent_chat(&host, "scout", Some(&own)).await;
    let read = |location: &Value| {
        let (host, location) = (host.clone(), location.clone());
        async move {
            file_op(&host, &location, "file_read", "secret.txt", json!({}))
                .await
                .map_err(|e| e.to_string())
        }
    };
    assert!(read(&chat).await.is_ok());

    // A chat of another project, with an agent of its own, names nothing here.
    let first = window(&host).await;
    host.call("add_project", json!({ "name": "Second" }))
        .await
        .unwrap();
    let second = window(&host).await;
    let foreign = agent_chat(&host, "other", Some(&folder(dir.path(), "foreign"))).await;
    std::fs::write(dir.path().join("foreign/secret.txt"), "foreign\n").unwrap();
    host.call("open_project", json!({ "id": first["project"] }))
        .await
        .unwrap();
    let crossed = at(&first, foreign["session"].as_str().unwrap());
    let e = read(&crossed).await.unwrap_err();
    assert!(e.contains("does not match"), "{e}");
    let again = at(&second, foreign["session"].as_str().unwrap());
    assert!(read(&again).await.is_ok(), "its own project still reads it");

    // A chat that does not exist, a runtime that is not this one.
    let e = read(&at(&first, "no-such-chat")).await.unwrap_err();
    assert!(e.contains("no chat"), "{e}");
    let mut elsewhere = chat.clone();
    elsewhere["runtime"] = json!("another-runtime");
    let e = read(&elsewhere).await.unwrap_err();
    assert!(
        e.contains("no linked chat owns") || e.contains("another runtime"),
        "{e}"
    );

    // A work item's or an automation's conversation in the agent's scope is not a chat of it.
    let store = ark_store::Store::open(dir.path().join("store")).unwrap();
    let id = chat["session"].as_str().unwrap();
    let mut record = store.get_session(id).unwrap().unwrap();
    for field in [
        ("work_item", json!("item-1")),
        ("execution_scope", json!("some-project")),
        ("execution_managed", json!(true)),
    ] {
        let original = record.clone();
        record[field.0] = field.1;
        let scope = ProjectId(record["project"].as_str().unwrap().to_string());
        store.put_session(id, &scope, &record).unwrap();
        let e = read(&chat).await.unwrap_err();
        assert!(e.contains("does not match"), "{}: {e}", field.0);
        store.put_session(id, &scope, &original).unwrap();
        record = original;
    }
    assert!(read(&chat).await.is_ok(), "restored");

    // An agent that was ended has no chat to read files for.
    project_call(&host, "terminate_agent", json!({ "agent": "scout" }))
        .await
        .unwrap();
    let e = read(&chat).await.unwrap_err();
    assert!(e.contains("does not match"), "{e}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_write_in_an_agent_chat_refreshes_the_views_of_that_chat() {
    let dir = tempfile::tempdir().unwrap();
    let own = folder(dir.path(), "own");
    let host = open(&dir.path().join("store")).await;
    std::fs::write(own.join("notes.md"), "one\n").unwrap();
    let chat = agent_chat(&host, "scout", Some(&own)).await;
    file_op(&host, &chat, "file_read", "notes.md", json!({}))
        .await
        .unwrap();
    let mut events = host.subscribe();
    file_op(
        &host,
        &chat,
        "file_write",
        "notes.md",
        json!({ "text": "two\n" }),
    )
    .await
    .unwrap();
    let mut seen = false;
    until("a files_changed event for the chat", async || {
        while let Ok(event) = events.try_recv() {
            if event["kind"] == "files_changed" && event["ref"]["location"] == chat {
                seen = true;
            }
        }
        seen
    })
    .await;
    assert_eq!(
        std::fs::read_to_string(own.join("notes.md")).unwrap(),
        "two\n"
    );

    // What the agent itself writes in its turn is in the folder the chat reads.
    let task = project_call(
        &host,
        "submit_task",
        json!({ "agent": "scout", "task": { "objective": "jot it down", "thread": chat["session"] } }),
    )
    .await
    .unwrap();
    assert!(task["task"].is_string());
    until("the agent's jot", async || own.join("jot.md").is_file()).await;
    let read = file_op(&host, &chat, "file_read", "jot.md", json!({}))
        .await
        .unwrap();
    assert_eq!(read["text"], "a line\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_that_is_not_in_the_agents_folder_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let own = folder(dir.path(), "ec");
    let host = open(&dir.path().join("store")).await;
    let chat = agent_chat(&host, "scout", Some(&own)).await;
    for (operation, id) in [
        ("file_read", "04-client-crm.xlsx"),
        ("file_media", "logo.png"),
        ("file_version", "brand/BRAND-GUIDE.md"),
        ("file_open", "dist/report.pdf"),
    ] {
        let e = file_op(&host, &chat, operation, id, json!({}))
            .await
            .unwrap_err()
            .to_string();
        let name = id.rsplit('/').next().unwrap();
        assert_eq!(e, format!("{name} is not in ec"), "{operation}: {e}");
    }
}
