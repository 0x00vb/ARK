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

#[derive(Clone)]
struct Client(Arc<Host>);

impl std::ops::Deref for Client {
    type Target = Host;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Client {
    async fn call(&self, method: &str, input: Value) -> anyhow::Result<Value> {
        let operation = match method {
            "tools"
            | "spawn_agent"
            | "agents"
            | "submit_task"
            | "orch_events"
            | "task"
            | "task_call"
            | "message_agent"
            | "graph"
            | "suspend_agent"
            | "resume_agent"
            | "cancel_task"
            | "workspace"
            | "unschedule"
            | "agent_conversations"
            | "agent_conversation_start" => Some(("project", method, "")),
            "artifact" => Some(("artifact", method, input["id"].as_str().unwrap_or(""))),
            _ => None,
        };
        let Some((kind, operation, id)) = operation else {
            return self.0.call(method, input).await;
        };
        let location = self.0.call("summary", json!({})).await?["location"].clone();
        self.0
            .call(
                "resource",
                json!({
                    "ref": { "location": location, "kind": kind, "id": id },
                    "operation": { "kind": operation, "input": input },
                }),
            )
            .await
    }
}

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

async fn open(dir: &std::path::Path, saves: Arc<AtomicUsize>) -> Client {
    open_with(dir, saves, model("notes.save", json!({}))).await
}

async fn open_with(
    dir: &std::path::Path,
    saves: Arc<AtomicUsize>,
    model: Arc<MockProvider>,
) -> Client {
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
    Client(ArkUi::new(app).open().await.unwrap().0)
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
    // The person's own chat asks too, because they asked it to (a chat starts in Auto, ADR 0107).
    host.call("execution_mode", json!({ "mode": "manual", "revision": 0 }))
        .await
        .unwrap();
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

/// What an app needs to draw an agent's task as a chat (ADR 0076): its text as it is written
/// (live only, never numbered or kept), its work from `task` once the live stream has passed,
/// and one call's detail from `task_call`, only within that task.
#[tokio::test]
async fn an_agents_task_reads_as_a_chat() {
    let dir = tempfile::tempdir().unwrap();
    let saves = Arc::new(AtomicUsize::new(0));
    let model = Arc::new(MockProvider::new("m", vec![]).with_fallback(|req| {
        if req.messages.iter().any(|m| m.tool_call_id.is_some()) {
            Completion {
                content: "Saved the notes.".into(),
                tool_calls: vec![],
                usage: Usage::default(),
            }
        } else {
            Completion {
                content: "Saving the notes now.".into(),
                tool_calls: vec![ToolCall {
                    id: "c".into(),
                    name: "notes.save".into(),
                    arguments: json!({ "title": "Notes" }),
                }],
                usage: Usage::default(),
            }
        }
    }));
    let host = open_with(dir.path(), saves, model).await;
    let mut rx = host.subscribe();
    // A window that reads the graph follows the project's `orch` events.
    host.call("graph", json!({})).await.unwrap();
    host.call(
        "spawn_agent",
        json!({ "spec": { "name": "scribe", "tools": ["notes.*"] } }),
    )
    .await
    .unwrap();
    let r = host
        .call(
            "submit_task",
            json!({ "agent": "scribe", "task": { "objective": "save the notes" } }),
        )
        .await
        .unwrap();
    let task = r["task"].as_str().unwrap().to_string();

    let text = wait_for(&mut rx, "streamed text", |v| v["kind"] == "orch_text").await;
    assert_eq!(text["task"], task.as_str());
    assert_eq!(text["agent"], "scribe");
    assert!(text.get("seq").is_none(), "live only: {text}");
    let ask = wait_for(&mut rx, "agent approval", |v| v["kind"] == "approval").await;
    host.call(
        "approve",
        json!({ "id": ask["id"], "decision": "allow_once" }),
    )
    .await
    .unwrap();
    let chunks = std::cell::Cell::new(0);
    wait_for(&mut rx, "task finished", |v| {
        assert!(
            v["event"]["event"]["type"] != "text_delta",
            "streamed text is not an orch event: {v}"
        );
        chunks.set(chunks.get() + usize::from(v["kind"] == "orch_text"));
        orch_type(v, "task_finished")
    })
    .await;
    assert!(chunks.get() > 0, "the answer streamed");

    let detail = host.call("task", json!({ "task": task })).await.unwrap();
    let feed = detail["feed"].as_array().unwrap();
    let tool = feed
        .iter()
        .find(|m| m["activity"]["kind"] == "tool")
        .unwrap_or_else(|| panic!("no tool row in {feed:?}"));
    assert_eq!(tool["activity"]["call"], "c");
    assert!(
        feed.iter()
            .all(|m| m["event"]["event"]["type"] != "text_delta"),
        "streamed text is not kept"
    );

    let call = host
        .call("task_call", json!({ "task": task, "call": "c" }))
        .await
        .unwrap();
    assert_eq!(call["call"]["action"], "notes.save");
    assert_eq!(call["call"]["status"], "ok");
    assert_eq!(call["call"]["input"]["title"], "Notes");
    assert_eq!(call["call"]["approval"]["decision"], "allow_once");
    assert!(call["invocation"]["id"].is_string(), "{call}");
    assert!(host
        .call("task_call", json!({ "task": task, "call": "other" }))
        .await
        .is_err());
    assert!(host
        .call("task_call", json!({ "task": "t-nope", "call": "c" }))
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

#[tokio::test]
async fn the_project_folder_is_set_once_and_its_agents_follow_it() {
    let dir = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let host = open(dir.path(), Arc::new(AtomicUsize::new(0))).await;
    let before = host.call("workspace", json!({})).await.unwrap();
    assert_eq!(before["chosen"], false);
    // An agent created before the folder is chosen.
    let agent = host
        .call("spawn_agent", json!({ "spec": { "name": "w" } }))
        .await
        .unwrap();

    let err = host
        .call("workspace", json!({ "path": "/no/such/folder" }))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not a folder"), "{err}");
    let set = host
        .call("workspace", json!({ "path": folder.path() }))
        .await
        .unwrap();
    let want = folder.path().canonicalize().unwrap();
    assert_eq!(set["chosen"], true);
    assert_eq!(set["path"], json!(want));
    let summary = host.call("summary", json!({})).await.unwrap();
    assert_eq!(summary["project"]["workspace"], json!(want));

    // The terminal's command reads the same setting.
    let shown = host
        .call("command", json!({ "line": "/folder" }))
        .await
        .unwrap();
    assert!(
        shown.to_string().contains(&*want.to_string_lossy()),
        "{shown}"
    );

    // The agent's own project works in the new folder too.
    drop(host);
    let ark = Ark::builder().store_root(dir.path()).build().unwrap();
    let scope: ProjectId = serde_json::from_value(agent["scope"].clone()).unwrap();
    assert_eq!(ark.engine().workspace_dir(&scope).unwrap(), want);
}

/// The terminal's `/agent` and `/task` run the same controls as the window's methods: what
/// one does, the other sees.
#[tokio::test]
async fn terminal_commands_and_window_methods_share_one_control() {
    let dir = tempfile::tempdir().unwrap();
    let host = open(dir.path(), Arc::new(AtomicUsize::new(0))).await;
    let cmd = |line: &str| {
        let host = host.clone();
        let line = line.to_string();
        async move {
            host.call("command", json!({ "line": line }))
                .await
                .unwrap()
                .to_string()
        }
    };
    let state = |name: &'static str| {
        let host = host.clone();
        async move {
            let g = host.call("graph", json!({})).await.unwrap();
            g["agents"]
                .as_array()
                .unwrap()
                .iter()
                .find(|a| a["name"] == name)
                .map(|a| a["state"].as_str().unwrap().to_string())
        }
    };

    assert!(cmd("/agent new scribe").await.contains("scribe is ready"));
    assert_eq!(state("scribe").await.as_deref(), Some("idle"));
    assert!(cmd("/agent pause scribe").await.contains("scribe paused"));
    assert_eq!(state("scribe").await.as_deref(), Some("suspended"));

    // A task for a paused agent waits; the window's method cancels what the terminal started.
    let started = cmd("/task scribe save the notes").await;
    assert!(started.contains("started task t-"), "{started}");
    let g = host.call("graph", json!({})).await.unwrap();
    let id = g["tasks"][0]["task"].as_str().unwrap().to_string();
    let mut rx = host.subscribe();
    host.call("cancel_task", json!({ "task": id }))
        .await
        .unwrap();
    wait_for(&mut rx, "task_finished", |v| orch_type(v, "task_finished")).await;
    let t = host.call("task", json!({ "task": id })).await.unwrap();
    assert_eq!(t["task"]["state"], "cancelled");

    assert!(cmd("/agent message scribe use the short form")
        .await
        .contains("kept for scribe's next task"));
    // Schedules: the terminal's list and the window's stop share one control.
    assert!(cmd("/schedules").await.contains("Nothing is scheduled"));
    let e = host
        .call("unschedule", json!({ "schedule": "s-nope" }))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("no schedule `s-nope`"), "{e}");
    assert!(cmd("/schedules stop s-nope")
        .await
        .contains("no schedule `s-nope`"));
    assert!(cmd("/agent resume scribe").await.contains("scribe resumed"));
    assert!(cmd("/agent end scribe").await.contains("scribe ended"));
    assert_eq!(state("scribe").await.as_deref(), Some("terminated"));
    assert!(cmd("/agent").await.contains("usage: /agent"));
    assert!(cmd("/task scribe")
        .await
        .contains("a task needs an objective"));
}

#[tokio::test]
async fn a_scheduled_run_posts_its_result_in_the_chat_and_the_window_shows_it() {
    let dir = tempfile::tempdir().unwrap();
    // The chat schedules; the agent's run answers with what it found.
    let model = Arc::new(MockProvider::new("m", vec![]).with_fallback(|req| {
        let asked = req.messages.iter().any(|m| m.content.contains("every 5 minutes"));
        let done = req.messages.iter().any(|m| m.tool_call_id.is_some());
        let (content, calls) = match (asked, done) {
            (true, false) => (
                String::new(),
                vec![ToolCall {
                    id: "s".into(),
                    name: "agents.schedule".into(),
                    arguments: json!({ "to": "helper", "objective": "read the notes", "every_minutes": 5 }),
                }],
            ),
            (true, true) => ("Scheduled.".to_string(), vec![]),
            (false, _) => ("The notes say the locket is brass.".to_string(), vec![]),
        };
        Completion {
            content,
            tool_calls: calls,
            usage: Usage::default(),
        }
    }));
    let provider: Arc<dyn ark_model::Provider> = model;
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(provider)
        .allow_mutate(true)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    // The test drives the schedule's clock itself.
    let built: Arc<std::sync::Mutex<Option<Orchestrator>>> = Default::default();
    let keep = built.clone();
    let app = App::new("Test")
        .ark(ark)
        .orchestrate(move |ark, project, gate| {
            let o = Orchestrator::builder(ark, project)
                .approvals(gate)
                .build()?;
            *keep.lock().unwrap() = Some(o.clone());
            Ok(o)
        });
    let host = Client(ArkUi::new(app).open().await.unwrap().0);
    host.call("spawn_agent", json!({ "spec": { "name": "helper" } }))
        .await
        .unwrap();
    let mut rx = host.subscribe();
    host.call(
        "send",
        json!({ "text": "every 5 minutes have helper read the notes" }),
    )
    .await
    .unwrap();
    wait_for(&mut rx, "the turn's end", |v| v["kind"] == "turn_finished").await;
    let session = host.call("summary", json!({})).await.unwrap()["session"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let o = built.lock().unwrap().clone().unwrap();
    let next = o.schedules()[0].next_ms.unwrap();
    let run = o.tick_schedules(next).await;
    o.wait(&run[0]).await.unwrap();
    let post = wait_for(&mut rx, "the post", |v| v["kind"] == "post").await;
    assert_eq!(post["session"], json!(session));
    assert_eq!(post["post"]["from"], "helper");

    let turns = host.call("transcript", json!({})).await.unwrap();
    let last = turns.as_array().unwrap().last().unwrap().clone();
    assert_eq!(
        last["posts"][0]["text"],
        "The notes say the locket is brass."
    );
}

/// A question an agent asks outlives the process (ADR 0036): after a restart, `hello` lists it
/// with the question's id, the graph says the agent waits for the answer as the terminal's
/// `/agents` does, and answering it by that id runs the call once.
#[test]
fn an_agents_question_survives_a_restart_and_any_client_answers_it() {
    let dir = tempfile::tempdir().unwrap();
    let saves = Arc::new(AtomicUsize::new(0));

    // The first process: the agent asks, nobody answers, the process stops.
    let path = dir.path().to_path_buf();
    let s = saves.clone();
    let task = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let task = rt.block_on(async {
            let host = open(&path, s).await;
            let mut rx = host.subscribe();
            let spec = json!({ "name": "scribe", "role": "keeps the notes", "tools": ["notes.*"] });
            host.call("spawn_agent", json!({ "spec": spec }))
                .await
                .unwrap();
            let r = host
                .call(
                    "submit_task",
                    json!({ "agent": "scribe", "task": { "objective": "save the notes" } }),
                )
                .await
                .unwrap();
            wait_for(&mut rx, "agent approval", |v| v["kind"] == "approval").await;

            // While it waits, the window and the terminal say the same thing about it.
            let g = host.call("graph", json!({})).await.unwrap();
            let doing = g["agents"][0]["doing"].as_str().unwrap().to_string();
            assert!(doing.starts_with("Waiting for your answer"), "{doing}");
            let agents = host
                .call("command", json!({ "line": "/agents" }))
                .await
                .unwrap()
                .to_string();
            assert!(agents.contains(&doing), "{agents}");
            assert!(agents.contains("/task answer"), "{agents}");
            r["task"].as_str().unwrap().to_string()
        });
        drop(rt);
        task
    })
    .join()
    .unwrap();

    // The process is gone: the task's record names a process that no longer runs.
    {
        let ark = Ark::builder().store_root(dir.path()).build().unwrap();
        let store = &ark.engine().store;
        let mut rec = store.get_record("orch_task", &task).unwrap().unwrap();
        assert_eq!(rec["question"]["request"]["action"], "notes.save");
        rec["pid"] = json!(999_999);
        let scope = rec["project"].as_str().unwrap().to_string();
        store.put_record("orch_task", &task, &scope, &rec).unwrap();
    }

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let host = open(dir.path(), saves.clone()).await;
        let mut rx = host.subscribe();
        let hello = host.call("hello", json!({})).await.unwrap();
        let kept = &hello["approvals"][0];
        assert_eq!(kept["request"]["task"], task.as_str());
        let id = kept["id"]
            .as_str()
            .expect("a kept question's id is its own")
            .to_string();
        // The agent is free while its task waits, and still says what it waits for.
        let g = host.call("graph", json!({})).await.unwrap();
        let doing = g["agents"][0]["doing"].as_str().unwrap_or_default();
        assert!(doing.starts_with("Waiting for your answer"), "{doing}");
        assert_eq!(saves.load(Ordering::SeqCst), 0);

        host.call("approve", json!({ "id": id, "decision": "allow_once" }))
            .await
            .unwrap();
        let done = wait_for(&mut rx, "task end", |v| orch_type(v, "task_finished")).await;
        assert_eq!(done["event"]["state"], "done");
        assert_eq!(saves.load(Ordering::SeqCst), 1, "the call ran once");
        let hello = host.call("hello", json!({})).await.unwrap();
        assert!(hello["approvals"].as_array().unwrap().is_empty());
    });
}

