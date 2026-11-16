//! An agent hands its own work to the background and answers later in the same chat
//! (`work.later`, ADR 0118). Scripted models only.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ark_core::ExecutionMode;
use ark_model::{Completion, Message, Provider, ProviderError, ToolCall, ToolDef, Usage};
use ark_orchestrate::*;
use ark_runtime::session::{posts, Post, PostKind};
use ark_sdk::prelude::*;
use serde_json::{json, Value};
use tokio::sync::Notify;

const BRIEF: &str = "Read the filings and report the numbers";

/// Answers by what the request holds. A person's message "zzdig" hands work off, "zztwice" does so
/// twice, "zzping" answers at once, "save" saves a document; any other request is the handed-off
/// work, which waits for the gate when `hold` and fails when `fail`.
struct Model {
    gate: Notify,
    /// Holds the person's own request ("zzslow") until released.
    slow: Notify,
    hold: bool,
    fail: bool,
    requests: Mutex<Vec<(String, bool)>>,
}

fn answer(text: &str) -> Completion {
    Completion {
        content: text.into(),
        tool_calls: vec![],
        usage: Usage::measured(10, 5),
    }
}

fn call(name: &str, args: Value) -> Completion {
    Completion {
        content: String::new(),
        tool_calls: vec![ToolCall {
            id: "c0".into(),
            name: name.into(),
            arguments: args,
        }],
        usage: Usage::measured(10, 5),
    }
}

#[async_trait::async_trait]
impl Provider for Model {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> ark_model::Result<Completion> {
        let text = format!(
            "{system}\n{}",
            messages
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        );
        self.requests.lock().unwrap().push((
            text.clone(),
            system.contains("work.later") || tools.iter().any(|t| t.name == "work.later"),
        ));
        let sent = text.matches("will arrive in this chat").count();
        let mut brief = BRIEF.to_string();
        for marker in ["zzsave", "zzempty", "zzkey"] {
            if text.contains(marker) {
                brief = format!("{brief} {marker}");
            }
        }
        let later = json!({ "brief": brief });
        if text.contains("zzping") {
            return Ok(answer("pong"));
        }
        if text.contains("zzkick") {
            return Ok(
                if messages
                    .iter()
                    .any(|m| m.tool_name.as_deref() == Some("agents.start"))
                {
                    answer("started")
                } else {
                    call(
                        "agents.start",
                        json!({ "to": "helper", "objective": "assist helper" }),
                    )
                },
            );
        }
        if text.contains("zztwice") {
            return Ok(if sent < 2 {
                call("work.later", later)
            } else {
                answer("Let me look into it.")
            });
        }
        if text.contains("zzdig") {
            if text.contains("zzslow") && sent == 0 {
                self.slow.notified().await;
            }
            return Ok(if sent == 0 {
                call("work.later", later)
            } else {
                answer("Let me look into it.")
            });
        }
        if text.contains("zzsave") && !text.contains("zzdone") {
            return Ok(call("doc.save", json!({ "text": "a" })));
        }
        // The handed-off work.
        if text.contains(BRIEF) {
            if self.hold {
                self.gate.notified().await;
            }
            if self.fail {
                return Err(ProviderError::Api {
                    provider: "m".into(),
                    detail: "the source did not answer".into(),
                });
            }
            if text.contains("zzsave") && !text.contains("zzdone") {
                return Ok(call("doc.save", json!({ "text": "later" })));
            }
            if text.contains("zzkey") {
                return Ok(answer("The key is sk-ant-api03-abcdefghij"));
            }
            if text.contains("zzempty") {
                return Ok(answer("  "));
            }
            return Ok(answer("The filings show 42."));
        }
        Ok(answer("done"))
    }

    fn describe(&self) -> String {
        "m".into()
    }
}

struct Doc(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Adapter for Doc {
    fn name(&self) -> &str {
        "doc"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new("doc.save", "save", RiskTier::Mutate)]
    }
    async fn run(&self, _: &str, _: Value, _: &RunCtx<'_>) -> ark_tools::Result<ActionOutput> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ActionOutput::json(json!({ "zzdone": true })))
    }
}

struct World {
    _dir: tempfile::TempDir,
    ark: Ark,
    project: Project,
    o: Orchestrator,
    m: Arc<Model>,
    /// What `doc.save` really changed, and how many questions the person was asked.
    saves: Arc<AtomicUsize>,
    asked: Arc<AtomicUsize>,
}

