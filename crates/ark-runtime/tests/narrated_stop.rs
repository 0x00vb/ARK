//! A brief task that narrates its steps and ends on a one-line note has no result. The harness
//! sends such a reply back once; whatever the model answers after that is the answer. A chat
//! turn, a one-line answer after silent calls and a one-line answer with no calls are left alone.

use std::sync::Arc;

use ark_core::{ActionDescriptor, RiskTier};
use ark_model::{Completion, MockProvider, Provider, ToolCall, Usage};
use ark_runtime::{reads_as_note, AgentProfile, Engine, Harness, ModelRouter};
use ark_tools::{ActionOutput, Adapter, ProducedArtifact, Registry, RunCtx};
use serde_json::{json, Value};

struct Look;

#[async_trait::async_trait]
impl Adapter for Look {
    fn name(&self) -> &str {
        "look"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new("look.read", "read a file", RiskTier::Read),
            ActionDescriptor::new("look.write", "write a file", RiskTier::Mutate),
        ]
    }
    async fn run(
        &self,
        action: &str,
        input: Value,
        _c: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        let path = input["path"].as_str().unwrap_or("").to_string();
        let mut out =
            ActionOutput::json(json!({ "path": path, "text": format!("the text of {path}") }));
        if action == "look.write" {
            let mut file = ProducedArtifact::text(path.clone(), "written");
            file.path = Some(path);
            out.produced.push(file);
        }
        Ok(out)
    }
}

fn read(path: &str) -> ToolCall {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    ToolCall {
        id: format!("c{}", N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)),
        name: "look.read".into(),
        arguments: json!({ "path": path }),
    }
}

fn write(path: &str) -> ToolCall {
    ToolCall {
        id: format!("w-{path}"),
        name: "look.write".into(),
        arguments: json!({ "path": path }),
    }
}

fn reply(text: &str, calls: Vec<ToolCall>) -> Completion {
    Completion {
        content: text.into(),
        tool_calls: calls,
        usage: Usage::measured(100, 20),
    }
}

const NOTE: &str = "Checking the project files and verifying the seed leads.";
const NOTE_AGAIN: &str = "Reading the price lines now.";
const RESULT: &str =
    "Lead list\n\n- Acme: 3 seats, warm\n- Birch: 10 seats, cold\n- Cora: 1 seat, warm";
const NUDGE: &str = "Your reply says what you will do next";

struct Run {
    answer: String,
    requests: Vec<ark_model::mock::Recorded>,
}

async fn run(brief: bool, script: Vec<Completion>) -> Run {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open_with(dir.path(), Registry::new().with(Arc::new(Look))).unwrap();
    let project = engine.create_project("leads", "").unwrap();
    let model =
        Arc::new(MockProvider::new("m", script).with_fallback(|_| reply("FALLBACK", vec![])));
    let provider: Arc<dyn Provider> = model.clone();
    let profile = AgentProfile::builder()
        .core_actions(["look.read", "look.write"])
        .build();
    let out = Harness::new(ModelRouter::single(provider))
        .planner(false)
        .critic(false)
        .brief_task(brief)
        .allow_mutate(true)
        .ask(
            &engine,
            &profile,
            &project.id,
            "qualify the seed leads",
            |_| {},
        )
        .await
        .unwrap();
    Run {
        answer: out.answer,
        requests: model.recorded(),
    }
}

fn nudged(r: &Run) -> bool {
    r.requests
        .iter()
        .any(|req| req.messages.iter().any(|m| m.content.contains(NUDGE)))
}

#[test]
fn a_note_is_one_short_line() {
    assert!(reads_as_note(NOTE));
    assert!(reads_as_note("\n  Done.  \n"));
    assert!(!reads_as_note(""));
    assert!(!reads_as_note(RESULT));
    assert!(!reads_as_note(&"word ".repeat(60)));
}

#[tokio::test]
async fn narrated_calls_then_a_note_get_one_more_try_in_a_brief_task() {
    let r = run(
        true,
        vec![
            reply("Reading the seed file.", vec![read("seed.md")]),
            reply(NOTE, vec![]),
            reply(RESULT, vec![]),
        ],
    )
    .await;
    assert_eq!(r.requests.len(), 3, "the note was sent back");
    assert!(nudged(&r), "the third request carries the nudge");
    assert_eq!(r.answer, RESULT);
}

#[tokio::test]
async fn the_same_note_in_a_chat_turn_is_the_answer() {
    let r = run(
        false,
        vec![
            reply("Reading the seed file.", vec![read("seed.md")]),
            reply(NOTE, vec![]),
        ],
    )
    .await;
    assert_eq!(r.requests.len(), 2);
    assert!(!nudged(&r));
    assert_eq!(r.answer, NOTE);
}

#[tokio::test]
async fn a_one_line_answer_with_no_calls_costs_nothing_extra() {
    let r = run(true, vec![reply("There are 3 warm leads.", vec![])]).await;
    assert_eq!(r.requests.len(), 1);
    assert_eq!(r.answer, "There are 3 warm leads.");
}

#[tokio::test]
async fn a_one_line_answer_after_silent_calls_is_the_answer() {
    let r = run(
        true,
        vec![
            reply("", vec![read("seed.md")]),
            reply("Renamed 3 leads.", vec![]),
        ],
    )
    .await;
    assert_eq!(r.requests.len(), 2);
    assert!(!nudged(&r));
    assert_eq!(r.answer, "Renamed 3 leads.");
}

#[tokio::test]
async fn a_note_again_after_the_nudge_is_the_answer() {
    let r = run(
        true,
        vec![
            reply("Reading the seed file.", vec![read("seed.md")]),
            reply(NOTE, vec![]),
            reply(NOTE_AGAIN, vec![]),
        ],
    )
    .await;
    assert_eq!(r.requests.len(), 3, "one nudge, not more");
    assert_eq!(r.answer, NOTE_AGAIN);
}

#[tokio::test]
async fn a_short_result_after_the_nudge_is_the_answer() {
    let r = run(
        true,
        vec![
            reply("Reading the seed file.", vec![read("seed.md")]),
            reply(NOTE, vec![]),
            reply("Renamed 3 leads.", vec![]),
        ],
    )
    .await;
    assert_eq!(r.requests.len(), 3);
    assert!(nudged(&r));
    assert_eq!(r.answer, "Renamed 3 leads.");
}

#[tokio::test]
async fn a_write_after_the_nudge_then_a_short_result_is_the_answer() {
    let r = run(
        true,
        vec![
            reply("Reading the seed file.", vec![read("seed.md")]),
            reply(NOTE, vec![]),
            reply("", vec![write("out/brand.md")]),
            reply("", vec![read("out/brand.md")]),
            reply("Wrote out/brand.md.", vec![]),
        ],
    )
    .await;
    assert_eq!(r.requests.len(), 5);
    assert!(nudged(&r));
    assert_eq!(r.answer, "Wrote out/brand.md.");
}
