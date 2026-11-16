//! A model that writes its full answer beside one more read of what it just changed, again and
//! again, ends the turn at the second restatement (ADR 0051). Work that reads something new, a
//! change in between, or a different answer does not end it.

use std::sync::Arc;

use ark_core::{ActionDescriptor, ArtifactKind, RiskTier};
use ark_model::{Completion, MockProvider, Provider, ToolCall, Usage};
use ark_runtime::{AgentProfile, Engine, Harness, HarnessEvent, ModelRouter};
use ark_tools::{ActionOutput, Adapter, ProducedArtifact, Registry, RunCtx};
use serde_json::{json, Value};

/// Chapters of a book: written (a change) and read.
struct Book;

#[async_trait::async_trait]
impl Adapter for Book {
    fn name(&self) -> &str {
        "book"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new("book.write", "write a chapter", RiskTier::Mutate),
            ActionDescriptor::new("book.read", "read a chapter", RiskTier::Read),
        ]
    }
    async fn run(
        &self,
        action: &str,
        input: Value,
        _c: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        let path = input["path"].as_str().unwrap_or("").to_string();
        Ok(match action {
            "book.write" => {
                let text = input["text"].as_str().unwrap_or("").to_string();
                ActionOutput::json(json!({ "saved": path })).with_artifact(
                    ProducedArtifact::bytes(ArtifactKind::Text, path.clone(), text.into_bytes())
                        .at(path),
                )
            }
            _ => ActionOutput::json(json!({ "path": path, "text": format!("the text of {path}") })),
        })
    }
}

fn call(name: &str, args: Value) -> ToolCall {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    ToolCall {
        id: format!("c{}", N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)),
        name: name.into(),
        arguments: args,
    }
}

fn reply(text: &str, calls: Vec<ToolCall>) -> Completion {
    Completion {
        content: text.into(),
        tool_calls: calls,
        usage: Usage::measured(100, 20),
    }
}

fn write(path: &str, text: &str) -> Completion {
    reply(
        "Writing the chapter.",
        vec![call("book.write", json!({ "path": path, "text": text }))],
    )
}

fn read(path: &str) -> ToolCall {
    call("book.read", json!({ "path": path }))
}

/// A full answer about chapter one, with `first` as its opening line: the same answer, said again.
fn answer(first: &str) -> String {
    format!(
        "{first}\n\n\
         **What changed.** Chapter one now opens on the harbour at dawn, with Mara waiting for the \
         ferry that never comes. The letter she carries is introduced in the second scene.\n\n\
         **Style.** Sentences are short, the tense is past, and the point of view stays with Mara \
         throughout, as in the outline.\n\n\
         **Limits.** I did not change chapters two and three, so the letter's first mention there \
         still contradicts the new opening.\n\n\
         **Checked.** I read the saved chapter back: the opening, the letter and the last line of \
         the first scene are as described above.\n\n\
         **Next step.** Read the new opening aloud and tell me whether the pace feels right."
    )
}

/// A long note on something else: not the same answer.
const OTHER: &str = "Looking further, the archive holds three drafts of the lighthouse keeper's \
diary.\n\nThe oldest is dated in winter and lists storms by name; the second adds a map of the \
reefs; the third is torn after the twelfth page.\n\nNone of them mentions a ferry, a harbour or \
a letter, so the keeper's story runs apart from the plot so far.\n\nI will compare their \
handwriting before deciding which draft the book should quote.\n\nThe paper of the torn draft is \
thinner and yellowed, which suggests it travelled; its ink smudges where salt water reached it. \
Two pages carry a stamp from a mainland post office, closed for decades.";

struct Run {
    answer: String,
    model_calls: usize,
    tool_calls: Vec<String>,
    ran_out: bool,
}

async fn run(script: Vec<Completion>) -> Run {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open_with(dir.path(), Registry::new().with(Arc::new(Book))).unwrap();
    let project = engine.create_project("book", "").unwrap();
    let model = MockProvider::new("m", script).with_fallback(|_| reply("FALLBACK", vec![]));
    let provider: Arc<dyn Provider> = Arc::new(model);
    let profile = AgentProfile::builder()
        .core_actions(["book.write", "book.read"])
        .build();
    let (mut model_calls, mut tool_calls) = (0, Vec::new());
    let out = Harness::new(ModelRouter::single(provider))
        .planner(false)
        .critic(false)
        .allow_mutate(true)
        .ask(
            &engine,
            &profile,
            &project.id,
            "rewrite the opening of chapter one",
            |e| match e {
                HarnessEvent::ModelCall { .. } => model_calls += 1,
                HarnessEvent::ToolCall { action, input, .. } => {
                    tool_calls.push(format!("{action} {}", input["path"].as_str().unwrap_or("")))
                }
                _ => {}
            },
        )
        .await
        .unwrap();
    Run {
        answer: out.answer,
        model_calls,
        tool_calls,
        ran_out: out.ran_out,
    }
}