/// Counts the questions and answers no to each.
struct Refuse(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Approver for Refuse {
    async fn decide(&self, _: &ApprovalRequest) -> ApprovalDecision {
        self.0.fetch_add(1, Ordering::SeqCst);
        ApprovalDecision::Deny
    }
}

fn world(hold: bool, fail: bool) -> World {
    let saves = Arc::new(AtomicUsize::new(0));
    let m = Arc::new(Model {
        gate: Notify::new(),
        slow: Notify::new(),
        hold,
        fail,
        requests: Mutex::new(Vec::new()),
    });
    let dir = tempfile::tempdir().unwrap();
    let ark = Ark::builder()
        .store_root(dir.path())
        .tool(Arc::new(Doc(saves.clone())))
        .provider(Arc::new(ark_model::MockProvider::new("unused", vec![])))
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("team", "a test").unwrap();
    let asked = Arc::new(AtomicUsize::new(0));
    let mm = m.clone();
    let o = Orchestrator::builder(&ark, &project)
        .approvals(Arc::new(ApprovalGate::new(Arc::new(Refuse(asked.clone())))))
        .configure_models(move |b| b.provider("m", mm as Arc<dyn Provider>))
        .build()
        .unwrap();
    World {
        _dir: dir,
        ark,
        project,
        o,
        m,
        saves,
        asked,
    }
}

fn said(text: &str) -> TaskSpec {
    let mut t = TaskSpec::new(text);
    t.person = true;
    t
}

fn lanes(o: &Orchestrator) -> Vec<TaskRecord> {
    o.tasks().into_iter().filter(|t| t.reply).collect()
}

async fn until(what: &str, mut ok: impl FnMut() -> bool) {
    for _ in 0..500 {
        if ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("never: {what}");
}

fn chat_posts(w: &World, agent: &str) -> Vec<Post> {
    let chat = w.o.agent(agent).unwrap().session.expect("a chat");
    posts(w.ark.engine(), &chat, 0, 50)
        .unwrap()
        .into_iter()
        .map(|(_, p)| p)
        .collect()
}

async fn ceo(w: &World, spec: impl FnOnce(AgentSpec) -> AgentSpec) {
    w.o.spawn(spec(AgentSpec::new("ceo").model("m")))
        .await
        .unwrap();
}

#[tokio::test]
async fn the_turn_ends_with_a_short_reply_and_the_answer_arrives_as_a_message() {
    let w = world(true, false);
    ceo(&w, |s| s).await;
    let turn = w.o.run("ceo", said("zzdig the filings")).await.unwrap();
    assert_eq!(turn.state, TaskState::Done);
    assert_eq!(turn.result.as_deref(), Some("Let me look into it."));
    let lane = lanes(&w.o);
    assert_eq!(lane.len(), 1);
    assert!(!lane[0].state.is_final(), "the work goes on after the turn");
    assert_eq!(lane[0].objective, BRIEF);
    assert_eq!(lane[0].report_to, w.o.agent("ceo").unwrap().session);
    assert!(!lane[0].person, "the work is not a person turn");
    assert_eq!(
        w.o.agent("ceo").unwrap().state,
        AgentState::Running,
        "the tile shows it works"
    );
    assert!(chat_posts(&w, "ceo").is_empty(), "nothing before it ends");

    w.m.gate.notify_one();
    until("a post", || !chat_posts(&w, "ceo").is_empty()).await;
    let posted = chat_posts(&w, "ceo");
    assert_eq!(posted.len(), 1);
    assert_eq!(posted[0].kind, PostKind::Message);
    assert_eq!(posted[0].text, "The filings show 42.");
    assert_eq!(posted[0].from, "ceo");
    assert!(posted[0].ok);
    assert_eq!(posted[0].work.as_deref(), Some(lane[0].id.as_str()));
    until("idle", || {
        w.o.agent("ceo").unwrap().state == AgentState::Idle
    })
    .await;
    assert!(w.o.task(&lane[0].id).unwrap().reported);
}

#[tokio::test]
async fn a_new_message_does_not_cancel_it_and_the_next_turn_knows_what_it_said() {
    let w = world(true, false);
    ceo(&w, |s| s).await;
    w.o.run("ceo", said("zzdig the filings")).await.unwrap();
    let pong = w.o.run("ceo", said("zzping")).await.unwrap();
    assert_eq!(pong.result.as_deref(), Some("pong"));
    let lane = lanes(&w.o);
    assert!(
        !lane[0].state.is_final(),
        "the person's message left it running"
    );
    w.m.gate.notify_one();
    until("a post", || !chat_posts(&w, "ceo").is_empty()).await;
    assert_eq!(w.o.task(&lane[0].id).unwrap().state, TaskState::Done);
    w.o.run("ceo", said("zzping again")).await.unwrap();
    let seen = w.m.requests.lock().unwrap();
    let last = &seen.last().unwrap().0;
    assert!(
        last.contains("[you, later] The filings show 42."),
        "the next turn is told what the agent said later: {last}"
    );
}

#[tokio::test]
async fn stop_ends_it_and_posts_nothing() {
    let w = world(true, false);
    ceo(&w, |s| s).await;
    w.o.run("ceo", said("zzdig the filings")).await.unwrap();
    let id = lanes(&w.o)[0].id.clone();
    w.o.cancel_and_wait(&id).await.unwrap();
    assert_eq!(w.o.task(&id).unwrap().state, TaskState::Cancelled);
    until("idle", || {
        w.o.agent("ceo").unwrap().state == AgentState::Idle
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(chat_posts(&w, "ceo").is_empty());
}

#[tokio::test]
async fn a_failed_hand_off_posts_a_failed_message() {
    let w = world(false, true);
    ceo(&w, |s| s).await;
    w.o.run("ceo", said("zzdig the filings")).await.unwrap();
    until("a post", || !chat_posts(&w, "ceo").is_empty()).await;
    let p = &chat_posts(&w, "ceo")[0];
    assert_eq!(p.kind, PostKind::Message);
    assert!(!p.ok);
    assert!(p.text.contains("the source did not answer"), "{}", p.text);
}

#[tokio::test]
async fn only_a_persons_or_a_group_turn_has_it_and_no_other_task_does() {
    let w = world(false, false);
    ceo(&w, |s| s.delegates_to(["helper"])).await;
    w.o.spawn(AgentSpec::new("helper").model("m"))
        .await
        .unwrap();
    // What an app's `submit` makes, and what Give work makes (a board item's task), are not chat
    // turns: not offered, even from the person's own start.
    w.o.run("ceo", TaskSpec::new("plain task")).await.unwrap();
    let card =
        w.o.add_work(board::PERSON, "Draft chapter 3", WorkChange::default())
            .unwrap();
    w.o.run("ceo", said("item task").item(card.id))
        .await
        .unwrap();
    {
        let seen = w.m.requests.lock().unwrap();
        for what in ["plain task", "item task"] {
            let first = seen.iter().find(|(t, _)| t.contains(what)).unwrap();
            assert!(!first.1, "{what} was offered work.later");
        }
    }
    // What the group driver submits for a member's turn.
    let mut group = TaskSpec::new("zzdig the filings");
    group.group_turn = true;
    let t = w.o.run("ceo", group).await.unwrap();
    assert_eq!(t.state, TaskState::Done);
    until("a post", || !chat_posts(&w, "ceo").is_empty()).await;
    {
        let seen = w.m.requests.lock().unwrap();
        let turn = seen.iter().find(|(t, _)| t.contains("zzdig")).unwrap();
        assert!(turn.1, "the group turn has it");
        let work = seen
            .iter()
            .find(|(t, _)| t.contains(BRIEF) && !t.contains("zzdig"))
            .unwrap();
        assert!(!work.1, "the handed-off work may not hand off again");
    }
    // A task another agent started is not a chat turn.
    w.o.run("ceo", said("zzkick the helper")).await.unwrap();
    until("helper ran", || {
        w.o.tasks()
            .iter()
            .any(|t| t.owner == "helper" && t.state.is_final())
    })
    .await;
    let seen = w.m.requests.lock().unwrap();
    let started: Vec<_> = seen
        .iter()
        .filter(|(t, _)| t.contains("assist helper") && !t.contains("zzkick"))
        .collect();
    assert!(!started.is_empty());
    assert!(
        !started.iter().any(|(_, offered)| *offered),
        "a started task was offered work.later"
    );
}

#[tokio::test]
async fn asking_twice_in_a_turn_starts_one_and_an_agent_keeps_at_most_three() {
    let w = world(true, false);
    ceo(&w, |s| s).await;
    let t = w.o.run("ceo", said("zztwice please")).await.unwrap();
    assert_eq!(t.state, TaskState::Done);
    assert_eq!(lanes(&w.o).len(), 1, "the second call changed nothing");
    for _ in 0..2 {
        w.o.run("ceo", said("zzdig one more")).await.unwrap();
    }
    assert_eq!(lanes(&w.o).len(), 3);
    let fourth = w.o.run("ceo", said("zzdig a fourth")).await.unwrap();
    assert_eq!(lanes(&w.o).len(), 3, "a fourth hand-off was started");
    let refused = fourth
        .tool_calls
        .iter()
        .find(|c| c.action == "work.later")
        .unwrap();
    assert_eq!(refused.ok, Some(false));
}

#[tokio::test]
async fn the_work_asks_where_the_turn_asked_and_acts_where_it_acted() {
    // An agent that acts alone, and a member of a group (the self-origin trap).
    for (alone, in_group, saved) in [(true, false, 1), (true, true, 1), (false, false, 0)] {
        let w = world(false, false);
        ceo(&w, |s| {
            let mut s = s.allow_mutate(alone);
            if in_group {
                s.groups = vec!["g1".into()];
            }
            s
        })
        .await;
        if in_group {
            let mut mate = AgentSpec::new("mate").model("m");
            mate.groups = vec!["g1".into()];
            w.o.spawn(mate).await.unwrap();
        }
        // The chat is in ask mode (a chat nobody chose a mode for is Auto and asks nothing,
        // ADR 0123). The turn's own save is the baseline: the work must do what the turn does.
        let chat = w.o.start_thread("ceo").unwrap();
        let mut s = Session::open(w.ark.engine(), &chat).unwrap().unwrap();
        s.set_execution_mode(w.ark.engine(), 0, ExecutionMode::Manual)
            .unwrap();
        let turn =
            w.o.run("ceo", said("zzsave a").thread(&chat))
                .await
                .unwrap();
        let in_turn = w.saves.load(Ordering::SeqCst);
        assert_eq!(in_turn, saved, "baseline alone={alone} group={in_group}");
        let _ = turn;
        let asked_in_turn = w.asked.load(Ordering::SeqCst);
        assert_eq!(
            asked_in_turn > 0,
            !alone,
            "the turn asks unless it acts alone"
        );
        w.o.run("ceo", said("zzdig the filings and zzsave").thread(&chat))
            .await
            .unwrap();
        until("work ends", || {
            lanes(&w.o).iter().all(|t| t.state.is_final()) && !lanes(&w.o).is_empty()
        })
        .await;
        assert_eq!(
            w.saves.load(Ordering::SeqCst),
            in_turn + saved,
            "the work did not match the turn: alone={alone} group={in_group}"
        );
        let asked = w.asked.load(Ordering::SeqCst);
        assert_eq!(
            asked > asked_in_turn,
            !alone,
            "the work asks where the turn asked, and not where it did not"
        );
        let origin = lanes(&w.o)[0].origin.clone();
        assert_eq!(origin, Origin::External);
    }
}

#[tokio::test]
async fn the_conversation_takes_the_chats_mode_and_is_not_listed_as_a_chat() {
    let w = world(false, false);
    ceo(&w, |s| s).await;
    w.o.run("ceo", said("hello")).await.unwrap();
    let chat = w.o.agent("ceo").unwrap().session.unwrap();
    let mut s = Session::open(w.ark.engine(), &chat).unwrap().unwrap();
    s.set_execution_mode(w.ark.engine(), 0, ExecutionMode::Manual)
        .unwrap();
    w.o.run("ceo", said("zzdig the filings")).await.unwrap();
    until("a post", || !chat_posts(&w, "ceo").is_empty()).await;
    until("idle", || {
        w.o.agent("ceo").unwrap().state == AgentState::Idle
    })
    .await;
    let lane = lanes(&w.o)[0].thread.clone().unwrap();
    let lane_session = Session::open(w.ark.engine(), &lane).unwrap().unwrap();
    assert_eq!(lane_session.execution_mode(), ExecutionMode::Manual);
    let listed: Vec<String> =
        w.o.threads("ceo")
            .unwrap()
            .into_iter()
            .map(|t| t.id)
            .collect();
    assert!(listed.contains(&chat));
    assert!(
        !listed.contains(&lane),
        "the answering conversation is listed as a chat"
    );
}

#[tokio::test]
async fn a_tool_restriction_hides_it() {
    let w = world(false, false);
    ceo(&w, |s| {
        let mut s = s;
        s.tools = vec!["doc.*".into()];
        s
    })
    .await;
    w.o.run("ceo", said("zzping")).await.unwrap();
    let seen = w.m.requests.lock().unwrap();
    assert!(!seen[0].1);
}

#[test]
fn a_post_from_before_reads_as_a_result_and_a_result_stays_as_it_was() {
    let old: Post = serde_json::from_value(json!({
        "from": "ceo", "title": "t", "text": "x", "ok": true, "at": "2026-11-01T00:00:00Z"
    }))
    .unwrap();
    assert_eq!(old.kind, PostKind::Result);
    assert!(serde_json::to_value(&old).unwrap().get("kind").is_none());
    let message = serde_json::to_value(Post::new("a", "t", "x", true).message()).unwrap();
    assert_eq!(message["kind"], "message");
}

#[tokio::test]
async fn a_restart_that_lost_the_mark_posts_the_answer_once() {
    let w = world(false, false);
    ceo(&w, |s| s).await;
    w.o.run("ceo", said("zzdig the filings")).await.unwrap();
    until("a post", || !chat_posts(&w, "ceo").is_empty()).await;
    let id = lanes(&w.o)[0].id.clone();
    until("marked", || w.o.task(&id).unwrap().reported).await;
    // The process stopped after the post was stored and before the task was marked.
    let mut rec = w
        .ark
        .engine()
        .store
        .get_record("orch_task", &id)
        .unwrap()
        .unwrap();
    rec["reported"] = json!(false);
    w.ark
        .engine()
        .store
        .put_record("orch_task", &id, w.project.id.as_str(), &rec)
        .unwrap();
    let mm = w.m.clone();
    let again = Orchestrator::builder(&w.ark, &w.project)
        .configure_models(move |b| b.provider("m", mm as Arc<dyn Provider>))
        .build()
        .unwrap();
    until("marked again", || {
        again.task(&id).is_some_and(|t| t.reported)
    })
    .await;
    assert_eq!(
        chat_posts(&w, "ceo").len(),
        1,
        "the answer was posted twice"
    );
}

#[tokio::test]
async fn an_empty_answer_is_said_not_drawn_as_an_empty_reply() {
    let w = world(false, false);
    ceo(&w, |s| s).await;
    w.o.run("ceo", said("zzdig the filings and zzempty"))
        .await
        .unwrap();
    until("a post", || !chat_posts(&w, "ceo").is_empty()).await;
    let p = &chat_posts(&w, "ceo")[0];
    assert!(!p.ok);
    assert_eq!(p.text, "It finished without an answer.");
}

#[tokio::test]
async fn a_posted_answer_is_scrubbed_of_secrets() {
    let w = world(false, false);
    ceo(&w, |s| s).await;
    w.o.run("ceo", said("zzdig the filings and zzkey"))
        .await
        .unwrap();
    until("a post", || !chat_posts(&w, "ceo").is_empty()).await;
    let p = &chat_posts(&w, "ceo")[0];
    assert_eq!(p.text, "The key is [redacted]");
    assert!(!p.text.contains("sk-ant"));
}

#[tokio::test]
async fn a_stop_before_the_hand_off_starts_no_work_and_a_stop_after_it_leaves_the_work_alone() {
    // Stop while the turn's model call is out: the hand-off finds the turn stopped.
    let w = world(true, false);
    ceo(&w, |s| s).await;
    let id = w.o.submit("ceo", said("zzslow zzdig the filings")).unwrap();
    until("the turn asked its model", || {
        !w.m.requests.lock().unwrap().is_empty()
    })
    .await;
    w.o.cancel(&id).unwrap();
    w.m.slow.notify_one();
    until("the turn ends", || w.o.task(&id).unwrap().state.is_final()).await;
    assert!(lanes(&w.o).iter().all(|t| t.state != TaskState::Running));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(chat_posts(&w, "ceo").is_empty());

    // Stop on the turn after the hand-off: the work goes on (the tile stops that).
    let w = world(true, false);
    ceo(&w, |s| s).await;
    let turn = w.o.run("ceo", said("zzdig the filings")).await.unwrap();
    let _ = w.o.cancel(&turn.id);
    let lane = lanes(&w.o)[0].id.clone();
    assert!(!w.o.task(&lane).unwrap().state.is_final());
    w.m.gate.notify_one();
    until("a post", || !chat_posts(&w, "ceo").is_empty()).await;
}
