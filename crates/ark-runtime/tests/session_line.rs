//! The first message of a turn says what the session has, built from the registry and not from
//! the actions offered in that request (ADR 0108, ADR 0102).

use std::sync::Arc;

use ark_core::{ActionDescriptor, RiskTier};
use ark_model::{Completion, MockProvider, Provider, Usage};
use ark_runtime::{AgentProfile, Engine, Harness, ModelRouter};
use ark_tools::{ActionOutput, Adapter, Registry, RunCtx};
use serde_json::Value;

struct Named(&'static str, &'static str);

#[async_trait::async_trait]
impl Adapter for Named {
    fn name(&self) -> &str {
        self.0
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(self.0, self.1, RiskTier::Read)]
    }
    async fn run(&self, _a: &str, _i: Value, _c: &RunCtx<'_>) -> ark_tools::Result<ActionOutput> {
        Ok(ActionOutput::json(serde_json::json!({})))
    }
}

struct Sent {
    system: String,
    tools: Vec<String>,
}

fn answer() -> Completion {
    Completion {
        content: "ok".into(),
        tool_calls: vec![],
        usage: Usage::measured(10, 5),
    }
}

async fn first_request(registry: Registry, model: MockProvider) -> (Sent, String) {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open_with(dir.path(), registry).unwrap();
    let project = engine.create_project("p", "").unwrap();
    let model = Arc::new(model);
    let p: Arc<dyn Provider> = model.clone();
    let profile = AgentProfile::builder().core_actions(["other.one"]).build();
    Harness::new(ModelRouter::single(p))
        .preload(2)
        .max_steps(4)
        .ask(
            &engine,
            &profile,
            &project.id,
            "start the dev server in the terminal",
            |_| {},
        )
        .await
        .unwrap();
    let first = &model.recorded()[0];
    let text = first.messages[0].content.clone();
    let sent = Sent {
        system: first.system.clone(),
        tools: first.tools.iter().map(|t| t.name.clone()).collect(),
    };
    (sent, text)
}

fn line(text: &str) -> String {
    text.lines()
        .skip_while(|l| !l.starts_with("== THIS SESSION =="))
        .nth(1)
        .unwrap_or("")
        .to_string()
}

fn registry(extra: &[(&'static str, &'static str)]) -> Registry {
    let mut r = Registry::new().with(Arc::new(Named("other.one", "an unrelated action")));
    for (name, about) in extra {
        r = r.with(Arc::new(Named(name, about)));
    }
    r
}

#[tokio::test]
async fn the_first_message_says_what_the_session_has() {
    let r = registry(&[
        (
            "process.start",
            "start a terminal process in the project folder",
        ),
        ("browser.open", "open a local page in the browser"),
    ]);
    let (_, text) = first_request(r, MockProvider::new("m", vec![answer()])).await;
    let l = line(&text);
    assert!(l.contains("terminal: yes"), "{text}");
    assert!(l.contains("local pages: yes"), "{l}");
    assert!(l.contains("Computer: off"), "{l}");
    assert!(l.len() < 200, "{l}");
}

#[tokio::test]
async fn no_process_start_says_no_terminal() {
    let (_, text) = first_request(registry(&[]), MockProvider::new("m", vec![answer()])).await;
    let l = line(&text);
    assert!(l.contains("terminal: no"), "{text}");
    assert!(l.contains("local pages: no"), "{l}");
}

#[tokio::test]
async fn a_preloaded_terminal_withheld_from_the_first_request_is_still_stated() {
    let r = registry(&[(
        "process.start",
        "start a terminal process in the project folder, a dev server",
    )]);
    let m = MockProvider::new("m", vec![answer()]).with_whole_system_cache();
    let (first, text) = first_request(r, m).await;
    assert!(
        !first.tools.iter().any(|t| t == "process.start"),
        "withheld on the first request"
    );
    assert!(line(&text).contains("terminal: yes"), "{text}");
}

#[tokio::test]
async fn the_system_prompt_does_not_carry_the_line() {
    let r = registry(&[("process.start", "start a terminal process")]);
    let (first, _) = first_request(r, MockProvider::new("m", vec![answer()])).await;
    assert!(!first.system.contains("THIS SESSION"));
}

struct Terminal;

#[async_trait::async_trait]
impl Adapter for Terminal {
    fn name(&self) -> &str {
        "process"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            "process.start",
            "start a terminal process",
            RiskTier::Mutate,
        )]
    }
    async fn run(&self, _a: &str, _i: Value, _c: &RunCtx<'_>) -> ark_tools::Result<ActionOutput> {
        Ok(ActionOutput::json(serde_json::json!({})))
    }
}

#[tokio::test]
async fn plan_mode_does_not_claim_a_terminal_it_withholds() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open_with(dir.path(), registry(&[]).with(Arc::new(Terminal))).unwrap();
    let project = engine.create_project("p", "").unwrap();
    let model = Arc::new(MockProvider::new("m", vec![answer()]));
    let p: Arc<dyn Provider> = model.clone();
    let profile = AgentProfile::builder().core_actions(["other.one"]).build();
    Harness::new(ModelRouter::single(p))
        .execution_mode(ark_core::ExecutionMode::Plan)
        .max_steps(4)
        .ask(&engine, &profile, &project.id, "start the server", |_| {})
        .await
        .unwrap();
    let text = model.recorded()[0].messages[0].content.clone();
    assert!(line(&text).contains("terminal: no"), "{text}");
}