#[tokio::test]
async fn agent_contact_search_reads_other_projects_without_opening_or_running_them() {
    let dir = tempfile::tempdir().unwrap();
    let host = open(dir.path(), Arc::new(AtomicUsize::new(0))).await;
    let first = host.call("summary", json!({})).await.unwrap();
    let first_id = first["project"]["id"].as_str().unwrap().to_string();
    host.call("spawn_agent", json!({"spec":{"name":"archivist","role":"First project","instructions":"PRIVATE-INSTRUCTIONS"}})).await.unwrap();
    host.call("add_project", json!({"name":"Second project"}))
        .await
        .unwrap();
    let second = host.call("summary", json!({})).await.unwrap();
    let second_id = second["project"]["id"].as_str().unwrap().to_string();
    assert_ne!(first_id, second_id);
    host.call("spawn_agent", json!({"spec":{"name":"archivist","role":"Second project","instructions":"OTHER-PRIVATE-INSTRUCTIONS"}})).await.unwrap();
    host.call("open_project", json!({"id":first_id}))
        .await
        .unwrap();
    let before = host.call("summary", json!({})).await.unwrap();
    let a = host
        .call("agent_contacts", json!({"project":first_id}))
        .await
        .unwrap();
    let b = host
        .call("agent_contacts", json!({"project":second_id}))
        .await
        .unwrap();
    assert_eq!(a[0]["role"], "First project");
    assert_eq!(b[0]["role"], "Second project");
    assert_eq!(a[0]["name"], b[0]["name"]);
    assert!(a[0].get("spec").is_none());
    assert!(a[0].get("instructions").is_none());
    assert!(a[0].get("session").is_none());
    assert_eq!(
        host.call("summary", json!({})).await.unwrap()["location"],
        before["location"]
    );
    assert!(host
        .call("agent_contacts", json!({"project":"missing"}))
        .await
        .is_err());
    assert_eq!(
        host.call("graph", json!({})).await.unwrap()["tasks"],
        json!([])
    );
}

