//! Orchestration through the workspace protocol, with a mock model: agents and tasks created
//! from a window, an agent's approval that outlives a conversation turn, the timeline, messages,
//! and one orchestrator per project.

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
        vec![
            ActionDescriptor::new("notes.read", "read the notes", RiskTier::Read),
            ActionDescriptor::new("notes.save", "save the notes", RiskTier::Mutate),
        ]
    }
    async fn run(
        &self,
        action: &str,
        _i: Value,
        _c: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        if action == "notes.save" {
            self.saves.fetch_add(1, Ordering::SeqCst);
        }
        Ok(ActionOutput::json(json!({ "ok": true })))
    }
}

/// Calls `action` with `args` once per turn (or task), then answers "Saved.".
fn model(action: &'static str, args: Value) -> Arc<MockProvider> {
    Arc::new(MockProvider::new("m", vec![]).with_fallback(move |req| {
        if req.messages.iter().any(|m| m.tool_call_id.is_some()) {
            Completion {
                content: "Saved.".into(),
                tool_calls: vec![],
                usage: Usage::default(),
            }
        } else {
            Completion {
                content: String::new(),
                tool_calls: vec![ToolCall {
                    id: "c".into(),
                    name: action.into(),
                    arguments: args.clone(),
                }],
                usage: Usage::default(),
            }
        }
    }))
}

async fn open(dir: &std::path::Path, saves: Arc<AtomicUsize>) -> Arc<Host> {
    open_with(dir, saves, model("notes.save", json!({}))).await
}

async fn open_with(
    dir: &std::path::Path,
    saves: Arc<AtomicUsize>,
    model: Arc<MockProvider>,
) -> Arc<Host> {
    let provider: Arc<dyn ark_model::Provider> = model;
    let ark = Ark::builder()
        .store_root(dir)
        .tool(Arc::new(Notes { saves }))
        .profile(
            AgentProfile::builder()
                .core_actions(["notes.read", "notes.save"])
                .build(),
        )
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

/// The next event matching `f`.
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
        if f(&v) {
            return v;
        }
    }
}

fn orch_type(v: &Value, ty: &str) -> bool {
    v["kind"] == "orch" && v["event"]["type"] == ty
}

#[tokio::test]
async fn a_window_creates_agents_and_tasks_and_answers_their_questions() {
    let dir = tempfile::tempdir().unwrap();
    let saves = Arc::new(AtomicUsize::new(0));
    let host = open(dir.path(), saves.clone()).await;
    let mut rx = host.subscribe();

    // The app's actions, for an agent's tool patterns.
    let tools = host.call("tools", json!({})).await.unwrap();
    let names: Vec<&str> = tools
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"notes.save"), "{names:?}");

    let spec = json!({ "name": "scribe", "role": "keeps the notes", "tools": ["notes.*"] });
    let rec = host
        .call("spawn_agent", json!({ "spec": spec }))
        .await
        .unwrap();
    assert_eq!(rec["name"], "scribe");
    let agents = host.call("agents", json!({})).await.unwrap();
    assert_eq!(agents[0]["spec"]["role"], "keeps the notes");
    assert!(host
        .call("spawn_agent", json!({ "spec": { "name": "no spaces" } }))
        .await
        .is_err());
    assert!(host
        .call(
            "submit_task",
            json!({ "agent": "scribe", "task": { "objective": " " } })
        )
        .await
        .is_err());

    let r = host
        .call(
            "submit_task",
            json!({ "agent": "scribe", "task": { "objective": "save the notes" } }),
        )
        .await
        .unwrap();
    let task = r["task"].as_str().unwrap().to_string();

    // The agent's question names it and its task.
    let ask = wait_for(&mut rx, "agent approval", |v| v["kind"] == "approval").await;
    assert_eq!(ask["request"]["agent"], "scribe");
    assert_eq!(ask["request"]["task"], task.as_str());
    let agent_ask = ask["id"].as_u64().unwrap();

    // A conversation turn asks its own question and ends; the agent's question stays open.
    host.call("send", json!({ "text": "save mine too" }))
        .await
        .unwrap();
    let own = wait_for(&mut rx, "turn approval", |v| v["kind"] == "approval").await;
    assert!(own["request"]["agent"].is_null());
    host.call("approve", json!({ "id": own["id"], "decision": "deny" }))
        .await
        .unwrap();
    wait_for(&mut rx, "turn end", |v| v["kind"] == "turn_finished").await;
    let hello = host.call("hello", json!({})).await.unwrap();
    let pending: Vec<u64> = hello["approvals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_u64().unwrap())
        .collect();
    assert_eq!(
        pending,
        vec![agent_ask],
        "the agent's question outlives the turn"
    );
    assert_eq!(hello["approvals"][0]["request"]["agent"], "scribe");

    host.call(
        "approve",
        json!({ "id": agent_ask, "decision": "allow_once" }),
    )
    .await
    .unwrap();
    let done = wait_for(&mut rx, "task end", |v| orch_type(v, "task_finished")).await;
    assert_eq!(done["event"]["state"], "done");
    // Forwarded events carry the line a timeline shows.
    assert_eq!(done["line"], "done");
    assert_eq!(saves.load(Ordering::SeqCst), 1, "only the agent's save ran");

    // A window that opens late reads the stored events, worded the same way.
    let past = host
        .call("orch_events", json!({ "after": 0, "limit": 500 }))
        .await
        .unwrap();
    let past = past.as_array().unwrap();
    assert!(past
        .iter()
        .any(|e| e["event"]["type"] == "task_finished" && e["line"] == "done"));
    assert!(
        past.iter()
            .any(|e| e["activity"]["kind"] == "tool" && e["event"]["agent"] == "scribe"),
        "an agent's tool call reads as the conversation shows it"
    );

    let detail = host.call("task", json!({ "task": task })).await.unwrap();
    assert_eq!(detail["task"]["result"], "Saved.");
    let lines: Vec<&str> = detail["timeline"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["line"].as_str().unwrap())
        .collect();
    assert!(lines[0].starts_with("created: save the notes"), "{lines:?}");
    assert_eq!(*lines.last().unwrap(), "done");

    // A note for an idle agent waits in its inbox.
    let m = host
        .call(
            "message_agent",
            json!({ "agent": "scribe", "text": "use bullet points next time" }),
        )
        .await
        .unwrap();
    assert_eq!(m["delivered"], false);
    let g = host.call("graph", json!({})).await.unwrap();
    assert_eq!(g["agents"][0]["inbox"], 1);

    host.call("suspend_agent", json!({ "agent": "scribe" }))
        .await
        .unwrap();
    let g = host.call("graph", json!({})).await.unwrap();
    assert_eq!(g["agents"][0]["state"], "suspended");
    host.call("resume_agent", json!({ "agent": "scribe" }))
        .await
        .unwrap();
    assert!(host
        .call("cancel_task", json!({ "task": "t-nope" }))
        .await
        .is_err());
}

