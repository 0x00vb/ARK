//! An answer after a change nobody looked at again gets one round asking for a check, never
//! two in a turn (ADR 0108).

use std::sync::Arc;

use ark_core::{ActionDescriptor, RiskTier};
use ark_model::{Completion, MockProvider, Provider, ToolCall, Usage};
use ark_runtime::{AgentProfile, Engine, Harness, HarnessEvent, ModelRouter};
use ark_tools::{ActionOutput, Adapter, Registry, RunCtx};
use serde_json::{json, Value};

struct Notes;
struct Lone;
struct Pad;

#[async_trait::async_trait]
impl Adapter for Pad {
    fn name(&self) -> &str {
        "pad"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new("pad.write", "write a page", RiskTier::Mutate),
            ActionDescriptor::new("pad.read", "read a page", RiskTier::Read),
        ]
    }
    async fn run(&self, _a: &str, _i: Value, _c: &RunCtx<'_>) -> ark_tools::Result<ActionOutput> {
        Ok(ActionOutput::json(json!({ "page": "a" })))
    }
}

#[async_trait::async_trait]
impl Adapter for Lone {
    fn name(&self) -> &str {
        "lone"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            "lone.set",
            "set it",
            RiskTier::Mutate,
        )]
    }
    async fn run(&self, _a: &str, _i: Value, _c: &RunCtx<'_>) -> ark_tools::Result<ActionOutput> {
        Ok(ActionOutput::json(json!({ "set": true })))
    }
}

#[async_trait::async_trait]
impl Adapter for Notes {
    fn name(&self) -> &str {
        "notes"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new("notes.write", "write a note", RiskTier::Mutate),
            ActionDescriptor::new("notes.look", "look at a note", RiskTier::Observe),
            ActionDescriptor::new("notes.peek", "look at a private note", RiskTier::Mutate)
                .volatile(),
        ]
    }
    async fn run(&self, _a: &str, i: Value, _c: &RunCtx<'_>) -> ark_tools::Result<ActionOutput> {
        if i["name"] == "bad" {
            return Err(ark_tools::AdapterError::Tool("no such note".into()));
        }
        Ok(ActionOutput::json(json!({ "note": i["name"] })))
    }
}

fn step(name: &str, id: &str) -> Completion {
    step_with(name, id, "a", "")
}

fn step_with(name: &str, id: &str, note: &str, said: &str) -> Completion {
    Completion {
        content: said.into(),
        tool_calls: vec![ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: json!({ "name": note }),
        }],
        usage: Usage::measured(10, 5),
    }
}

fn both(first: &str, second: &str) -> Completion {
    let call = |id: &str, name: &str| ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: json!({ "name": "a" }),
    };
    Completion {
        content: String::new(),
        tool_calls: vec![call("p1", first), call("p2", second)],
        usage: Usage::measured(10, 5),
    }
}

fn say(text: &str) -> Completion {
    Completion {
        content: text.into(),
        tool_calls: vec![],
        usage: Usage::measured(10, 5),
    }
}

struct Run {
    model_calls: usize,
    answer: String,
    checks: Vec<(bool, String)>,
    nudges: usize,
    shown: String,
    withdrawn: Vec<String>,
}

async fn run(script: Vec<Completion>) -> Run {
    run_with(script, 8).await
}

async fn run_with(script: Vec<Completion>, max_steps: usize) -> Run {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open_with(
        dir.path(),
        Registry::new()
            .with(Arc::new(Notes))
            .with(Arc::new(Lone))
            .with(Arc::new(Pad)),
    )
    .unwrap();
    let project = engine.create_project("n", "").unwrap();
    let model = Arc::new(MockProvider::new("m", script).with_fallback(|_| say("FALLBACK")));
    let provider: Arc<dyn Provider> = model.clone();
    let profile = AgentProfile::builder()
        .core_actions([
            "notes.write",
            "notes.look",
            "lone.set",
            "pad.write",
            "pad.read",
        ])
        .build();
    let mut checks = Vec::new();
    let mut shown = String::new();
    let mut withdrawn = Vec::new();
    let out = Harness::new(ModelRouter::single(provider))
        .planner(false)
        .critic(false)
        .allow_mutate(true)
        .max_steps(max_steps)
        .ask(
            &engine,
            &profile,
            &project.id,
            "write note a",
            |e| match e {
                HarnessEvent::SelfCheck { passed, detail } => checks.push((passed, detail)),
                HarnessEvent::TextDelta { text } => shown.push_str(&text),
                HarnessEvent::TextWithdrawn { text } => withdrawn.push(text),
                _ => {}
            },
        )
        .await
        .unwrap();
    let nudges = model.recorded()[model.recorded().len() - 1]
        .messages
        .iter()
        .filter(|m| m.content.contains("Check the change"))
        .count();
    Run {
        model_calls: model.recorded().len(),
        answer: out.answer,
        checks,
        nudges,
        shown,
        withdrawn,
    }
}

#[tokio::test]
async fn an_unchecked_change_gets_one_nudge_and_a_second_answer() {
    let r = run(vec![
        step("notes.write", "w1"),
        say("Saved note a."),
        say("Note a is saved; I did not check it."),
    ])
    .await;
    assert_eq!(r.model_calls, 3);
    assert_eq!(r.answer, "Note a is saved; I did not check it.");
    assert_eq!(r.nudges, 1);
    let (passed, detail) = r.checks.last().unwrap();
    assert!(!passed, "{detail}");
    assert!(detail.contains("changed 1; checked after: no"), "{detail}");
}

