//! An agent's own conversations (`TaskSpec::thread`): they run at the same time, each queues
//! its own tasks, the agent's state follows what runs, steering and late mail stay in their
//! conversation, and a restart keeps what runs. Driven by a scripted model.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ark_model::{Completion, Message, Provider, ToolDef, Usage};
use ark_orchestrate::*;
use ark_sdk::prelude::*;
use serde_json::json;
use tokio::sync::Notify;

/// A model whose every call waits for the gate, then answers from its script.
struct Script {
    replies: Mutex<VecDeque<Completion>>,
    seen: Mutex<Vec<String>>,
    now: AtomicUsize,
    peak: AtomicUsize,
    gate: Arc<Notify>,
}

impl Script {
    fn new(gate: &Arc<Notify>) -> Arc<Self> {
        Arc::new(Script {
            replies: Mutex::new(VecDeque::new()),
            seen: Mutex::new(Vec::new()),
            now: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            gate: gate.clone(),
        })
    }
    fn texts(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
    fn in_flight(&self) -> usize {
        self.now.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl Provider for Script {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        _tools: &[ToolDef],
    ) -> ark_model::Result<Completion> {
        let n = self.now.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(n, Ordering::SeqCst);
        let mut text = system.to_string();
        for m in messages {
            text.push('\n');
            text.push_str(&m.content);
        }
        self.seen.lock().unwrap().push(text);
        self.gate.notified().await;
        self.now.fetch_sub(1, Ordering::SeqCst);
        let next = self.replies.lock().unwrap().pop_front();
        Ok(next.unwrap_or_else(|| Completion {
            content: "done".into(),
            tool_calls: vec![],
            usage: Usage::measured(100, 20),
        }))
    }
    fn describe(&self) -> String {
        "m".into()
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    ark: Ark,
    project: Project,
    gate: Arc<Notify>,
    model: Arc<Script>,
    o: Orchestrator,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(Arc::new(ark_model::MockProvider::new("unused", vec![])))
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("team", "a test").unwrap();
    let gate = Arc::new(Notify::new());
    let model = Script::new(&gate);
    let o = orchestrator(&ark, &project, &model);
    o.spawn(AgentSpec::new("w").model("m")).await.unwrap();
    Fixture {
        _dir: dir,
        ark,
        project,
        gate,
        model,
        o,
    }
}

fn orchestrator(ark: &Ark, project: &Project, model: &Arc<Script>) -> Orchestrator {
    let model = model.clone();
    Orchestrator::builder(ark, project)
        .configure_models(move |b| b.provider("m", model as Arc<dyn Provider>))
        .build()
        .unwrap()
}

async fn until(what: &str, mut ok: impl FnMut() -> bool) {
    for _ in 0..300 {
        if ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

/// Let calls through until every task in `ids` has ended.
async fn release(f: &Fixture, ids: &[&str]) {
    for _ in 0..300 {
        f.gate.notify_waiters();
        if ids.iter().all(|id| f.o.task(id).unwrap().state.is_final()) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("tasks did not end");
}

#[tokio::test]
async fn two_conversations_of_one_agent_run_at_once_and_leave_its_default_alone() {
    let f = fixture().await;
    let (one, two) = (
        f.o.start_thread("w").unwrap(),
        f.o.start_thread("w").unwrap(),
    );
    let a =
        f.o.submit("w", TaskSpec::new("job one").thread(&one))
            .unwrap();
    let b =
        f.o.submit("w", TaskSpec::new("job two").thread(&two))
            .unwrap();
    until("both conversations to call the model", || {
        f.model.in_flight() == 2
    })
    .await;
    let agent = f.o.agent("w").unwrap();
    assert_eq!(agent.state, AgentState::Running);
    assert_eq!(agent.running.len(), 2);
    assert_eq!(agent.current_task.as_ref(), agent.running.first());
    let g = f.o.graph();
    let node = g.agent("w").unwrap();
    assert_eq!(node.running.len(), 2);
    assert!(g
        .tasks
        .iter()
        .any(|t| t.task == a && t.thread.as_deref() == Some(one.as_str())));
    release(&f, &[&a, &b]).await;
    for id in [&a, &b] {
        assert_eq!(f.o.wait(id).await.unwrap().state, TaskState::Done);
    }
    let agent = f.o.agent("w").unwrap();
    assert_eq!(agent.state, AgentState::Idle);
    assert!(agent.running.is_empty() && agent.current_task.is_none());
    assert_eq!(
        agent.session, None,
        "a conversation task leaves the default alone"
    );
    let rows = f.o.threads("w").unwrap();
    assert_eq!(rows.len(), 2, "both have a turn now: {rows:?}");
    assert!(rows
        .iter()
        .all(|r| !r.default && r.turns == 1 && r.running.is_none()));
    // A chat is named by what the person asked in it, not by the brief the agent was given.
    let mut titles: Vec<_> = rows.iter().map(|r| r.title.as_str()).collect();
    titles.sort();
    assert_eq!(titles, ["job one", "job two"]);
}

#[tokio::test]
async fn one_conversation_queues_its_own_tasks() {
    let f = fixture().await;
    let one = f.o.start_thread("w").unwrap();
    let a =
        f.o.submit("w", TaskSpec::new("first").thread(&one))
            .unwrap();
    let b =
        f.o.submit("w", TaskSpec::new("second").thread(&one))
            .unwrap();
    until("the first to run", || f.model.in_flight() == 1).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(f.model.in_flight(), 1);
    let queued = f.o.task(&b).unwrap();
    assert_eq!(queued.state, TaskState::Queued);
    assert_eq!(queued.waiting_on.as_deref(), Some("agent w is busy"));
    let rows = f.o.threads("w").unwrap();
    assert_eq!(
        rows.len(),
        0,
        "no turn recorded yet, and it is not the default"
    );
    release(&f, &[&a, &b]).await;
    assert_eq!(f.model.peak.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn the_default_conversation_and_work_still_queue_together_beside_a_thread() {
    let f = fixture().await;
    let d = f.o.submit("w", TaskSpec::new("default job")).unwrap();
    until("the default task to run", || f.model.in_flight() == 1).await;
    let receipt =
        f.o.admit_work(
            WorkTarget::Agent { name: "w".into() },
            TaskSpec::new("work job"),
            "desktop",
            "one",
        )
        .await
        .unwrap();
    f.o.execute_work(&receipt.id).unwrap();
    let work = receipt.task.clone().unwrap();
    let one = f.o.start_thread("w").unwrap();
    let t =
        f.o.submit("w", TaskSpec::new("thread job").thread(&one))
            .unwrap();
    until("the thread to run beside the default", || {
        f.model.in_flight() == 2
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        f.model.in_flight(),
        2,
        "Work waits for the default conversation"
    );
    assert_eq!(
        f.o.task(&work).unwrap().waiting_on.as_deref(),
        Some("agent w is busy")
    );
    // A Work conversation is no conversation of the agent's own.
    assert!(matches!(
        f.o.submit("w", TaskSpec::new("x").thread(&receipt.conversation)),
        Err(OrchError::NotAllowed(_))
    ));
    release(&f, &[&d, &t]).await;
    for _ in 0..300 {
        f.gate.notify_waiters();
        if f.o.task(&work).unwrap().state.is_final() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(f.o.task(&work).unwrap().state, TaskState::Done);
}

#[tokio::test]
async fn cancelling_one_conversation_leaves_the_other_running() {
    let f = fixture().await;
    let (one, two) = (
        f.o.start_thread("w").unwrap(),
        f.o.start_thread("w").unwrap(),
    );
    let a =
        f.o.submit("w", TaskSpec::new("job one").thread(&one))
            .unwrap();
    let b =
        f.o.submit("w", TaskSpec::new("job two").thread(&two))
            .unwrap();
    until("both to run", || f.model.in_flight() == 2).await;
    f.o.cancel(&a).unwrap();
    assert_eq!(f.o.wait(&a).await.unwrap().state, TaskState::Cancelled);
    let agent = f.o.agent("w").unwrap();
    assert_eq!(agent.state, AgentState::Running, "the other still works");
    assert_eq!(agent.running, vec![b.clone()]);
    assert_eq!(f.o.task(&b).unwrap().state, TaskState::Running);
    release(&f, &[&b]).await;
    assert_eq!(f.o.agent("w").unwrap().state, AgentState::Idle);
}

#[tokio::test]
async fn suspend_stops_every_conversation_and_resume_runs_them_again() {
    let f = fixture().await;
    let (one, two) = (
        f.o.start_thread("w").unwrap(),
        f.o.start_thread("w").unwrap(),
    );
    let a =
        f.o.submit("w", TaskSpec::new("job one").thread(&one))
            .unwrap();
    let b =
        f.o.submit("w", TaskSpec::new("job two").thread(&two))
            .unwrap();
    until("both to run", || f.model.in_flight() == 2).await;
    f.o.suspend("w").unwrap();
    until("both to be suspended", || {
        [&a, &b]
            .iter()
            .all(|id| f.o.task(id).unwrap().state == TaskState::Suspended)
    })
    .await;
    assert_eq!(f.o.agent("w").unwrap().state, AgentState::Suspended);
    f.o.resume("w").unwrap();
    release(&f, &[&a, &b]).await;
    for id in [&a, &b] {
        assert_eq!(f.o.task(id).unwrap().state, TaskState::Done);
    }
    assert_eq!(f.o.agent("w").unwrap().state, AgentState::Idle);
}

#[tokio::test]
async fn terminate_ends_every_conversation() {
    let f = fixture().await;
    let (one, two) = (
        f.o.start_thread("w").unwrap(),
        f.o.start_thread("w").unwrap(),
    );
    let a =
        f.o.submit("w", TaskSpec::new("job one").thread(&one))
            .unwrap();
    let b =
        f.o.submit("w", TaskSpec::new("job two").thread(&two))
            .unwrap();
    until("both to run", || f.model.in_flight() == 2).await;
    f.o.terminate("w").await.unwrap();
    for id in [&a, &b] {
        assert_eq!(f.o.wait(id).await.unwrap().state, TaskState::Cancelled);
    }
    let agent = f.o.agent("w").unwrap();
    assert_eq!(agent.state, AgentState::Terminated);
    assert!(agent.running.is_empty());
}

#[tokio::test]
async fn a_message_steers_only_its_own_conversation() {
    let f = fixture().await;
    let (one, two) = (
        f.o.start_thread("w").unwrap(),
        f.o.start_thread("w").unwrap(),
    );
    let a =
        f.o.submit("w", TaskSpec::new("job one").thread(&one))
            .unwrap();
    let b =
        f.o.submit("w", TaskSpec::new("job two").thread(&two))
            .unwrap();
    until("both to run", || f.model.in_flight() == 2).await;
    assert!(f
        .o
        .message_thread("user", "w", Some(&one), "STEER-ONE", "")
        .unwrap());
    // No conversation named: the default one, which runs nothing, so it waits.
    assert!(!f.o.message("user", "w", "FOR-DEFAULT").unwrap());
    release(&f, &[&a, &b]).await;
    let texts = f.model.texts();
    let steered: Vec<&String> = texts.iter().filter(|t| t.contains("STEER-ONE")).collect();
    assert!(!steered.is_empty(), "the message reached a running task");
    assert!(steered
        .iter()
        .all(|t| t.contains("job one") && !t.contains("job two")));
    assert!(texts.iter().all(|t| !t.contains("FOR-DEFAULT")));
    let inbox = f.o.agent("w").unwrap().inbox;
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].thread, None);
}

/// The `thread` of each stored message event to `w`, in order.
fn message_threads(f: &Fixture) -> Vec<(String, Option<String>)> {
    f.o.events(0, 10_000)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.kind {
            EventKind::Message { text, thread, .. } => Some((text, thread)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_message_event_names_the_conversation_it_went_to() {
    let f = fixture().await;
    let one = f.o.start_thread("w").unwrap();
    let a =
        f.o.submit("w", TaskSpec::new("job one").thread(&one))
            .unwrap();
    until("it runs", || f.model.in_flight() == 1).await;
    assert!(f
        .o
        .message_thread("user", "w", Some(&one), "IN-ONE", "")
        .unwrap());
    release(&f, &[&a]).await;
    assert!(!f
        .o
        .message_thread("user", "w", Some(&one), "LATER-ONE", "")
        .unwrap());
    assert!(!f.o.message("user", "w", "IN-DEFAULT").unwrap());
    assert_eq!(
        message_threads(&f),
        vec![
            ("IN-ONE".to_string(), Some(one.clone())),
            ("LATER-ONE".to_string(), Some(one.clone())),
            ("IN-DEFAULT".to_string(), None),
        ]
    );
}

#[tokio::test]
async fn a_message_naming_no_conversation_steers_the_one_task_that_runs() {
    let f = fixture().await;
    f.o.spawn(AgentSpec::new("lead").model("m")).await.unwrap();
    let one = f.o.start_thread("w").unwrap();
    let a =
        f.o.submit("w", TaskSpec::new("job one").thread(&one))
            .unwrap();
    until("it runs", || f.model.in_flight() == 1).await;
    // The person names no chat: the note stays in the default one, which every client shows.
    assert!(
        !f.o.message("user", "w", "FROM-PERSON").unwrap(),
        "the person's note waits for the default chat"
    );
    // Another agent names no chat: the only running task takes it, as before chats.
    assert!(
        f.o.message("lead", "w", "FROM-LEAD").unwrap(),
        "the only running task takes it"
    );
    release(&f, &[&a]).await;
    assert!(f
        .model
        .texts()
        .iter()
        .any(|t| t.contains("FROM-LEAD") && t.contains("job one")));
    assert!(!f.model.texts().iter().any(|t| t.contains("FROM-PERSON")));
    let inbox = f.o.agent("w").unwrap().inbox;
    assert_eq!(inbox.len(), 1);
    assert_eq!(
        (inbox[0].text.as_str(), inbox[0].thread.as_deref()),
        ("FROM-PERSON", None)
    );
    assert_eq!(
        message_threads(&f),
        vec![
            ("FROM-PERSON".to_string(), None),
            ("FROM-LEAD".to_string(), Some(one))
        ]
    );
}

#[tokio::test]
async fn a_message_names_only_the_agents_own_conversations() {
    let f = fixture().await;
    f.o.spawn(AgentSpec::new("v").model("m")).await.unwrap();
    let general = Session::start(f.ark.engine(), &f.project.id).unwrap();
    let others = f.o.start_thread("v").unwrap();
    for foreign in [general.id().to_string(), others, "s-missing".to_string()] {
        assert!(
            matches!(
                f.o.message_thread("user", "w", Some(&foreign), "x", ""),
                Err(OrchError::NotAllowed(_))
            ),
            "{foreign} is refused"
        );
    }
    assert!(f.o.agent("w").unwrap().inbox.is_empty());
}

#[tokio::test]
async fn mail_for_a_conversation_waits_for_that_conversations_next_task() {
    let f = fixture().await;
    let (one, two) = (
        f.o.start_thread("w").unwrap(),
        f.o.start_thread("w").unwrap(),
    );
    assert!(!f
        .o
        .message_thread("user", "w", Some(&one), "NOTE-FOR-ONE", "")
        .unwrap());
    assert_eq!(
        f.o.agent("w").unwrap().inbox[0].thread.as_deref(),
        Some(one.as_str())
    );
    let other =
        f.o.submit("w", TaskSpec::new("in two").thread(&two))
            .unwrap();
    release(&f, &[&other]).await;
    let default = f.o.submit("w", TaskSpec::new("in default")).unwrap();
    release(&f, &[&default]).await;
    assert!(f.model.texts().iter().all(|t| !t.contains("NOTE-FOR-ONE")));
    assert_eq!(f.o.agent("w").unwrap().inbox.len(), 1);
    let mine =
        f.o.submit("w", TaskSpec::new("in one").thread(&one))
            .unwrap();
    release(&f, &[&mine]).await;
    let texts = f.model.texts();
    let with_note: Vec<&String> = texts
        .iter()
        .filter(|t| t.contains("NOTE-FOR-ONE"))
        .collect();
    assert!(!with_note.is_empty());
    assert!(with_note.iter().all(|t| t.contains("in one")));
    assert!(f.o.agent("w").unwrap().inbox.is_empty());
}

#[tokio::test]
async fn only_the_agents_own_conversations_are_threads() {
    let f = fixture().await;
    f.o.spawn(AgentSpec::new("v").model("m")).await.unwrap();
    let engine = f.ark.engine();
    let general = Session::start(engine, &f.project.id).unwrap();
    let others = f.o.start_thread("v").unwrap();
    for foreign in [general.id().to_string(), others, "s-missing".to_string()] {
        assert!(
            matches!(
                f.o.submit("w", TaskSpec::new("x").thread(&foreign)),
                Err(OrchError::NotAllowed(_))
            ),
            "{foreign} is refused"
        );
    }
    let own = f.o.start_thread("w").unwrap();
    let id = f.o.submit("w", TaskSpec::new("x").thread(&own)).unwrap();
    release(&f, &[&id]).await;
    // Its default conversation as a thread runs on the default lane and stays the default.
    let first = f.o.submit("w", TaskSpec::new("default")).unwrap();
    release(&f, &[&first]).await;
    let session = f.o.agent("w").unwrap().session.unwrap();
    let again =
        f.o.submit("w", TaskSpec::new("again").thread(&session))
            .unwrap();
    release(&f, &[&again]).await;
    assert_eq!(f.o.agent("w").unwrap().session, Some(session.clone()));
    let rows = f.o.threads("w").unwrap();
    assert!(rows
        .iter()
        .any(|r| r.id == session && r.default && r.turns == 2));
}

#[tokio::test]
async fn idempotent_submission_names_its_conversation() {
    let f = fixture().await;
    let one = f.o.start_thread("w").unwrap();
    let first =
        f.o.submit_once("w", TaskSpec::new("hi").thread(&one), "r1")
            .await
            .unwrap();
    let retry =
        f.o.submit_once("w", TaskSpec::new("hi").thread(&one), "r1")
            .await
            .unwrap();
    assert_eq!(first, retry);
    assert!(f
        .o
        .submit_once("w", TaskSpec::new("hi"), "r1")
        .await
        .is_err());
    release(&f, &[&first]).await;
}

/// A process other than this one, alive for the test.
struct Other(std::process::Child);

impl Drop for Other {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn store_task(f: &Fixture, id: &str, state: &str, pid: Option<u32>) {
    let task = json!({
        "id": id,
        "project": f.project.id.as_str(),
        "owner": "w",
        "objective": "held",
        "origin": { "by": "external" },
        "state": state,
        "attempts": 1,
        "created_ms": 1,
        "started_ms": 1,
        "execution_owner": "elsewhere",
        "pid": pid,
    });
    f.ark
        .engine()
        .store
        .put_record("orch_task", id, f.project.id.as_str(), &task)
        .unwrap();
}

fn store_agent(f: &Fixture, change: impl FnOnce(&mut serde_json::Value)) {
    let key = format!("{}/w", f.project.id);
    let store = &f.ark.engine().store;
    let mut agent = store.get_record("orch_agent", &key).unwrap().unwrap();
    change(&mut agent);
    store
        .put_record("orch_agent", &key, f.project.id.as_str(), &agent)
        .unwrap();
}

#[tokio::test]
async fn a_restart_keeps_what_runs_from_an_old_record_and_from_two_conversations() {
    let f = fixture().await;
    let other = Other(
        std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap(),
    );
    let pid = other.0.id();
    store_task(&f, "t-live1", "running", Some(pid));
    store_task(&f, "t-live2", "running", Some(pid));
    // A record written before `running`: its one task is in `current_task`.
    store_agent(&f, |a| {
        a.as_object_mut().unwrap().remove("running");
        a["current_task"] = json!("t-live1");
        a["state"] = json!("running");
    });
    let reloaded = orchestrator(&f.ark, &f.project, &f.model);
    let agent = reloaded.agent("w").unwrap();
    assert_eq!(agent.running, vec!["t-live1".to_string()]);
    assert_eq!(agent.current_task.as_deref(), Some("t-live1"));
    assert_eq!(agent.state, AgentState::Running);
    drop(reloaded);

    // Two conversations running in another process, and one that ended without saying so.
    store_task(&f, "t-gone", "done", None);
    store_agent(&f, |a| {
        a["running"] = json!(["t-live1", "t-gone", "t-live2"]);
        a["current_task"] = json!("t-live1");
        a["state"] = json!("running");
    });
    let reloaded = orchestrator(&f.ark, &f.project, &f.model);
    let agent = reloaded.agent("w").unwrap();
    assert_eq!(
        agent.running,
        vec!["t-live1".to_string(), "t-live2".to_string()]
    );
    assert_eq!(agent.state, AgentState::Running);
}

#[tokio::test]
async fn each_conversation_gets_its_own_vendor_session_and_releases_it_when_its_task_ends() {
    use ark_model::session::mock::{Call, MockSessions};
    use ark_model::session::{SessionBackend, SessionPolicy, SubscriptionRuntime};
    let dir = tempfile::tempdir().unwrap();
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(Arc::new(ark_model::MockProvider::new("unused", vec![])))
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("team", "a test").unwrap();
    let answer = |t: &str| Completion {
        content: t.into(),
        tool_calls: vec![],
        usage: Usage::measured(100, 20),
    };
    let w = Arc::new(MockSessions::new(
        "fake:w",
        "fake",
        vec![answer("one"), answer("two"), answer("default")],
    ));
    let backend = w.clone();
    let subs = SubscriptionRuntime::builder()
        .policy(SessionPolicy::default())
        .backends(Arc::new(move |p: &str, m: &str| {
            (p == "fake" && m == "w").then(|| backend.clone() as Arc<dyn SessionBackend>)
        }))
        .build();
    let o = Orchestrator::builder(&ark, &project)
        .configure_models(|b| b.subscriptions(subs))
        .build()
        .unwrap();
    o.spawn(AgentSpec::new("w").model("fake:w")).await.unwrap();
    for thread in [o.start_thread("w").unwrap(), o.start_thread("w").unwrap()] {
        let done = o
            .run("w", TaskSpec::new("x").thread(&thread))
            .await
            .unwrap();
        assert_eq!(done.state, TaskState::Done, "{:?}", done.error);
    }
    let done = o.run("w", TaskSpec::new("x")).await.unwrap();
    assert_eq!(done.state, TaskState::Done, "{:?}", done.error);
    let calls = w.calls();
    let opened: std::collections::HashSet<&String> = calls
        .iter()
        .filter_map(|c| match c {
            Call::Open { vendor, .. } => Some(vendor),
            _ => None,
        })
        .collect();
    assert_eq!(
        opened.len(),
        3,
        "one vendor session per conversation: {calls:?}"
    );
    let discarded = calls
        .iter()
        .filter(|c| matches!(c, Call::Discard { .. }))
        .count();
    assert_eq!(
        discarded, 2,
        "each conversation's session ends with its task: {calls:?}"
    );
    assert!(
        o.graph().agents[0].session.is_some(),
        "the default conversation keeps its session"
    );
}
