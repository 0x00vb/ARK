//! Colleagues a person's chat turn started continue that chat when they finish (ADR 0126).
//! Scripted models only.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ark_model::{Completion, Message, Provider, ProviderError, ToolCall, ToolDef, Usage};
use ark_orchestrate::*;
use ark_sdk::prelude::*;
use serde_json::json;
use tokio::sync::Notify;

const CONTINUED: &str = "colleagues you started from this chat have finished";

/// The person's "zzfan" starts h1 on part one and h2 on part `two` (or `bad`, or `slow`);
/// "zzloop" starts h1 once, and so does each continuation of it. Helpers answer from their
/// brief; a continuation answers "Combined.".
struct Model {
    two: &'static str,
    gate: Notify,
    loops: AtomicUsize,
    requests: Mutex<Vec<String>>,
}

fn answer(text: &str) -> Completion {
    Completion {
        content: text.into(),
        tool_calls: vec![],
        usage: Usage::measured(10, 5),
    }
}

fn start(calls: &[(&str, String)]) -> Completion {
    Completion {
        content: String::new(),
        tool_calls: calls
            .iter()
            .enumerate()
            .map(|(n, (to, objective))| ToolCall {
                id: format!("c{n}"),
                name: "agents.start".into(),
                arguments: json!({ "to": to, "objective": objective }),
            })
            .collect(),
        usage: Usage::measured(10, 5),
    }
}

#[async_trait::async_trait]
impl Provider for Model {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        _: &[ToolDef],
    ) -> ark_model::Result<Completion> {
        let text = format!(
            "{system}\n{}",
            messages
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        );
        self.requests.lock().unwrap().push(text.clone());
        let started = messages
            .iter()
            .any(|m| m.tool_name.as_deref() == Some("agents.start"));
        if text.contains("delegated by ceo") {
            if text.contains("part bad") {
                return Err(ProviderError::Api {
                    provider: "m".into(),
                    detail: "the source did not answer".into(),
                });
            }
            if text.contains("part slow") {
                self.gate.notified().await;
            }
            let part = text.split("part ").nth(1).unwrap_or("").split_whitespace();
            return Ok(answer(&format!(
                "result of part {}",
                part.take(1).collect::<String>()
            )));
        }
        if text.contains(CONTINUED) {
            if text.contains("zzloop") && !started {
                let n = self.loops.fetch_add(1, Ordering::SeqCst);
                return Ok(start(&[("h1", format!("part loop{n}"))]));
            }
            return Ok(answer("Combined."));
        }
        if text.contains("zzping") {
            return Ok(answer("pong"));
        }
        if text.contains("zzbreak") {
            if started {
                return Err(ProviderError::Api {
                    provider: "m".into(),
                    detail: "the turn broke".into(),
                });
            }
            return Ok(start(&[("h2", "part slow".into())]));
        }
        if text.contains("zznarrate") && !started {
            let mut c = start(&[("h1", "part one".into())]);
            c.content = "Starting the translator on part one.".into();
            return Ok(c);
        }
        if started {
            return Ok(answer("When they report back, I will combine them."));
        }
        if text.contains("zzboth") {
            return Ok(start(&[
                ("h1", "part slow".into()),
                ("h2", "part slow".into()),
            ]));
        }
        if text.contains("zzfan") {
            return Ok(start(&[
                ("h1", "part one".into()),
                ("h2", format!("part {}", self.two)),
            ]));
        }
        if text.contains("zzloop") {
            let n = self.loops.fetch_add(1, Ordering::SeqCst);
            return Ok(start(&[("h1", format!("part loop{n}"))]));
        }
        if text.contains("zzmake") {
            return Ok(
                if messages
                    .iter()
                    .any(|m| m.tool_name.as_deref() == Some("agents.create"))
                {
                    answer("made")
                } else {
                    Completion {
                        content: String::new(),
                        tool_calls: vec![ToolCall {
                            id: "c0".into(),
                            name: "agents.create".into(),
                            arguments: json!({ "name": "trad", "role": "translates" }),
                        }],
                        usage: Usage::measured(10, 5),
                    }
                },
            );
        }
        Ok(answer("done"))
    }

    fn describe(&self) -> String {
        "m".into()
    }
}

struct World {
    _dir: tempfile::TempDir,
    ark: Ark,
    o: Orchestrator,
    m: Arc<Model>,
}

async fn world(two: &'static str) -> World {
    let m = Arc::new(Model {
        two,
        gate: Notify::new(),
        loops: AtomicUsize::new(0),
        requests: Mutex::new(Vec::new()),
    });
    let dir = tempfile::tempdir().unwrap();
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(Arc::new(ark_model::MockProvider::new("unused", vec![])))
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("team", "a test").unwrap();
    let mm = m.clone();
    let o = Orchestrator::builder(&ark, &project)
        .configure_models(move |b| b.provider("m", mm as Arc<dyn Provider>))
        .build()
        .unwrap();
    o.spawn(AgentSpec::new("ceo").model("m").delegates_to(["h1", "h2"]))
        .await
        .unwrap();
    for h in ["h1", "h2"] {
        o.spawn(AgentSpec::new(h).model("m")).await.unwrap();
    }
    World {
        _dir: dir,
        ark,
        o,
        m,
    }
}

fn said(text: &str) -> TaskSpec {
    let mut t = TaskSpec::new(text);
    t.person = true;
    t
}

fn continued(o: &Orchestrator) -> Vec<TaskRecord> {
    o.tasks()
        .into_iter()
        .filter(|t| t.continues.is_some())
        .collect()
}

