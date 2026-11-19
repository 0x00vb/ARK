//! How often the person is asked while an agent uses the computer, through the real harness and
//! approval gate: an Auto chat asks nothing (the person's own words asked for the work), and a
//! chat that asks does so once, not at every look, click and keystroke.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ark_computer::fake::FakeScreen;
use ark_computer::*;
use ark_core::ExecutionMode;
use ark_model::{Completion, MockProvider, Provider, ToolCall, Usage};
use ark_runtime::{
    AgentProfile, ApprovalDecision, ApprovalGate, ApprovalRequest, Approver, Engine, Harness,
    ModelRouter,
};
use ark_tools::Registry;
use serde_json::json;

struct Answers(ApprovalDecision, Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Approver for Answers {
    async fn decide(&self, _r: &ApprovalRequest) -> ApprovalDecision {
        self.1.fetch_add(1, Ordering::SeqCst);
        self.0
    }
}

fn step(id: &str, name: &str, input: serde_json::Value) -> Completion {
    Completion {
        content: String::new(),
        tool_calls: vec![ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: input,
        }],
        usage: Usage::measured(100, 5),
    }
}

fn answer() -> Completion {
    Completion {
        content: "Saved the note titled Plan in Notes.".into(),
        tool_calls: vec![],
        usage: Usage::measured(120, 10),
    }
}

/// Look, type, click in Notes; returns (questions asked, what the screen did).
async fn save_a_note(mode: ExecutionMode, reply: ApprovalDecision) -> (usize, Vec<String>) {
    let screen = FakeScreen::new().app("Notes", &[("text field", "Title"), ("button", "Save")]);
    let svc = ComputerService::with_screen(Arc::new(screen.clone()))
        .timing(Duration::from_millis(50), Duration::from_millis(50));
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open_with(
        dir.path(),
        Registry::new().with(Arc::new(svc.for_owner("p/writer"))),
    )
    .unwrap();
    let project = engine.create_project("t", "").unwrap();
    let profile = AgentProfile::builder()
        .core_actions([LOOK, TYPE, CLICK])
        .build();
    let mut script = vec![
        step("c1", LOOK, json!({ "app": "Notes" })),
        step(
            "c2",
            TYPE,
            json!({ "app": "Notes", "ref": 1, "name": "Title", "text": "Plan" }),
        ),
        step(
            "c3",
            CLICK,
            json!({ "app": "Notes", "ref": 2, "name": "Save" }),
        ),
    ];
    script.extend(std::iter::repeat_with(answer).take(4));
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new("m", script));
    let asked = Arc::new(AtomicUsize::new(0));
    Harness::new(ModelRouter::single(provider))
        .planner(false)
        .critic(false)
        .execution_mode(mode)
        .approvals(Some(Arc::new(ApprovalGate::new(Arc::new(Answers(
            reply,
            asked.clone(),
        ))))))
        .ask(
            &engine,
            &profile,
            &project.id,
            "Use Notes on my computer: save a note titled Plan",
            |_| {},
        )
        .await
        .unwrap();
    let log = screen.log.lock().unwrap().clone();
    (asked.load(Ordering::SeqCst), log)
}

#[tokio::test]
async fn an_auto_chat_uses_the_computer_without_asking() {
    let (asked, log) = save_a_note(ExecutionMode::Auto, ApprovalDecision::Deny).await;
    assert_eq!(asked, 0, "the person's message asked for the work");
    assert!(log.contains(&"press Notes/Save".to_string()), "{log:?}");
}

#[tokio::test]
async fn a_chat_that_asks_asks_once_for_the_computer() {
    // The computer's question offers one yes, for the rest of the chat
    // (`ark_interact::approval_choices`).
    let (asked, log) = save_a_note(ExecutionMode::Manual, ApprovalDecision::AllowSession).await;
    assert_eq!(
        asked, 1,
        "one question, then the look, the typing and the click"
    );
    assert!(log.contains(&"press Notes/Save".to_string()), "{log:?}");
}

#[tokio::test]
async fn a_no_stops_the_computer() {
    let (asked, log) = save_a_note(ExecutionMode::Manual, ApprovalDecision::Deny).await;
    assert!(asked >= 1);
    assert!(log.is_empty(), "nothing touched the screen: {log:?}");
}