#[tokio::test]
async fn the_nudge_is_never_sent_twice_in_a_turn() {
    let r = run(vec![
        step("notes.write", "w1"),
        say("one"),
        step("notes.write", "w2"),
        say("two"),
    ])
    .await;
    assert_eq!(r.model_calls, 4);
    assert_eq!(r.nudges, 1);
    assert_eq!(r.answer, "two");
}

#[tokio::test]
async fn a_read_after_the_change_needs_no_nudge() {
    let r = run(vec![
        step("notes.write", "w1"),
        step("notes.look", "r1"),
        say("Saved and read back."),
    ])
    .await;
    assert_eq!(r.model_calls, 3);
    assert_eq!(r.nudges, 0);
    let (passed, detail) = r.checks.last().unwrap();
    assert!(*passed && detail.contains("checked after: yes"), "{detail}");
}

#[tokio::test]
async fn a_turn_without_changes_is_not_nudged() {
    let r = run(vec![step("notes.look", "r1"), say("It says a.")]).await;
    assert_eq!(r.model_calls, 2);
    assert_eq!(r.nudges, 0);
}

#[tokio::test]
async fn a_change_with_nothing_to_check_it_with_is_not_nudged() {
    let r = run(vec![step("lone.set", "s1"), say("It is set.")]).await;
    assert_eq!(r.model_calls, 2);
    assert_eq!(r.nudges, 0);
    assert!(r.checks.last().unwrap().0, "{:?}", r.checks);
}

#[tokio::test]
async fn a_read_that_failed_does_not_check_the_change() {
    let r = run(vec![
        step("notes.write", "w1"),
        step_with("notes.look", "r1", "bad", ""),
        say("Saved."),
        say("Saved; I could not read it back."),
    ])
    .await;
    assert_eq!(r.nudges, 1);
    let (passed, detail) = r.checks.last().unwrap();
    assert!(!passed && detail.contains("checked after: no"), "{detail}");
}

#[tokio::test]
async fn a_change_that_failed_needs_no_check() {
    let r = run(vec![
        step_with("notes.write", "w1", "bad", ""),
        say("The note could not be saved."),
    ])
    .await;
    assert_eq!(r.nudges, 0);
    let (passed, detail) = r.checks.last().unwrap();
    assert!(*passed && !detail.contains("checked after: no"), "{detail}");
}

#[tokio::test]
async fn a_turn_that_ran_out_of_steps_after_an_unchecked_change_fails_its_check() {
    let r = run_with(vec![step("notes.write", "w1")], 1).await;
    let (passed, detail) = r.checks.last().unwrap();
    assert!(!passed, "{detail}");
    assert!(detail.contains("changed 1; checked after: no"), "{detail}");
}

#[tokio::test]
async fn the_nudged_answer_is_never_shown_or_withdrawn() {
    let r = run(vec![
        step("notes.write", "w1"),
        say("Saved note a."),
        say("Note a is saved; unchecked."),
    ])
    .await;
    assert!(r.withdrawn.is_empty(), "{:?}", r.withdrawn);
    assert_eq!(r.shown, "Note a is saved; unchecked.");
}

#[tokio::test]
async fn text_beside_a_call_after_an_unchecked_change_is_still_shown() {
    let r = run(vec![
        step("notes.write", "w1"),
        step_with("notes.look", "r1", "a", "Reading it back."),
        say("Saved and read."),
    ])
    .await;
    assert_eq!(r.shown, "Reading it back.Saved and read.");
    assert!(r.withdrawn.is_empty());
}

#[tokio::test]
async fn a_change_in_a_namespace_with_only_a_read_is_not_nudged() {
    let r = run(vec![step("pad.write", "p1"), say("The page is written.")]).await;
    assert_eq!(r.model_calls, 2);
    assert_eq!(r.nudges, 0);
    let (passed, detail) = r.checks.last().unwrap();
    assert!(*passed, "{detail}");
}

#[tokio::test]
async fn a_volatile_call_that_asks_for_consent_looks_rather_than_changes() {
    let r = run(vec![step("notes.peek", "k1"), say("The note says a.")]).await;
    assert_eq!(r.nudges, 0);
    let r = run(vec![
        step("notes.write", "w1"),
        step("notes.peek", "k1"),
        say("Saved and seen."),
    ])
    .await;
    assert_eq!(r.nudges, 0);
    let (passed, detail) = r.checks.last().unwrap();
    assert!(*passed && detail.contains("checked after: yes"), "{detail}");
}

#[tokio::test]
async fn a_look_before_a_change_in_the_same_step_does_not_check_it() {
    let r = run(vec![
        both("notes.look", "notes.write"),
        say("Saved."),
        say("Saved; unchecked."),
    ])
    .await;
    assert_eq!(r.nudges, 1);
}

#[tokio::test]
async fn a_look_after_a_change_in_the_same_step_checks_it() {
    let r = run(vec![
        both("notes.write", "notes.look"),
        say("Saved and seen."),
    ])
    .await;
    assert_eq!(r.nudges, 0);
}
