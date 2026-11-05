//! The system prompt is the same for every turn of a profile, so a vendor that caches it only
//! whole (Claude Code) serves it from its cache when the next vendor session opens.

use std::sync::Arc;

use ark_core::{ActionDescriptor, RiskTier};
use ark_model::{Completion, MockProvider, Provider, ToolCall, Usage};
use ark_runtime::{AgentProfile, Engine, Harness, ModelRouter};
use ark_tools::{ActionOutput, Adapter, Registry, RunCtx};
use serde_json::json;

struct Kv;

#[async_trait::async_trait]
impl Adapter for Kv {
    fn name(&self) -> &str {
        "kv"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new("kv.list", "list the stored keys", RiskTier::Read),
            ActionDescriptor::new(
                "kv.get",
                "read the value stored under a key",
                RiskTier::Read,
            ),
        ]
    }
    async fn run(
        &self,
        _a: &str,
        i: serde_json::Value,
        _c: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        Ok(ActionOutput::json(json!({ "key": i["key"], "value": 42 })))
    }
}

const TASK: &str = "read the value stored under key a";

fn get_a() -> Completion {
    Completion {
        content: String::new(),
        tool_calls: vec![ToolCall {
            id: "c1".into(),
            name: "kv.get".into(),
            arguments: json!({ "key": "a" }),
        }],
        usage: Usage::measured(10, 5),
    }
}

fn answer() -> Completion {
    Completion {
        content: "The value under a is 42.".into(),
        tool_calls: vec![],
        usage: Usage::measured(10, 5),
    }
}

fn profile() -> AgentProfile {
    AgentProfile::builder().core_actions(["kv.list"]).build()
}

async fn ask(engine: &Engine, project: &str, model: Arc<MockProvider>) {
    let project = engine.create_project(project, "keys").unwrap();
    let p: Arc<dyn Provider> = model;
    Harness::new(ModelRouter::single(p))
        .preload(2)
        .max_steps(4)
        .ask(engine, &profile(), &project.id, TASK, |_| {})
        .await
        .unwrap();
}

fn tool_names(model: &MockProvider, request: usize) -> Vec<String> {
    model.recorded()[request]
        .tools
        .iter()
        .map(|t| t.name.clone())
        .collect()
}

#[tokio::test]
async fn the_system_prompt_names_no_project() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open_with(dir.path(), Registry::new().with(Arc::new(Kv))).unwrap();
    let a = Arc::new(MockProvider::new("m", vec![answer()]));
    let b = Arc::new(MockProvider::new("m", vec![answer()]));
    ask(&engine, "alpha", a.clone()).await;
    ask(&engine, "beta", b.clone()).await;
    let (a, b) = (&a.recorded()[0], &b.recorded()[0]);
    assert_eq!(a.system, b.system, "same profile, same system prompt");
    assert!(!a.system.contains("alpha"));
    assert!(a.messages[0]
        .content
        .contains("== PROJECT ==\nProject: alpha"));
    assert!(b.messages[0].content.contains("Project: beta"));
}

#[tokio::test]
async fn a_whole_system_cache_gets_preloaded_actions_after_the_first_call() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open_with(dir.path(), Registry::new().with(Arc::new(Kv))).unwrap();
    let model = Arc::new(MockProvider::new("m", vec![get_a(), answer()]).with_whole_system_cache());
    ask(&engine, "p", model.clone()).await;

    let first = tool_names(&model, 0);
    assert!(
        first.contains(&"kv.list".to_string()),
        "core actions open the session"
    );
    assert!(
        !first.contains(&"kv.get".to_string()),
        "the preload waits: {first:?}"
    );
    let ready = &model.recorded()[0].messages[0].content;
    assert!(
        ready.contains("== READY TO CALL ==") && ready.contains("- kv.get:"),
        "{ready}"
    );

    // The direct call on the first step ran, and the next request carries the action.
    let second = &model.recorded()[1];
    assert!(second.tools.iter().any(|t| t.name == "kv.get"));
    assert!(
        second.messages.iter().any(|m| m.content.contains("42")),
        "the held action ran on the first step"
    );
}

#[tokio::test]
async fn other_providers_get_preloaded_actions_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open_with(dir.path(), Registry::new().with(Arc::new(Kv))).unwrap();
    let model = Arc::new(MockProvider::new("m", vec![answer()]));
    ask(&engine, "p", model.clone()).await;
    let first = tool_names(&model, 0);
    assert!(first.contains(&"kv.list".to_string()) && first.contains(&"kv.get".to_string()));
}
