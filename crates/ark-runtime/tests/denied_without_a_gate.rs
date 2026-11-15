//! With no one to ask, a state-changing call is refused, and the refusal does not tell the
//! model that it is read-only: only that no one can approve here.

use std::sync::Arc;

use ark_core::{ActionDescriptor, RiskTier};
use ark_model::{Completion, MockProvider, Provider, ToolCall, Usage};
use ark_runtime::{AgentProfile, Engine, Harness, ModelRouter};
use ark_tools::{ActionOutput, Adapter, Registry, RunCtx};
use serde_json::{json, Value};

struct Doc;

#[async_trait::async_trait]
impl Adapter for Doc {
    fn name(&self) -> &str {
        "doc"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new("doc.save", "save", RiskTier::Mutate)]
    }
    async fn run(&self, _: &str, _: Value, _: &RunCtx<'_>) -> ark_tools::Result<ActionOutput> {
        Ok(ActionOutput::json(json!({ "saved": true })))
    }
}

#[tokio::test]
async fn the_refusal_without_a_gate_does_not_call_the_agent_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open_with(dir.path(), Registry::new().with(Arc::new(Doc))).unwrap();
    let project = engine.create_project("p", "").unwrap();
    let call = ToolCall {
        id: "c0".into(),
        name: "doc.save".into(),
        arguments: json!({}),
    };
    let model = Arc::new(MockProvider::new(
        "m",
        vec![
            Completion {
                content: String::new(),
                tool_calls: vec![call],
                usage: Usage::measured(10, 5),
            },
            Completion {
                content: "It needs approval, which nobody can give here.".into(),
                tool_calls: vec![],
                usage: Usage::measured(10, 5),
            },
        ],
    ));
    let provider: Arc<dyn Provider> = model.clone();
    let profile = AgentProfile::builder().core_actions(["doc.save"]).build();
    Harness::new(ModelRouter::single(provider))
        .planner(false)
        .critic(false)
        .ask(&engine, &profile, &project.id, "save it", |_| {})
        .await
        .unwrap();
    let requests = model.recorded();
    let denial = requests[1]
        .messages
        .iter()
        .find(|m| m.content.starts_with("denied:"))
        .expect("the refusal reached the model")
        .content
        .clone();
    assert!(denial.contains("needs human approval"), "{denial}");
    assert!(!denial.contains("read-only"), "{denial}");
}
