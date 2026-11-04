use std::sync::Arc;

use ark_interact::{App, ChannelApprover, Interaction, Start, TurnControl};
use ark_sdk::prelude::ark_model::{Completion, MockProvider, Usage};
use ark_sdk::prelude::*;

fn open(dir: &std::path::Path, model: Arc<MockProvider>) -> Interaction {
    let ark = Ark::builder()
        .store_root(dir)
        .profile(
            AgentProfile::builder()
                .instructions("Answer the person's task.")
                .build(),
        )
        .provider(model)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let (approver, _rx) = ChannelApprover::channel();
    App::new("Input preservation")
        .ark(ark)
        .open(&Start::default(), Arc::new(approver))
        .unwrap()
        .interaction
}

#[tokio::test]
async fn normalized_model_task_preserves_raw_stored_turn_trace_history_and_answer() {
    let dir = tempfile::tempdir().unwrap();
    let raw = "Reply with exactly: Ready. Do not use tools.";
    let supplied = "Ready. Do not use tools.";
    let model = Arc::new(MockProvider::new(
        "mock",
        vec![Completion {
            content: supplied.into(),
            tool_calls: vec![],
            usage: Usage::measured(10, 5),
        }],
    ));
    let mut interaction = open(dir.path(), model.clone());
    let outcome = interaction
        .run_turn(raw, &TurnControl::new(), |_, _| {})
        .await
        .result
        .unwrap();
    assert_eq!(outcome.answer, supplied);
    assert_eq!(outcome.state.goal, raw);
    assert_eq!(interaction.trace(None).unwrap().input, raw);
    let session = interaction.session().id().as_str().to_string();
    drop(interaction);
    let mut reopened = open(dir.path(), model.clone());
    assert!(reopened.resume(&session).unwrap());
    let turns = reopened.transcript(None, 10).unwrap();
    assert_eq!(turns[0].input, raw);
    assert_eq!(turns[0].answer, supplied);
    assert_eq!(reopened.trace(Some(0)).unwrap().input, raw);
    let recorded = model.recorded();
    assert_eq!(recorded.len(), 1);
    let messages = serde_json::to_string(&recorded[0].messages).unwrap();
    assert!(messages.contains("Reply with exactly: Ready."));
    assert!(messages.contains("GOAL: Reply with exactly: Ready."));
    assert!(!messages.contains(raw));
    assert!(!messages.contains("Do not use tools."));
}