#[tokio::test]
async fn a_restated_answer_beside_another_check_ends_the_turn() {
    // The owner's run: change, answer and read it back, answer again and read it back again...
    let r = run(vec![
        write("ch1.md", "Dawn at the harbour."),
        reply(
            &answer("Chapter one has a new opening."),
            vec![read("ch1.md")],
        ),
        reply(
            &answer("The new opening of chapter one is saved."),
            vec![read("ch1.md")],
        ),
        reply(
            &answer("Chapter one's opening is rewritten."),
            vec![read("ch1.md")],
        ),
    ])
    .await;
    assert_eq!(
        r.model_calls, 3,
        "the third reply restated the answer and ended the turn"
    );
    assert_eq!(
        r.tool_calls,
        ["book.write ch1.md", "book.read ch1.md"],
        "the second check did not run"
    );
    assert!(
        r.answer
            .starts_with("The new opening of chapter one is saved."),
        "{}",
        r.answer
    );
    assert!(
        !r.ran_out,
        "the turn ended on the model's own answer, not as unfinished"
    );
}

#[tokio::test]
async fn long_notes_beside_reads_of_new_material_do_not_end_the_turn() {
    // Writing a chapter, then reading sources it did not write: research, not a check.
    let r = run(vec![
        write("ch1.md", "Dawn at the harbour."),
        reply(
            &answer("Chapter one has a new opening."),
            vec![read("notes/harbour.md")],
        ),
        reply(
            &answer("The new opening of chapter one is saved."),
            vec![read("notes/ferry.md")],
        ),
        reply(
            "Done: the opening is rewritten.\n\nIt is saved in ch1.md.",
            vec![],
        ),
    ])
    .await;
    assert_eq!(r.model_calls, 4);
    assert_eq!(
        r.answer,
        "Done: the opening is rewritten.\n\nIt is saved in ch1.md."
    );
    assert_eq!(r.tool_calls.len(), 3);
}

#[tokio::test]
async fn a_change_between_checks_starts_over() {
    let r = run(vec![
        write("ch1.md", "Dawn at the harbour."),
        reply(
            &answer("Chapter one has a new opening."),
            vec![read("ch1.md")],
        ),
        write("ch1.md", "Dawn at the harbour, and rain."),
        reply(
            &answer("The new opening of chapter one is saved."),
            vec![read("ch1.md")],
        ),
        reply(
            &answer("Chapter one's opening is rewritten."),
            vec![read("ch1.md")],
        ),
    ])
    .await;
    assert_eq!(
        r.model_calls, 5,
        "the check after the fix counts as the first one again"
    );
    assert!(
        r.answer.starts_with("Chapter one's opening is rewritten."),
        "{}",
        r.answer
    );
    assert_eq!(
        r.tool_calls.len(),
        4,
        "write, read, write, read: the last read did not run"
    );
}

#[tokio::test]
async fn a_different_answer_beside_a_check_does_not_end_the_turn() {
    let r = run(vec![
        write("ch1.md", "Dawn at the harbour."),
        reply(
            &answer("Chapter one has a new opening."),
            vec![read("ch1.md")],
        ),
        reply(OTHER, vec![read("ch1.md")]),
        reply("Done.\n\nChapter one is saved.", vec![]),
    ])
    .await;
    assert_eq!(r.model_calls, 4);
    assert_eq!(r.answer, "Done.\n\nChapter one is saved.");
}

#[tokio::test]
async fn a_folder_holding_the_changed_file_counts_as_a_check() {
    // Searching the folder of what it wrote is reading its own work back too.
    let r = run(vec![
        write("book/ch1.md", "Dawn at the harbour."),
        reply(
            &answer("Chapter one has a new opening."),
            vec![read("book")],
        ),
        reply(
            &answer("The new opening of chapter one is saved."),
            vec![read("book/")],
        ),
    ])
    .await;
    assert_eq!(r.model_calls, 3);
    assert!(r
        .answer
        .starts_with("The new opening of chapter one is saved."));
}
