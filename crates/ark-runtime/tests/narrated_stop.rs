//! A turn that narrates its steps and ends on a one-line note has no result. The harness sends
//! such a reply back once, in a chat turn as in a brief task (ADR 0125); whatever the model
//! answers after that is the answer. A one-line answer after silent calls, with no calls, ending
//! in a question, after a refused call or after a call that asks for a short closing reply is
//! left alone.

use std::sync::Arc;

use ark_core::{ActionDescriptor, RiskTier};
use ark_model::{Completion, MockProvider, Provider, ToolCall, Usage};
use ark_runtime::{reads_as_note, AgentProfile, Engine, Harness, HarnessEvent, ModelRouter};
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
            ActionDescriptor::new("look.later", "hand work off", RiskTier::Observe)
                .ends_with_short_reply(),
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

fn later() -> ToolCall {
    ToolCall {
        id: "later".into(),
        name: "look.later".into(),
        arguments: json!({}),
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
const NUDGE: &str = "Your last reply was one short line";
const ANNOUNCED: &str = "Your reply says what you will do next";

struct Run {
    answer: String,
    requests: Vec<ark_model::mock::Recorded>,
    shown: String,
    withdrawn: Vec<String>,
}

async fn run(script: Vec<Completion>) -> Run {
    run_as(true, script).await
}

async fn run_as(allow_mutate: bool, script: Vec<Completion>) -> Run {
    run_full(allow_mutate, "qualify the seed leads", script).await
}

/// The owner's message of 2026-11-17 that got one note and no work (t-42106d23).
const LONG_ASK: &str = "Make sure the product does not have any AI slop, like em dashes, and that the Spanish and English files are consistent";

async fn run_full(allow_mutate: bool, question: &str, script: Vec<Completion>) -> Run {
    run_first_note(allow_mutate, true, question, script).await
}

async fn run_first_note(
    allow_mutate: bool,
    first_note: bool,
    question: &str,
    script: Vec<Completion>,
) -> Run {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open_with(dir.path(), Registry::new().with(Arc::new(Look))).unwrap();
    let project = engine.create_project("leads", "").unwrap();
    let model =
        Arc::new(MockProvider::new("m", script).with_fallback(|_| reply("FALLBACK", vec![])));
    let provider: Arc<dyn Provider> = model.clone();
    let profile = AgentProfile::builder()
        .core_actions(["look.read", "look.write", "look.later"])
        .build();
    let (mut shown, mut withdrawn) = (String::new(), Vec::new());
    let out = Harness::new(ModelRouter::single(provider))
        .planner(false)
        .critic(false)
        .allow_mutate(allow_mutate)
        .first_note(first_note)
        .ask(&engine, &profile, &project.id, question, |e| match e {
            HarnessEvent::TextDelta { text } => shown.push_str(&text),
            HarnessEvent::TextWithdrawn { text } => {
                shown.truncate(shown.len() - text.len());
                withdrawn.push(text)
            }
            _ => {}
        })
        .await
        .unwrap();
    Run {
        answer: out.answer,
        requests: model.recorded(),
        shown,
        withdrawn,
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
async fn narrated_calls_then_a_note_get_one_more_try_in_a_chat_turn() {
    let r = run(vec![
        reply("Reading the seed file.", vec![read("seed.md")]),
        reply(NOTE, vec![]),
        reply("", vec![read("other.md")]),
        reply(RESULT, vec![]),
    ])
    .await;
    assert_eq!(r.requests.len(), 4, "the note was sent back once");
    assert!(nudged(&r));
    assert!(!r
        .requests
        .iter()
        .any(|q| q.messages.iter().any(|m| m.content.contains(ANNOUNCED))));
    assert_eq!(r.answer, RESULT);
}

#[tokio::test]
async fn a_question_is_not_a_note() {
    assert!(!reads_as_note("Shall I save the summary now?"));
    let r = run(vec![
        reply("Reading the seed file.", vec![read("seed.md")]),
        reply("Shall I save the summary now?", vec![]),
    ])
    .await;
    assert_eq!(r.requests.len(), 2);
    assert_eq!(r.answer, "Shall I save the summary now?");
}

#[tokio::test]
async fn a_refused_call_this_turn_means_the_note_is_the_answer() {
    let r = run_as(
        false,
        vec![
            reply("Saving the file.", vec![write("out.md")]),
            reply(NOTE, vec![]),
        ],
    )
    .await;
    assert_eq!(r.requests.len(), 2);
    assert!(!nudged(&r));
    assert_eq!(r.answer, NOTE);
}

#[tokio::test]
async fn a_call_that_asks_for_a_short_closing_reply_is_not_followed_by_a_nudge() {
    let r = run(vec![
        reply("Handing it off.", vec![later()]),
        reply("I will look into it.", vec![]),
    ])
    .await;
    assert_eq!(r.requests.len(), 2);
    assert!(!nudged(&r));
    assert_eq!(r.answer, "I will look into it.");
}

#[allow(deprecated)]
#[tokio::test]
async fn the_brief_task_flag_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open_with(dir.path(), Registry::new().with(Arc::new(Look))).unwrap();
    let project = engine.create_project("leads", "").unwrap();
    let model: Arc<dyn Provider> = Arc::new(MockProvider::new(
        "m",
        vec![
            reply("Reading.", vec![read("a.md")]),
            reply(NOTE, vec![]),
            reply(RESULT, vec![]),
        ],
    ));
    let out = Harness::new(ModelRouter::single(model))
        .planner(false)
        .critic(false)
        .brief_task(false)
        .ask(
            &engine,
            &AgentProfile::builder().core_actions(["look.read"]).build(),
            &project.id,
            "go",
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(out.answer, RESULT);
}

#[tokio::test]
async fn a_one_line_answer_with_no_calls_costs_nothing_extra() {
    let r = run(vec![reply("There are 3 warm leads.", vec![])]).await;
    assert_eq!(r.requests.len(), 1);
    assert_eq!(r.answer, "There are 3 warm leads.");
}

#[tokio::test]
async fn a_one_line_answer_after_silent_calls_is_the_answer() {
    let r = run(vec![
        reply("", vec![read("seed.md")]),
        reply("Renamed 3 leads.", vec![]),
    ])
    .await;
    assert_eq!(r.requests.len(), 2);
    assert!(!nudged(&r));
    assert_eq!(r.answer, "Renamed 3 leads.");
}

#[tokio::test]
async fn a_note_again_after_the_nudge_is_the_answer() {
    let r = run(vec![
        reply("Reading the seed file.", vec![read("seed.md")]),
        reply(NOTE, vec![]),
        reply(NOTE_AGAIN, vec![]),
    ])
    .await;
    assert_eq!(r.requests.len(), 3, "one nudge, not more");
    assert_eq!(r.answer, NOTE_AGAIN);
}

#[tokio::test]
async fn a_short_result_after_the_nudge_is_the_answer() {
    let r = run(vec![
        reply("Reading the seed file.", vec![read("seed.md")]),
        reply(NOTE, vec![]),
        reply("Renamed 3 leads.", vec![]),
    ])
    .await;
    assert_eq!(r.requests.len(), 3);
    assert!(nudged(&r));
    assert_eq!(r.answer, "Renamed 3 leads.");
}

#[tokio::test]
async fn a_write_after_the_nudge_then_a_short_result_is_the_answer() {
    let r = run(vec![
        reply("Reading the seed file.", vec![read("seed.md")]),
        reply(NOTE, vec![]),
        reply("", vec![write("out/brand.md")]),
        reply("", vec![read("out/brand.md")]),
        reply("Wrote out/brand.md.", vec![]),
        reply("Wrote out/brand.md and read it back.", vec![]),
    ])
    .await;
    assert_eq!(
        r.requests.len(),
        6,
        "a one-line reply after more work is sent back again (ADR 0127)"
    );
    assert!(nudged(&r));
    assert_eq!(r.answer, "Wrote out/brand.md and read it back.");
}

#[tokio::test]
async fn a_first_reply_that_is_a_note_on_a_real_task_is_sent_back() {
    // t-42106d23: one model call, no tool call, and the turn was over.
    let r = run_full(
        true,
        LONG_ASK,
        vec![
            reply(
                "Finding every em dash and listing the product files first.",
                vec![],
            ),
            reply("", vec![read("product/a.md")]),
            reply(RESULT, vec![]),
        ],
    )
    .await;
    assert!(nudged(&r));
    assert_eq!(r.requests.len(), 3);
    assert_eq!(r.answer, RESULT);
}

#[tokio::test]
async fn a_one_line_answer_to_a_quick_question_is_not_sent_back() {
    let r = run_full(true, "Gumroad or Shopify?", vec![reply("Gumroad.", vec![])]).await;
    assert_eq!(r.requests.len(), 1);
    assert_eq!(r.answer, "Gumroad.");
}

#[tokio::test]
async fn a_note_after_more_work_is_sent_back_again() {
    // t-dfe88f78: nudged at step 24, then stopped on another note at step 32.
    let r = run(vec![
        reply("Reading the seed file.", vec![read("seed.md")]),
        reply(NOTE, vec![]),
        reply("Reading the next file.", vec![read("other.md")]),
        reply(NOTE_AGAIN, vec![]),
        reply("", vec![read("third.md")]),
        reply(RESULT, vec![]),
    ])
    .await;
    let nudges = r
        .requests
        .last()
        .unwrap()
        .messages
        .iter()
        .filter(|m| m.content.contains(NUDGE))
        .count();
    assert_eq!(nudges, 2);
    assert_eq!(r.answer, RESULT);
}

#[tokio::test]
async fn the_prompt_asks_for_all_the_work_and_no_offers() {
    let r = run(vec![reply(RESULT, vec![])]).await;
    let system = &r.requests[0].system;
    assert!(system.contains("Do all of the work asked"), "{system}");
    assert!(system.contains("do not offer optional extras"), "{system}");
    assert!(!system.contains("single most useful next step"), "{system}");
}

#[test]
fn a_line_that_cites_a_result_is_not_a_note() {
    // Live, claude:sonnet: this answer was sent back and cost a call to say it again.
    assert!(!reads_as_note(
        "12 × 12 = **144** (calc.eval, inv:ba7d6f7a)."
    ));
}

#[tokio::test]
async fn a_cited_one_line_result_after_narrated_calls_is_the_answer() {
    let cited = "The seed file lists 3 leads (look.read, inv:1a2b3c4d).";
    let r = run(vec![
        reply("Reading the seed file.", vec![read("seed.md")]),
        reply(cited, vec![]),
    ])
    .await;
    assert_eq!(r.requests.len(), 2);
    assert_eq!(r.answer, cited);
}

#[tokio::test]
async fn work_a_program_hands_over_keeps_a_one_line_first_reply() {
    let r = run_first_note(true, false, LONG_ASK, vec![reply("Copy ready.", vec![])]).await;
    assert_eq!(r.requests.len(), 1);
    assert_eq!(r.answer, "Copy ready.");
}

#[tokio::test]
async fn an_answer_sent_back_for_an_unwritten_file_is_shown_once() {
    // Live, claude:sonnet: the person saw the first answer and then the corrected one.
    let wrong = "Saved a.md and b.md.";
    let right = "Saved a.md and read it back. b.md was not written.";
    let r = run(vec![
        reply("", vec![write("a.md")]),
        reply("", vec![read("a.md")]),
        reply(wrong, vec![]),
        reply(right, vec![]),
    ])
    .await;
    assert_eq!(r.requests.len(), 4);
    assert_eq!(r.withdrawn, vec![wrong.to_string()]);
    assert_eq!(r.shown, right);
    assert_eq!(r.answer, right);
}

#[tokio::test]
async fn the_prompt_allows_one_more_try_of_a_failed_call() {
    let r = run(vec![reply(RESULT, vec![])]).await;
    let system = &r.requests[0].system;
    assert!(system.contains("is worth one more try"), "{system}");
    assert!(
        !system.contains("Do not re-run a tool you already ran"),
        "{system}"
    );
    assert!(!system.contains('\u{2014}'), "no em dash: {system}");
}

#[tokio::test]
async fn notes_are_sent_back_at_most_eight_times_a_turn() {
    let mut script = Vec::new();
    for i in 0..9 {
        script.push(reply(
            "Reading the next file.",
            vec![read(&format!("{i}.md"))],
        ));
        script.push(reply(NOTE, vec![]));
    }
    let r = run(script).await;
    let nudges = r
        .requests
        .last()
        .unwrap()
        .messages
        .iter()
        .filter(|m| m.content.contains(NUDGE))
        .count();
    assert_eq!(nudges, 8);
    assert_eq!(r.requests.len(), 18);
    assert_eq!(r.answer, NOTE);
}
