//! A chat's working state is its own: another chat of the same project neither reads it nor
//! overwrites it (ADR 0108).

use std::sync::Arc;

use ark_core::{Claim, ClaimType, ResearchState};
use ark_model::{Completion, MockProvider, Provider, Usage};
use ark_runtime::{AgentProfile, Engine, Harness, ModelRouter};
use ark_tools::Registry;

fn answer() -> Completion {
    Completion {
        content: "hello".into(),
        tool_calls: vec![],
        usage: Usage::measured(10, 5),
    }
}

async fn ask(engine: &Engine, project: &ark_core::ProjectId, chat: &str) -> String {
    let model = Arc::new(MockProvider::new("m", vec![answer()]));
    let p: Arc<dyn Provider> = model.clone();
    Harness::new(ModelRouter::single(p))
        .planner(false)
        .critic(false)
        .chat(chat)
        .ask(
            engine,
            &AgentProfile::builder().build(),
            project,
            "hello",
            |_| {},
        )
        .await
        .unwrap();
    model.recorded()[0]
        .messages
        .iter()
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn working_state_of_one_chat_is_absent_from_another() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open_with(dir.path(), Registry::new()).unwrap();
    let project = engine.create_project("p", "").unwrap();
    let mut state = ResearchState::new("write a rust program");
    state
        .findings
        .push(Claim::new("MINIMAL_RUST_PROGRAM", ClaimType::Fact));
    engine
        .store
        .put_research_state(&project.id, "chat-1", &state)
        .unwrap();

    let two = ask(&engine, &project.id, "chat-2").await;
    assert!(!two.contains("MINIMAL_RUST_PROGRAM"), "{two}");

    let one = engine
        .store
        .get_research_state(&project.id, "chat-1")
        .unwrap()
        .unwrap();
    assert_eq!(
        one.goal, "write a rust program",
        "chat 2 did not overwrite it"
    );
    assert_eq!(one.findings.len(), 1);

    let again = ask(&engine, &project.id, "chat-1").await;
    assert!(again.contains("MINIMAL_RUST_PROGRAM"), "{again}");
}