#[tokio::test]
async fn new_chat_agent_creation_is_origin_scoped_and_never_upserts() {
    let dir = tempfile::tempdir().unwrap();
    let host = open(dir.path(), Arc::new(AtomicUsize::new(0))).await;
    let summary = host.call("hello", json!({})).await.unwrap()["summary"].clone();
    let reference =
        json!({"kind":"project", "id":summary["project"]["id"], "location":summary["location"]});
    let input =
        json!({"request_id":"creation-one", "spec":{"name":"growth","role":"Find opportunities"}});
    let call = |input| json!({"ref":reference, "operation":{"kind":"create_agent","input":input}});
    let first = host.call("resource", call(input.clone())).await.unwrap();
    let again = host.call("resource", call(input.clone())).await.unwrap();
    assert_eq!(first["scope"], again["scope"]);
    let mut changed = input.clone();
    changed["spec"]["role"] = json!("Replace role");
    assert!(host.call("resource", call(changed)).await.is_err());
    assert_eq!(
        host.call("agents", json!({})).await.unwrap()[0]["spec"]["role"],
        "Find opportunities"
    );
    let mut stale = reference.clone();
    stale["location"]["project"] = json!("missing-project");
    assert!(host.call("resource", json!({"ref":stale,"operation":{"kind":"create_agent","input":{"request_id":"two","spec":{"name":"other"}}}})).await.is_err());
    assert_eq!(
        host.call("agents", json!({}))
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn chat_turn_request_retries_exact_payload_and_refuses_unsupported_fields() {
    let dir = tempfile::tempdir().unwrap();
    let host = open(dir.path(), Arc::new(AtomicUsize::new(0))).await;
    host.call("spawn_agent", json!({"spec":{"name":"writer"}}))
        .await
        .unwrap();
    let input = json!({"agent":"writer","request_id":"first-message","task":{"objective":"Review this","context":"original context"}});
    let first = host.call("submit_task", input.clone()).await.unwrap();
    let retry = host.call("submit_task", input.clone()).await.unwrap();
    assert_eq!(first["task"], retry["task"]);
    for field in ["objective", "context"] {
        let mut changed = input.clone();
        changed["task"][field] = json!("different");
        assert!(host.call("submit_task", changed).await.is_err());
    }
    for field in ["artifacts", "after", "budget", "max_attempts", "unknown"] {
        let mut changed = input.clone();
        changed["task"][field] = json!(null);
        assert!(host.call("submit_task", changed).await.is_err());
    }
    for task in [
        json!(null),
        json!([]),
        json!("text"),
        json!({"objective":""}),
        json!({"objective":1}),
        json!({"objective":"ok","context":false}),
    ] {
        let mut changed = input.clone();
        changed["task"] = task;
        assert!(host.call("submit_task", changed).await.is_err());
    }
    for request in [json!(null), json!(1), json!(""), json!("x".repeat(129))] {
        let mut changed = input.clone();
        changed["request_id"] = request;
        assert!(host.call("submit_task", changed).await.is_err());
    }
}

#[tokio::test]
async fn work_and_automation_lists_subscribe_to_project_changes_without_opening_the_board() {
    for method in ["work_list", "automations"] {
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path(), Arc::default()).await;
        let location = host.call("summary", json!({})).await.unwrap()["location"].clone();
        host.0.call("resource",json!({"ref":{"location":location,"kind":"project","id":""},"operation":{"kind":method,"input":{}}})).await.unwrap();
        let mut events = host.subscribe();
        host.call("spawn_agent", json!({"spec":{"name":"new-helper"}}))
            .await
            .unwrap();
        let event = wait_for(&mut events, "scoped project update", |event| {
            orch_type(event, "agent_spawned") && event.get("ref").is_some()
        })
        .await;
        assert_eq!(event["ref"]["location"], location);
    }
}

#[tokio::test]
async fn an_agent_has_conversations_of_its_own_and_the_window_lists_every_agent_and_chat() {
    let dir = tempfile::tempdir().unwrap();
    let model = Arc::new(
        MockProvider::new("m", vec![]).with_fallback(|_| Completion {
            content: "Done.".into(),
            tool_calls: vec![],
            usage: Usage::default(),
        }),
    );
    let host = open_with(dir.path(), Arc::new(AtomicUsize::new(0)), model).await;
    host.call(
        "spawn_agent",
        json!({"spec":{"name":"writer","title":"Writer"}}),
    )
    .await
    .unwrap();
    let location = host.call("summary", json!({})).await.unwrap()["location"].clone();
    let project = location["project"].as_str().unwrap().to_string();

    let started = host
        .call("agent_conversation_start", json!({"agent":"writer"}))
        .await
        .unwrap();
    let conversation = started["conversation"].as_str().unwrap().to_string();
    let empty = host
        .call("agent_conversations", json!({"agent":"writer"}))
        .await
        .unwrap();
    assert_eq!(
        empty,
        json!([]),
        "a conversation without turns is not listed"
    );

    let input = json!({"agent":"writer","request_id":"m1","task":{"objective":"Draft it","thread":conversation}});
    let task = host.call("submit_task", input.clone()).await.unwrap()["task"].clone();
    let retry = host.call("submit_task", input.clone()).await.unwrap()["task"].clone();
    assert_eq!(task, retry);
    let mut elsewhere = input.clone();
    elsewhere["task"]["thread"] = json!(location["session"]);
    assert!(
        host.call("submit_task", elsewhere).await.is_err(),
        "the person's own chat is no conversation of the agent's"
    );
    let mut done = Value::Null;
    for _ in 0..500 {
        done = host.call("task", json!({"task":task})).await.unwrap()["task"].clone();
        if done["state"] == "done" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(done["thread"], conversation.as_str());

    let rows = host
        .call("agent_conversations", json!({"agent":"writer"}))
        .await
        .unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1, "{rows}");
    assert_eq!(rows[0]["id"], conversation.as_str());
    assert_eq!(rows[0]["turns"], 1);
    assert_eq!(rows[0]["running"], Value::Null);
    assert_eq!(rows[0]["asking"], false);
    assert_eq!(rows[0]["default"], false);
    let read = host
        .call(
            "agent_conversations",
            json!({"agent":"writer","project":project}),
        )
        .await
        .unwrap();
    assert_eq!(read, rows, "read from the store alike");
    assert!(host
        .call(
            "agent_conversations",
            json!({"agent":"writer","project":"missing"}),
        )
        .await
        .is_err());

    let m = host
        .call(
            "message_agent",
            json!({"agent":"writer","text":"shorter next time","thread":conversation}),
        )
        .await
        .unwrap();
    assert_eq!(m["delivered"], false);
    let agents = host.call("agents", json!({})).await.unwrap();
    assert_eq!(agents[0]["inbox"][0]["thread"], conversation.as_str());

    let scope = agents[0]["scope"].as_str().unwrap().to_string();
    for all in [
        host.call("agents", json!({"all":true})).await.unwrap(),
        host.0.call("agents", json!({"all":true})).await.unwrap(),
    ] {
        let rows = all.as_array().unwrap();
        assert_eq!(rows.len(), 1, "{all}");
        let row = &rows[0];
        assert_eq!(row["project"], project.as_str());
        assert!(row["project_name"].is_string());
        assert_eq!(row["name"], "writer");
        assert_eq!(row["title"], "Writer");
        assert_eq!(row["state"], "idle");
        assert_eq!(row["asking"], false);
        assert_eq!(row["program"], false);
        assert_eq!(row["running"], json!([]));
        assert!(row["preview"].is_string(), "{row}");
        assert!(row["updated_ms"].as_u64().unwrap() > 0);
    }
    {
        let ark = Ark::builder().store_root(dir.path()).build().unwrap();
        ark.engine()
            .store
            .put_record(
                "orch_task",
                "t-damaged",
                &project,
                &json!({"damaged": true}),
            )
            .unwrap();
    }
    let all = host.call("agents", json!({"all":true})).await.unwrap();
    assert_eq!(
        all.as_array().unwrap().len(),
        1,
        "a damaged task record hides no agent: {all}"
    );

    let general = host
        .call("sessions", json!({"general":true}))
        .await
        .unwrap();
    let general = general.as_array().unwrap();
    assert!(general.iter().any(|r| r["id"] == location["session"]));
    assert!(general
        .iter()
        .all(|r| r["project"] == project.as_str() && r["project_name"].is_string()));
    assert!(general
        .iter()
        .all(|r| r["project"] != scope.as_str() && r["id"] != conversation.as_str()));
}