async fn settled(o: &Orchestrator) {
    for _ in 0..500 {
        if o.tasks().iter().all(|t| t.state.is_final()) {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if o.tasks().iter().all(|t| t.state.is_final()) {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("never settled: {:?}", o.tasks());
}

#[tokio::test]
async fn two_helpers_continue_the_chat_once_with_both_results() {
    let w = world("bad").await;
    let turn = w.o.run("ceo", said("zzfan translate")).await.unwrap();
    settled(&w.o).await;
    let next = continued(&w.o);
    assert_eq!(next.len(), 1, "one continuation for the batch");
    assert_eq!(next[0].continues.as_deref(), Some(turn.id.as_str()));
    assert_eq!(next[0].owner, "ceo");
    assert!(!next[0].person, "nobody wrote it");
    assert_eq!(next[0].result.as_deref(), Some("Combined."));
    let requests = w.m.requests.lock().unwrap();
    let brief = requests.iter().find(|r| r.contains(CONTINUED)).unwrap();
    assert!(brief.contains("result of part one"), "{brief}");
    assert!(brief.contains("could not finish: part bad"), "{brief}");
    assert!(brief.contains("(data)"), "results are data: {brief}");
    assert!(
        w.o.agent("ceo").unwrap().inbox.is_empty(),
        "the continuation read them"
    );
}

#[tokio::test]
async fn a_helper_still_working_holds_the_batch_and_a_message_meanwhile_does_not_continue() {
    let w = world("slow").await;
    w.o.run("ceo", said("zzfan translate")).await.unwrap();
    let pong = w.o.run("ceo", said("zzping")).await.unwrap();
    assert_eq!(pong.result.as_deref(), Some("pong"));
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(continued(&w.o).is_empty(), "h2 is still working");
    w.m.gate.notify_one();
    settled(&w.o).await;
    assert_eq!(continued(&w.o).len(), 1);
}

#[tokio::test]
async fn helpers_the_person_stopped_do_not_continue_the_chat() {
    let w = world("slow").await;
    w.o.run("ceo", said("zzboth translate")).await.unwrap();
    let helpers: Vec<TaskRecord> =
        w.o.tasks()
            .into_iter()
            .filter(|t| t.owner != "ceo")
            .collect();
    assert_eq!(helpers.len(), 2);
    for h in helpers {
        w.o.cancel_and_wait(&h.id).await.unwrap();
    }
    settled(&w.o).await;
    assert!(continued(&w.o).is_empty());
}

#[tokio::test]
async fn continuations_stop_at_three_per_message_and_a_new_message_resets_them() {
    let w = world("one").await;
    w.o.run("ceo", said("zzloop translate")).await.unwrap();
    settled(&w.o).await;
    assert_eq!(continued(&w.o).len(), 3, "at most three continuations");
    w.o.run("ceo", said("zzloop again")).await.unwrap();
    settled(&w.o).await;
    assert_eq!(continued(&w.o).len(), 6, "a new message allows three more");
}

#[tokio::test]
async fn a_colleague_an_agent_creates_works_in_that_agents_folder() {
    let w = world("one").await;
    let folder = tempfile::tempdir().unwrap();
    let scope = ProjectId(w.o.agent("ceo").unwrap().scope);
    w.ark.engine().set_workspace(&scope, folder.path()).unwrap();
    w.o.run("ceo", said("zzmake a translator")).await.unwrap();
    let trad = w.o.agent("trad").expect("created");
    let engine = w.ark.engine();
    assert_eq!(
        engine.workspace_dir(&ProjectId(trad.scope)).unwrap(),
        engine.workspace_dir(&scope).unwrap()
    );
    let h1 = w.o.agent("h1").unwrap();
    assert_ne!(
        engine.workspace_dir(&ProjectId(h1.scope)).unwrap(),
        engine.workspace_dir(&scope).unwrap(),
        "an agent the person made keeps the project's folder"
    );
}

#[tokio::test]
async fn a_turn_that_narrates_its_start_and_ends_on_one_line_is_not_asked_again() {
    let w = world("one").await;
    let turn = w.o.run("ceo", said("zznarrate translate")).await.unwrap();
    assert_eq!(
        turn.result.as_deref(),
        Some("When they report back, I will combine them.")
    );
    settled(&w.o).await;
    let asked =
        w.m.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| {
                r.contains("zznarrate") && !r.contains(CONTINUED) && !r.contains("delegated by ceo")
            })
            .count();
    assert_eq!(asked, 2, "the start, then the one-line reply: no nudge");
    assert_eq!(continued(&w.o).len(), 1);
}

#[tokio::test]
async fn helpers_of_a_turn_that_failed_do_not_continue_the_chat() {
    let w = world("one").await;
    let turn = w.o.run("ceo", said("zzbreak translate")).await.unwrap();
    assert_eq!(turn.state, TaskState::Failed);
    w.m.gate.notify_one();
    settled(&w.o).await;
    assert!(continued(&w.o).is_empty());
}

#[tokio::test]
async fn the_stored_continuation_has_no_persons_message() {
    let w = world("one").await;
    w.o.run("ceo", said("zzfan translate")).await.unwrap();
    settled(&w.o).await;
    let chat = w.o.agent("ceo").unwrap().session.expect("a chat");
    let turns = ark_runtime::session::Session::open(w.ark.engine(), &chat)
        .unwrap()
        .expect("the chat")
        .transcript(w.ark.engine(), None)
        .unwrap();
    assert_eq!(turns.len(), 2);
    assert!(turns[0].input.contains("zzfan"));
    assert_eq!(turns[1].input, "", "nobody wrote the continuation");
    assert_eq!(turns[1].answer, "Combined.");
}
