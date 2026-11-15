//! Pictures a person attaches to a task (ADR 0121): they reach the model, stay with the task and
//! its turn, are refused everywhere but the person's own message, and fail the task when the
//! file changed after it was sent.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ark_model::{Completion, MockProvider, Provider, ToolCall, Usage};
use ark_orchestrate::*;
use ark_sdk::prelude::*;
use serde_json::json;

/// A 1x1 PNG.
const PIXEL: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64, 0x60, 0xf8, 0x5f,
    0x0f, 0x00, 0x02, 0x87, 0x01, 0x80, 0xeb, 0x47, 0xba, 0x92, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

/// The wrapped mock records what it was sent. It reads images when `reads`, and waits while
/// `hold` is set, so a test decides when a task gets to run.
struct Model {
    mock: Arc<MockProvider>,
    reads: bool,
    hold: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl Provider for Model {
    async fn complete(
        &self,
        system: &str,
        messages: &[ark_model::Message],
        tools: &[ark_model::ToolDef],
    ) -> ark_model::Result<Completion> {
        while self.hold.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self.mock.complete(system, messages, tools).await
    }
    fn describe(&self) -> String {
        "model".into()
    }
    fn reads_pictures(&self) -> bool {
        self.reads
    }
}

struct World {
    _dir: tempfile::TempDir,
    ark: Ark,
    o: Orchestrator,
    model: Arc<MockProvider>,
    hold: Arc<AtomicBool>,
    folder: std::path::PathBuf,
}

/// An agent over a model that makes `steps` calls to a tool that is not there, then answers.
async fn world(reads: bool, steps: usize) -> World {
    let dir = tempfile::tempdir().unwrap();
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(Arc::new(MockProvider::new("unused", vec![])))
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("team", "test").unwrap();
    let script = (0..steps)
        .map(|n| Completion {
            content: String::new(),
            tool_calls: vec![ToolCall {
                id: format!("c{n}"),
                name: "nothing.here".into(),
                arguments: json!({}),
            }],
            usage: Usage::default(),
        })
        .collect();
    let model = Arc::new(
        MockProvider::new("m", script).with_fallback(|_| Completion {
            content: "It is one pixel.".into(),
            tool_calls: vec![],
            usage: Usage::default(),
        }),
    );
    let hold = Arc::new(AtomicBool::new(false));
    let provider: Arc<dyn Provider> = Arc::new(Model {
        mock: model.clone(),
        reads,
        hold: hold.clone(),
    });
    let o = Orchestrator::builder(&ark, &project)
        .configure_models(move |b| b.provider("m", provider))
        .build()
        .unwrap();
    o.spawn(AgentSpec::new("w").model("m")).await.unwrap();
    o.spawn(AgentSpec::new("lead").model("m").delegates_to(["w"]))
        .await
        .unwrap();
    let folder = ark.engine().workspace_dir(&project.id).unwrap();
    std::fs::write(folder.join("shot.png"), PIXEL).unwrap();
    World {
        _dir: dir,
        ark,
        o,
        model,
        hold,
        folder,
    }
}

fn person(text: &str, images: &[&str]) -> TaskSpec {
    let mut spec = TaskSpec::new(text).images(images.iter().copied());
    spec.person = true;
    spec
}

fn pictures_per_request(model: &MockProvider) -> Vec<usize> {
    model
        .recorded()
        .iter()
        .map(|r| r.messages.iter().map(|m| m.pictures.len()).sum())
        .collect()
}

#[tokio::test]
async fn a_task_with_images_reaches_the_model_and_stays_with_its_task_and_turn() {
    let w = world(true, 2).await;
    let id =
        w.o.submit("w", person("what is this", &["shot.png"]))
            .unwrap();
    let done = w.o.wait(&id).await.unwrap();
    assert_eq!(done.state, TaskState::Done, "{:?}", done.error);
    let sent: Vec<_> = w
        .model
        .recorded()
        .into_iter()
        .flat_map(|r| r.messages)
        .flat_map(|m| m.pictures)
        .collect();
    assert!(!sent.is_empty(), "the model was shown the picture");
    assert!(sent.iter().all(|p| p.media_type == "image/png"));
    assert_eq!(done.images.len(), 1);
    assert_eq!(done.images[0].path, "shot.png");
    assert_eq!(done.images[0].size, PIXEL.len() as u64);
    assert_eq!(done.images[0].sha256.len(), 64);
    let graph = w.o.graph();
    let node = graph.tasks.iter().find(|t| t.task == id).unwrap();
    assert_eq!(node.images, ["shot.png"]);
    let agent = w.o.agent("w").unwrap();
    let turns = w
        .ark
        .engine()
        .store
        .list_session_turns(agent.session.as_deref().unwrap(), None)
        .unwrap();
    assert_eq!(turns[0]["images"], json!(["shot.png"]));
}

#[tokio::test]
async fn pictures_per_request_are_counted() {
    let w = world(true, 3).await;
    let id =
        w.o.submit("w", person("what is this", &["shot.png"]))
            .unwrap();
    w.o.wait(&id).await.unwrap();
    let counts = pictures_per_request(&w.model);
    println!("pictures per request: {counts:?}");
    assert_eq!(counts.len(), 4, "three tool steps and the answer");
    assert!(
        counts.iter().all(|n| *n == 1),
        "the task message carries its one picture in every request: {counts:?}"
    );
}

#[tokio::test]
async fn a_model_that_reads_no_images_is_told_so_and_the_turn_still_lists_the_image() {
    let w = world(false, 0).await;
    let id =
        w.o.submit("w", person("what is this", &["shot.png"]))
            .unwrap();
    let done = w.o.wait(&id).await.unwrap();
    assert_eq!(done.state, TaskState::Done, "{:?}", done.error);
    let calls = w.model.recorded();
    assert!(calls
        .iter()
        .flat_map(|r| &r.messages)
        .all(|m| m.pictures.is_empty()));
    assert!(calls
        .iter()
        .flat_map(|r| &r.messages)
        .any(|m| m.content.contains("this model is not sent images")));
    assert_eq!(w.o.graph().tasks[0].images, ["shot.png"]);
}

#[tokio::test]
async fn a_task_without_images_sends_none_and_its_node_lists_none() {
    let w = world(true, 0).await;
    let id = w.o.submit("w", person("hello", &[])).unwrap();
    w.o.wait(&id).await.unwrap();
    assert_eq!(pictures_per_request(&w.model), [0]);
    let node = serde_json::to_value(&w.o.graph().tasks[0]).unwrap();
    assert!(node.get("images").is_none(), "{node}");
}

fn refused(result: Result<String, OrchError>, why: &str) {
    let error = result.unwrap_err().to_string();
    assert!(error.contains(why), "{error}");
}

#[tokio::test]
async fn pictures_that_are_too_big_not_images_outside_or_too_many_start_no_task() {
    let w = world(true, 0).await;
    std::fs::write(w.folder.join("notes.txt"), "hello").unwrap();
    let mut big = PIXEL.to_vec();
    big.resize(3_600_000, 0);
    std::fs::write(w.folder.join("big.png"), &big).unwrap();
    std::fs::write(w.folder.parent().unwrap().join("outside.png"), PIXEL).unwrap();
    for (name, why) in [
        ("big.png", "larger than 3.5 MB"),
        ("notes.txt", "not a PNG"),
        ("../outside.png", "outside the project folder"),
        ("missing.png", "missing.png"),
    ] {
        refused(w.o.submit("w", person("look", &[name])), why);
    }
    let many: Vec<String> = (0..9).map(|i| format!("{i}.png")).collect();
    refused(
        w.o.submit(
            "w",
            person("look", &many.iter().map(String::as_str).collect::<Vec<_>>()),
        ),
        "at most 8",
    );
    assert!(w.o.tasks().is_empty(), "a refused message starts no task");
}

#[tokio::test]
async fn pictures_are_refused_for_everyone_but_the_person() {
    let w = world(true, 0).await;
    let mut automation = TaskSpec::new("report").images(["shot.png"]);
    automation.person = false;
    refused(
        w.o.submit("w", automation),
        "only with a message the person sends",
    );
    let mut group = person("turn", &["shot.png"]);
    group.group_turn = true;
    refused(
        w.o.submit("w", group),
        "only with a message the person sends",
    );
    let mut reply = person("later", &["shot.png"]);
    reply.reply = true;
    refused(
        w.o.submit("w", reply),
        "only with a message the person sends",
    );
    let mut posted = person("report", &["shot.png"]);
    posted.report_to = Some("s-1".into());
    refused(
        w.o.submit("w", posted),
        "only with a message the person sends",
    );
    let scheduled =
        w.o.schedule("w", person("daily", &["shot.png"]), When::every_minutes(10));
    assert!(
        scheduled
            .unwrap_err()
            .to_string()
            .contains("cannot carry pictures"),
        "a schedule keeps no pictures"
    );
    assert!(w.o.tasks().is_empty() && w.o.schedules().is_empty());
}

#[tokio::test]
async fn a_picture_that_changed_or_was_removed_fails_the_task_and_names_it() {
    let w = world(true, 0).await;
    w.hold.store(true, Ordering::SeqCst);
    // The first task holds the agent, so the others wait in the queue with their pictures.
    let first = w.o.submit("w", person("first", &[])).unwrap();
    let edited = w.o.submit("w", person("edited", &["shot.png"])).unwrap();
    std::fs::write(w.folder.join("gone.png"), PIXEL).unwrap();
    let removed = w.o.submit("w", person("removed", &["gone.png"])).unwrap();
    let unchanged = {
        std::fs::write(w.folder.join("same.png"), PIXEL).unwrap();
        w.o.submit("w", person("same", &["same.png"])).unwrap()
    };
    std::fs::write(w.folder.join("shot.png"), [PIXEL, &[0]].concat()).unwrap();
    std::fs::remove_file(w.folder.join("gone.png")).unwrap();
    w.hold.store(false, Ordering::SeqCst);
    w.o.wait(&first).await.unwrap();
    let ended = |id: String| {
        let o = w.o.clone();
        async move {
            tokio::time::timeout(Duration::from_secs(10), o.wait(&id))
                .await
                .unwrap()
                .unwrap()
        }
    };
    let edited = ended(edited).await;
    assert_eq!(edited.state, TaskState::Failed);
    assert_eq!(
        edited.error.as_deref(),
        Some("The picture shot.png changed or was removed after it was sent.")
    );
    let removed = ended(removed).await;
    assert_eq!(removed.state, TaskState::Failed);
    assert_eq!(
        removed.error.as_deref(),
        Some("The picture gone.png changed or was removed after it was sent.")
    );
    assert_eq!(ended(unchanged).await.state, TaskState::Done);
}

#[test]
fn records_from_before_images_load_and_write_without_them() {
    let node: serde_json::Value = serde_json::to_value(TaskSpec::new("x")).unwrap();
    assert!(node.get("images").is_none());
    let spec: TaskSpec = serde_json::from_value(json!({"objective": "x"})).unwrap();
    assert!(spec.images.is_empty());
    let with: TaskSpec =
        serde_json::from_value(json!({"objective": "x", "images": ["a.png"]})).unwrap();
    assert_eq!(with.images, ["a.png"]);
}
