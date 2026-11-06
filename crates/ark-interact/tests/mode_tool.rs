//! The chat changes its own execution mode on request (ADR 0104): the person approves, the mode
//! applies when the turn has ended, never during it.

use std::sync::{Arc, Mutex};

use ark_interact::{App, ChannelApprover, Interaction, Start, TurnControl};
use ark_sdk::prelude::ark_core::ExecutionMode;
use ark_sdk::prelude::ark_model::{Completion, MockProvider, ToolCall, Usage};
use ark_sdk::prelude::*;

/// A write that Auto may run without asking.
struct Notes;
#[async_trait::async_trait]
impl Adapter for Notes {
    fn name(&self) -> &str {
        "notes"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        let mut write = ActionDescriptor::new("notes.write", "write a note", RiskTier::Mutate);
        write.auto_eligible = true;
        vec![write]
    }
    async fn run(
        &self,
        _a: &str,
        _i: serde_json::Value,
        _c: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        Ok(ActionOutput::json(serde_json::json!({ "saved": true })))
    }
}

fn call(name: &str, args: serde_json::Value) -> Completion {
    Completion {
        content: String::new(),
        tool_calls: vec![ToolCall {
            id: format!("c-{name}"),
            name: name.into(),
            arguments: args,
        }],
        usage: Usage::measured(40, 4),
    }
}

fn answer(text: &str) -> Completion {
    Completion {
        content: text.into(),
        tool_calls: vec![],
        usage: Usage::measured(40, 4),
    }
}

fn set(mode: &str) -> Completion {
    call("mode.set", serde_json::json!({ "mode": mode }))
}

type Asked = Arc<Mutex<Vec<String>>>;

/// A chat whose person answers every question with `decision` and notes what was asked.
fn open(
    dir: &std::path::Path,
    script: Vec<Completion>,
    decision: ApprovalDecision,
) -> (Interaction, Arc<MockProvider>, Asked, Arc<Engine>) {
    let model = Arc::new(MockProvider::new("m", script));
    let ark = Ark::builder()
        .store_root(dir)
        .tool(Arc::new(Notes))
        .provider(model.clone())
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let engine = ark.shared_engine();
    let (approver, mut rx) = ChannelApprover::channel();
    let asked: Asked = Arc::default();
    let seen = asked.clone();
    tokio::spawn(async move {
        while let Some(p) = rx.recv().await {
            seen.lock().unwrap().push(p.request.action.clone());
            p.answer(decision);
        }
    });
    let ix = App::new("Test")
        .ark(ark)
        .open(&Start::default(), Arc::new(approver))
        .unwrap()
        .interaction;
    (ix, model, asked, engine)
}

async fn say(ix: &mut Interaction, text: &str) {
    let end = ix.run_turn(text, &TurnControl::new(), |_, _| {}).await;
    end.result.unwrap();
}

fn mode_of(ix: &Interaction) -> (ExecutionMode, u64) {
    let s = ix.summary();
    (s.execution_mode, s.execution_mode_revision)
}

#[tokio::test]
async fn a_mode_asked_for_applies_after_the_turn_not_during_it() {
    let dir = tempfile::tempdir().unwrap();
    let (mut ix, model, asked, _) = open(
        dir.path(),
        vec![
            set("auto"),
            call("notes.write", serde_json::json!({})),
            answer("Done."),
            call("notes.write", serde_json::json!({})),
            answer("Done again."),
        ],
        ApprovalDecision::AllowOnce,
    );
    assert_eq!(mode_of(&ix), (ExecutionMode::Manual, 0));

    say(&mut ix, "go auto, then write a note").await;
    // The turn kept Manual: the write asked even after the mode was approved.
    assert_eq!(*asked.lock().unwrap(), ["mode.set", "notes.write"]);
    assert_eq!(mode_of(&ix), (ExecutionMode::Auto, 1));
    let said = model.recorded()[1]
        .messages
        .iter()
        .find(|m| m.tool_call_id.is_some())
        .map(|m| m.content.clone())
        .unwrap();
    assert!(
        said.contains("next message") && said.contains("keeps manual"),
        "{said}"
    );

    say(&mut ix, "write another").await;
    assert_eq!(
        asked.lock().unwrap().len(),
        2,
        "the next turn ran in Auto without asking"
    );
}

#[tokio::test]
async fn a_denied_change_leaves_the_mode_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (mut ix, _, asked, _) = open(
        dir.path(),
        vec![set("auto"), answer("Staying as is.")],
        ApprovalDecision::Deny,
    );
    say(&mut ix, "go auto").await;
    assert_eq!(*asked.lock().unwrap(), ["mode.set"]);
    assert_eq!(mode_of(&ix), (ExecutionMode::Manual, 0));
}

#[tokio::test]
async fn every_change_asks_even_after_always() {
    let dir = tempfile::tempdir().unwrap();
    let (mut ix, _, asked, _) = open(
        dir.path(),
        vec![set("auto"), answer("ok"), set("plan"), answer("ok")],
        ApprovalDecision::AllowAlways,
    );
    say(&mut ix, "go auto").await;
    say(&mut ix, "now plan").await;
    assert_eq!(*asked.lock().unwrap(), ["mode.set", "mode.set"]);
    assert_eq!(mode_of(&ix), (ExecutionMode::Plan, 2));
}

#[tokio::test]
async fn plan_still_offers_the_tool_and_can_leave_plan() {
    let dir = tempfile::tempdir().unwrap();
    let (mut ix, model, _, _) = open(
        dir.path(),
        vec![set("manual"), answer("Back to asking first.")],
        ApprovalDecision::AllowOnce,
    );
    ix.set_execution_mode(0, ExecutionMode::Plan).unwrap();
    say(&mut ix, "ask me before changes").await;

    let tools: Vec<String> = model.recorded()[0]
        .tools
        .iter()
        .map(|t| t.name.clone())
        .collect();
    assert!(tools.contains(&"mode.set".to_string()), "{tools:?}");
    assert!(!tools.contains(&"notes.write".to_string()), "{tools:?}");
    assert_eq!(mode_of(&ix), (ExecutionMode::Manual, 2));
}

#[tokio::test]
async fn a_mode_another_client_set_meanwhile_wins() {
    let dir = tempfile::tempdir().unwrap();
    let (mut ix, _, _, engine) = open(
        dir.path(),
        vec![set("auto"), answer("ok")],
        ApprovalDecision::AllowOnce,
    );
    let id = ix.summary().session.id;
    // Another client changes the stored chat while the turn runs.
    let mut other = ark_sdk::prelude::ark_runtime::Session::open(&engine, &id)
        .unwrap()
        .unwrap();
    other
        .set_execution_mode(&engine, 0, ExecutionMode::Plan)
        .unwrap();

    say(&mut ix, "go auto").await;
    assert_eq!(mode_of(&ix), (ExecutionMode::Plan, 1));
}

#[tokio::test]
async fn asking_for_the_mode_it_has_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (mut ix, _, _, _) = open(
        dir.path(),
        vec![set("manual"), answer("It already was.")],
        ApprovalDecision::AllowOnce,
    );
    say(&mut ix, "ask me before changes").await;
    assert_eq!(mode_of(&ix), (ExecutionMode::Manual, 0));
}