#[tokio::test]
async fn each_project_has_its_own_agents() {
    let dir = tempfile::tempdir().unwrap();
    let host = open(dir.path(), Arc::new(AtomicUsize::new(0))).await;
    host.call("spawn_agent", json!({ "spec": { "name": "a" } }))
        .await
        .unwrap();
    let first = host.call("graph", json!({})).await.unwrap()["project"].clone();

    host.call("switch_project", json!({ "name": "other" }))
        .await
        .unwrap();
    let g = host.call("graph", json!({})).await.unwrap();
    assert_ne!(g["project"], first);
    assert_eq!(g["agents"].as_array().unwrap().len(), 0);
    host.call("spawn_agent", json!({ "spec": { "name": "b" } }))
        .await
        .unwrap();

    // Events follow the current project's orchestrator.
    let mut rx = host.subscribe();
    host.call(
        "submit_task",
        json!({ "agent": "b", "task": { "objective": "x" } }),
    )
    .await
    .unwrap();
    let created = wait_for(&mut rx, "task created", |v| orch_type(v, "task_created")).await;
    assert_eq!(created["event"]["project"], g["project"]);

    host.call("switch_project", json!({ "name": "test" }))
        .await
        .unwrap();
    let g = host.call("graph", json!({})).await.unwrap();
    assert_eq!(g["project"], first);
    let names: Vec<&str> = g["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["a"]);
}

#[tokio::test]
async fn what_an_agent_produced_opens_in_the_window() {
    let dir = tempfile::tempdir().unwrap();
    let draw = model(
        "diagram.create",
        json!({ "kind": "flowchart", "title": "Flow", "source": "flowchart LR\n  a --> b" }),
    );
    let host = open_with(dir.path(), Arc::new(AtomicUsize::new(0)), draw).await;
    let mut rx = host.subscribe();
    host.call(
        "spawn_agent",
        json!({ "spec": { "name": "drawer", "tools": [], "diagrams": true } }),
    )
    .await
    .unwrap();
    host.call(
        "submit_task",
        json!({ "agent": "drawer", "task": { "objective": "draw the flow" } }),
    )
    .await
    .unwrap();
    let done = wait_for(&mut rx, "task end", |v| orch_type(v, "task_finished")).await;
    assert_eq!(done["event"]["state"], "done");
    let g = host.call("graph", json!({})).await.unwrap();
    let out = &g["tasks"][0]["outputs"][0];
    assert_eq!(out["kind"], "diagram");
    // The diagram is stored in the agent's own project; the window still opens it.
    let art = host
        .call("artifact", json!({ "id": out["id"] }))
        .await
        .unwrap();
    assert!(art["text"].as_str().unwrap().contains("a --> b"), "{art}");
}
