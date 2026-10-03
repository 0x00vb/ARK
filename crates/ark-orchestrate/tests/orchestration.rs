//! Orchestration driven by scripted models: delegation, isolation, capacity, budgets,
//! lifecycle, recovery, rules, managed sessions and the graph. No model or CLI is needed.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ark_core::diagram::{validate, DiagramKind};
use ark_core::{RuleBinding, RuleScope, RuleSet};
use ark_model::capacity::Capacity;
use ark_model::session::mock::{Call, MockSessions};
use ark_model::session::{SessionBackend, SessionPolicy, SubscriptionRuntime};
use ark_model::{Completion, Message, Provider, ToolCall, ToolDef, Usage};
use ark_orchestrate::*;
use ark_sdk::prelude::*;
use serde_json::json;
use tokio::sync::Notify;

/// A scripted model that records each request and how many ran at once.
struct Script {
    id: String,
    replies: Mutex<VecDeque<Completion>>,
    seen: Mutex<Vec<(String, Vec<Message>)>>,
    now: AtomicUsize,
    peak: AtomicUsize,
    delay: Duration,
    /// When set, every call waits for a notification first.
    gate: Option<Arc<Notify>>,
    /// (start, end) of each call.
    windows: Mutex<Vec<(std::time::Instant, std::time::Instant)>>,
}

impl Script {
    fn new(id: &str, replies: Vec<Completion>) -> Arc<Self> {
        Self::with(id, replies, Duration::ZERO, None)
    }
    fn with(
        id: &str,
        replies: Vec<Completion>,
        delay: Duration,
        gate: Option<Arc<Notify>>,
    ) -> Arc<Self> {
        Arc::new(Script {
            id: id.into(),
            replies: Mutex::new(replies.into()),
            seen: Mutex::new(Vec::new()),
            now: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            delay,
            gate,
            windows: Mutex::new(Vec::new()),
        })
    }
    /// Every request's text: system prompt and messages.
    fn texts(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|(s, m)| {
                let mut t = s.clone();
                for x in m {
                    t.push('\n');
                    t.push_str(&x.content);
                }
                t
            })
            .collect()
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
        let start = std::time::Instant::now();
        let n = self.now.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(n, Ordering::SeqCst);
        self.seen
            .lock()
            .unwrap()
            .push((system.to_string(), messages.to_vec()));
        if let Some(g) = &self.gate {
            g.notified().await;
        }
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        self.now.fetch_sub(1, Ordering::SeqCst);
        self.windows
            .lock()
            .unwrap()
            .push((start, std::time::Instant::now()));
        let next = self.replies.lock().unwrap().pop_front();
        Ok(next.unwrap_or_else(|| answer("(no more script)")))
    }
    fn describe(&self) -> String {
        self.id.clone()
    }
}

fn answer(text: &str) -> Completion {
    Completion {
        content: text.into(),
        tool_calls: vec![],
        usage: Usage::measured(100, 20),
    }
}

fn calls(calls: Vec<(&str, serde_json::Value)>) -> Completion {
    Completion {
        content: String::new(),
        tool_calls: calls
            .into_iter()
            .enumerate()
            .map(|(i, (name, args))| ToolCall {
                id: format!("c{i}"),
                name: name.into(),
                arguments: args,
            })
            .collect(),
        usage: Usage::measured(100, 20),
    }
}

fn delegate(to: &str, objective: &str) -> (&'static str, serde_json::Value) {
    (
        "agents.delegate",
        json!({ "to": to, "objective": objective, "context": format!("context for {to}") }),
    )
}

struct Fixture {
    _dir: tempfile::TempDir,
    ark: Ark,
    project: Project,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(Arc::new(ark_model::MockProvider::new("unused", vec![])))
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("team", "a test").unwrap();
    Fixture {
        _dir: dir,
        ark,
        project,
    }
}

fn orch(
    f: &Fixture,
    models: Vec<(&str, Arc<Script>)>,
    caps: Vec<(&str, Capacity)>,
) -> Orchestrator {
    Orchestrator::builder(&f.ark, &f.project)
        .configure_models(move |mut b| {
            for (name, p) in models {
                b = b.provider(name, p as Arc<dyn Provider>);
            }
            for (acct, c) in caps {
                b = b.capacity(acct, c);
            }
            b
        })
        .build()
        .unwrap()
}

#[tokio::test]
async fn a_coordinator_delegates_in_parallel_and_synthesizes() {
    let f = fixture();
    let lead = Script::new(
        "lead-model",
        vec![
            calls(vec![
                delegate("reader-a", "Summarize part A"),
                delegate("reader-b", "Summarize part B"),
            ]),
            answer("Both parts agree. [inv:1]"),
        ],
    );
    let a = Script::with(
        "a-model",
        vec![answer("RESULT-A: part A is about costs")],
        Duration::from_millis(150),
        None,
    );
    let b = Script::with(
        "b-model",
        vec![answer("RESULT-B: part B is about risks")],
        Duration::from_millis(150),
        None,
    );
    let o = orch(
        &f,
        vec![
            ("lead-model", lead.clone()),
            ("a-model", a.clone()),
            ("b-model", b.clone()),
        ],
        vec![],
    );
    o.spawn(
        AgentSpec::new("lead")
            .model("lead-model")
            .delegates_to(["reader-a", "reader-b"]),
    )
    .await
    .unwrap();
    o.spawn(
        AgentSpec::new("reader-a")
            .role("reads part A")
            .model("a-model")
            .parent("lead"),
    )
    .await
    .unwrap();
    o.spawn(
        AgentSpec::new("reader-b")
            .role("reads part B")
            .model("b-model")
            .parent("lead"),
    )
    .await
    .unwrap();

    let done = o
        .run(
            "lead",
            TaskSpec::new("Compare the two parts").context("LEAD-ONLY-SECRET"),
        )
        .await
        .unwrap();
    assert_eq!(done.state, TaskState::Done, "{:?}", done.error);
    assert_eq!(done.result.as_deref(), Some("Both parts agree. [inv:1]"));
    assert_eq!(done.children.len(), 2);
    let (wa, wb) = (a.windows.lock().unwrap()[0], b.windows.lock().unwrap()[0]);
    assert!(
        wa.0 < wb.1 && wb.0 < wa.1,
        "the two delegated calls overlapped in time"
    );

    // The results came back to the coordinator as evidence.
    let lead_texts = lead.texts();
    assert_eq!(lead_texts.len(), 2);
    assert!(lead_texts[1].contains("RESULT-A") && lead_texts[1].contains("RESULT-B"));
    assert!(
        lead_texts[0].contains("reader-a: reads part A"),
        "the roster is in its instructions"
    );

    // Isolation: each reader saw its own brief only.
    let a_text = a.texts().join("\n");
    assert!(a_text.contains("Summarize part A"));
    assert!(a_text.contains("context for reader-a"));
    assert!(
        !a_text.contains("LEAD-ONLY-SECRET"),
        "not the coordinator's context"
    );
    assert!(
        !a_text.contains("Summarize part B"),
        "not the other reader's task"
    );
    assert!(!a_text.contains("RESULT-B"));
    assert!(
        !a_text.contains("agents.delegate"),
        "readers cannot delegate"
    );

    // Each agent has its own working state.
    let scope_a = ProjectId(o.agent("reader-a").unwrap().scope);
    let scope_lead = ProjectId(o.agent("lead").unwrap().scope);
    assert_ne!(scope_a, scope_lead);
    let st = f
        .ark
        .engine()
        .store
        .get_research_state(&scope_a)
        .unwrap()
        .unwrap();
    assert!(st.goal.contains("Summarize part A"), "{}", st.goal);
    assert!(f
        .ark
        .engine()
        .store
        .get_research_state(&scope_lead)
        .unwrap()
        .unwrap()
        .goal
        .contains("Compare the two parts"));

    // Every delegation is traced.
    let trace = o.task_trace(&done.id).unwrap();
    let delegated: Vec<&EventKind> = trace
        .iter()
        .map(|e| &e.kind)
        .filter(|k| matches!(k, EventKind::Delegated { .. }))
        .collect();
    assert_eq!(delegated.len(), 2);
    let child = o.task(&done.children[0]).unwrap();
    assert_eq!(child.parent.as_deref(), Some(done.id.as_str()));
    assert!(matches!(child.origin, Origin::Agent { ref agent, .. } if agent == "lead"));
    assert!(
        child.brief_tokens > 0 && child.brief_tokens < 200,
        "a small brief: {}",
        child.brief_tokens
    );

    // The graph shows it all, and exports as a valid diagram.
    let g = o.graph();
    assert_eq!(g.agents.len(), 3);
    assert_eq!(g.tasks.len(), 3);
    assert_eq!(
        g.edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Delegated)
            .count(),
        2
    );
    assert!(g
        .edges
        .iter()
        .any(|e| e.kind == EdgeKind::Parent && e.from == "agent:reader-a"));
    assert_eq!(g.agent("lead").unwrap().tasks_done, 1);
    assert!(g.seq > 0);
    let lead_node = g.task(&done.id).unwrap();
    assert_eq!(lead_node.tool_calls.len(), 2);
    assert!(lead_node.tool_calls.iter().all(|c| c.ok == Some(true)));
    let mm = g.to_mermaid();
    validate(DiagramKind::Orchestration, &mm).unwrap_or_else(|e| panic!("{e:?}\n{mm}"));
    assert!(mm.contains("-->|delegated|"));
    let json = serde_json::to_value(&g).unwrap();
    assert!(json["tasks"][0]["usage"]["prompt_tokens"]
        .as_u64()
        .is_some());
}

#[tokio::test]
async fn work_queues_for_provider_capacity() {
    let f = fixture();
    let lead = Script::new(
        "lead-model",
        vec![
            calls(vec![delegate("w1", "one"), delegate("w2", "two")]),
            answer("done [inv:1]"),
        ],
    );
    let shared = Script::with(
        "workers",
        vec![answer("r1"), answer("r2")],
        Duration::from_millis(80),
        None,
    );
    let o = orch(
        &f,
        vec![("lead-model", lead), ("workers", shared.clone())],
        vec![("workers", Capacity::concurrent(1))],
    );
    o.spawn(
        AgentSpec::new("lead")
            .model("lead-model")
            .delegates_to(["w1", "w2"]),
    )
    .await
    .unwrap();
    o.spawn(AgentSpec::new("w1").model("workers"))
        .await
        .unwrap();
    o.spawn(AgentSpec::new("w2").model("workers"))
        .await
        .unwrap();
    let mut rx = o.subscribe();
    let done = o.run("lead", TaskSpec::new("split it")).await.unwrap();
    assert_eq!(done.state, TaskState::Done);
    assert_eq!(
        shared.peak.load(Ordering::SeqCst),
        1,
        "one call at a time on that account"
    );
    let mut queued_for_capacity = false;
    while let Ok(ev) = rx.try_recv() {
        if let EventKind::TaskQueued { reason, .. } = &ev.kind {
            queued_for_capacity |= reason.starts_with("capacity: workers");
        }
    }
    assert!(queued_for_capacity, "the second worker waited, visibly");
    let acct = o
        .graph()
        .accounts
        .into_iter()
        .find(|a| a.account == "workers")
        .unwrap();
    assert_eq!(acct.max_concurrent, 1);
    assert_eq!(acct.started, 2);
}

#[tokio::test]
async fn a_parent_waiting_on_children_gives_back_its_capacity() {
    // One slot for everyone: the coordinator must release it while its child runs.
    let f = fixture();
    let lead = Script::new(
        "m",
        vec![
            calls(vec![delegate("w", "sub-task")]),
            answer("r1"),
            answer("final [inv:1]"),
        ],
    );
    let o = orch(&f, vec![("m", lead)], vec![("m", Capacity::concurrent(1))]);
    o.spawn(AgentSpec::new("lead").model("m").delegates_to(["w"]))
        .await
        .unwrap();
    o.spawn(AgentSpec::new("w").model("m")).await.unwrap();
    let done = tokio::time::timeout(Duration::from_secs(10), o.run("lead", TaskSpec::new("go")))
        .await
        .expect("no deadlock")
        .unwrap();
    assert_eq!(done.state, TaskState::Done, "{:?}", done.error);
    let trace = o.task_trace(&done.id).unwrap();
    assert!(trace
        .iter()
        .any(|e| matches!(e.kind, EventKind::TaskWaiting { .. })));
    assert!(trace
        .iter()
        .any(|e| matches!(e.kind, EventKind::TaskResumed { .. })));
}

#[tokio::test]
async fn delegating_back_up_a_waiting_chain_is_refused() {
    let f = fixture();
    let m = Script::new(
        "m",
        vec![
            calls(vec![delegate("b", "help")]),
            calls(vec![delegate("a", "help back")]),
            answer("could not ask a"),
            answer("ok [inv:1]"),
        ],
    );
    let o = orch(&f, vec![("m", m.clone())], vec![]);
    o.spawn(AgentSpec::new("a").model("m").delegates_to(["b"]))
        .await
        .unwrap();
    o.spawn(AgentSpec::new("b").model("m").delegates_to(["a"]))
        .await
        .unwrap();
    let done = tokio::time::timeout(Duration::from_secs(10), o.run("a", TaskSpec::new("x")))
        .await
        .expect("no deadlock")
        .unwrap();
    assert_eq!(done.state, TaskState::Done);
    let b_task = o.task(&done.children[0]).unwrap();
    assert!(b_task.children.is_empty(), "b's delegation was refused");
    assert_eq!(b_task.tool_calls[0].ok, Some(false));
}

#[tokio::test]
async fn a_token_budget_stops_the_task_and_its_children() {
    let f = fixture();
    let gate = Arc::new(Notify::new());
    let lead = Script::new(
        "lead",
        vec![calls(vec![delegate("w", "long work")]), answer("never")],
    );
    let w = Script::with(
        "w",
        vec![answer("late")],
        Duration::ZERO,
        Some(gate.clone()),
    );
    let o = orch(&f, vec![("lead", lead), ("w", w)], vec![]);
    o.spawn(AgentSpec::new("lead").model("lead").delegates_to(["w"]))
        .await
        .unwrap();
    o.spawn(AgentSpec::new("w").model("w")).await.unwrap();
    // 120 tokens per call: the first call passes 100, and the task stops there.
    let done = o
        .run(
            "lead",
            TaskSpec::new("x").budget(Budget::default().tokens(100)),
        )
        .await
        .unwrap();
    assert_eq!(done.state, TaskState::Failed);
    assert!(
        done.error
            .as_deref()
            .unwrap()
            .contains("budget exceeded: tokens"),
        "{:?}",
        done.error
    );
    assert!(done.children.is_empty(), "nothing ran after the limit");
    let trace = o.task_trace(&done.id).unwrap();
    assert!(trace
        .iter()
        .any(|e| matches!(e.kind, EventKind::BudgetExceeded { .. })));
    gate.notify_waiters();
}

#[tokio::test]
async fn suspend_and_resume_continue_the_task() {
    let f = fixture();
    let gate = Arc::new(Notify::new());
    let m = Script::with(
        "m",
        vec![answer("first try"), answer("finished")],
        Duration::ZERO,
        Some(gate.clone()),
    );
    let o = orch(&f, vec![("m", m.clone())], vec![]);
    o.spawn(AgentSpec::new("w").model("m")).await.unwrap();
    let id = o.submit("w", TaskSpec::new("long job")).unwrap();
    for _ in 0..100 {
        if o.task(&id).unwrap().state == TaskState::Running {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    o.suspend("w").unwrap();
    for _ in 0..100 {
        if o.task(&id).unwrap().state == TaskState::Suspended {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(o.task(&id).unwrap().state, TaskState::Suspended);
    assert_eq!(o.agent("w").unwrap().state, AgentState::Suspended);
    o.resume("w").unwrap();
    // Let the resumed call through.
    for _ in 0..50 {
        gate.notify_waiters();
        if o.task(&id).unwrap().state.is_final() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let done = o.wait(&id).await.unwrap();
    assert_eq!(done.state, TaskState::Done, "{:?}", done.error);
    assert_eq!(done.attempts, 2);
    let texts = m.texts();
    assert!(texts
        .last()
        .unwrap()
        .contains("interrupted before it finished"));
}

#[tokio::test]
async fn cancelling_a_task_cancels_what_it_delegated() {
    let f = fixture();
    let gate = Arc::new(Notify::new());
    let lead = Script::new("lead", vec![calls(vec![delegate("w", "slow")])]);
    let w = Script::with("w", vec![answer("x")], Duration::ZERO, Some(gate));
    let o = orch(&f, vec![("lead", lead), ("w", w)], vec![]);
    o.spawn(AgentSpec::new("lead").model("lead").delegates_to(["w"]))
        .await
        .unwrap();
    o.spawn(AgentSpec::new("w").model("w")).await.unwrap();
    let id = o.submit("lead", TaskSpec::new("x")).unwrap();
    for _ in 0..200 {
        if o.task(&id).unwrap().children.len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let child = o.task(&id).unwrap().children[0].clone();
    o.cancel(&id).unwrap();
    let done = o.wait(&id).await.unwrap();
    assert_eq!(done.state, TaskState::Cancelled);
    assert_eq!(o.wait(&child).await.unwrap().state, TaskState::Cancelled);
    assert_eq!(o.agent("w").unwrap().state, AgentState::Idle);
}

#[tokio::test]
async fn terminate_closes_the_agent() {
    let f = fixture();
    let o = orch(&f, vec![("m", Script::new("m", vec![]))], vec![]);
    o.spawn(AgentSpec::new("w").model("m")).await.unwrap();
    o.terminate("w").await.unwrap();
    assert_eq!(o.agent("w").unwrap().state, AgentState::Terminated);
    assert!(matches!(
        o.submit("w", TaskSpec::new("x")),
        Err(OrchError::Terminated(_))
    ));
    o.spawn(AgentSpec::new("w").model("m")).await.unwrap();
    assert_eq!(
        o.agent("w").unwrap().state,
        AgentState::Idle,
        "spawn brings it back"
    );
}

#[tokio::test]
async fn dependencies_pass_results_not_conversations() {
    let f = fixture();
    let m = Script::new("m", vec![answer("FIRST-RESULT"), answer("second done")]);
    let o = orch(&f, vec![("m", m.clone())], vec![]);
    o.spawn(AgentSpec::new("a").model("m")).await.unwrap();
    o.spawn(AgentSpec::new("b").model("m")).await.unwrap();
    let first = o.submit("a", TaskSpec::new("produce something")).unwrap();
    let second = o
        .submit("b", TaskSpec::new("use it").after([first.clone()]))
        .unwrap();
    let done = o.wait(&second).await.unwrap();
    assert_eq!(done.state, TaskState::Done);
    let b_req = m.texts().last().unwrap().clone();
    assert!(b_req.contains(&format!("result of {first} (a): FIRST-RESULT")));
    assert!(!b_req.contains("produce something"), "not a's objective");
    let g = o.graph();
    assert!(g.edges.iter().any(|e| e.kind == EdgeKind::DependsOn));
}

#[tokio::test]
async fn a_failed_dependency_fails_the_task() {
    let f = fixture();
    let o = orch(&f, vec![("m", Script::new("m", vec![]))], vec![]);
    o.spawn(AgentSpec::new("a").model("m")).await.unwrap();
    let first = o.submit("a", TaskSpec::new("x")).unwrap();
    o.cancel(&first).unwrap();
    let second = o.submit("a", TaskSpec::new("y").after([first])).unwrap();
    let done = o.wait(&second).await.unwrap();
    assert_eq!(done.state, TaskState::Failed);
}

#[tokio::test]
async fn rules_are_selected_per_task_and_traced() {
    let f = fixture();
    let book = f.ark.rule_book();
    book.put(&RuleSet::parse(
        "engineering",
        "- [E1] (always) Never commit a secret.\n- [E2] Every public function has a test. #testing\n- [E3] Migrations are reversible. #database\n",
    ))
    .unwrap();
    book.bind(&RuleBinding::new(
        "engineering",
        RuleScope::Project(f.project.id.to_string()),
    ))
    .unwrap();
    let m = Script::new("m", vec![answer("ok")]);
    let o = orch(&f, vec![("m", m.clone())], vec![]);
    o.spawn(AgentSpec::new("dev").model("m")).await.unwrap();
    let done = o
        .run("dev", TaskSpec::new("Add testing for the parser"))
        .await
        .unwrap();
    let ids: Vec<&str> = done.rules.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec!["E1", "E2"]);
    assert_eq!(done.rules_held_back, 1);
    let req = m.texts()[0].clone();
    assert!(req.contains("== RULES =="));
    assert!(req.contains("[E2] Every public function has a test."));
    assert!(
        !req.contains("Migrations are reversible"),
        "only relevant rules are sent"
    );
    let trace = o.task_trace(&done.id).unwrap();
    assert!(trace
        .iter()
        .any(|e| matches!(&e.kind, EventKind::RulesApplied { applied, .. } if applied.len() == 2)));
}

#[tokio::test]
async fn interrupted_tasks_are_marked_and_can_run_again() {
    let f = fixture();
    let m = Script::new("m", vec![answer("recovered")]);
    {
        let o = orch(&f, vec![("m", m.clone())], vec![]);
        o.spawn(AgentSpec::new("w").model("m")).await.unwrap();
    }
    // A task a dead process left running.
    let record = json!({
        "id": "t-dead0001", "project": f.project.id.to_string(), "owner": "w",
        "objective": "unfinished", "origin": { "by": "external" }, "state": "running",
        "created_ms": 1, "pid": 999_999_u32
    });
    f.ark
        .engine()
        .store
        .put_record("orch_task", "t-dead0001", f.project.id.as_str(), &record)
        .unwrap();
    let o = orch(&f, vec![("m", m.clone())], vec![]);
    let t = o.task("t-dead0001").unwrap();
    assert_eq!(t.state, TaskState::Failed);
    assert_eq!(t.error.as_deref(), Some(INTERRUPTED));
    o.spawn(AgentSpec::new("w").model("m")).await.unwrap();
    o.retry("t-dead0001").unwrap();
    let done = o.wait("t-dead0001").await.unwrap();
    assert_eq!(done.state, TaskState::Done);
    assert_eq!(done.result.as_deref(), Some("recovered"));
}

#[tokio::test]
async fn messages_reach_a_running_task_or_wait_in_the_inbox() {
    let f = fixture();
    let m = Script::new("m", vec![answer("noted")]);
    let o = orch(&f, vec![("m", m.clone())], vec![]);
    o.spawn(AgentSpec::new("w").model("m")).await.unwrap();
    assert!(!o.message("user", "w", "PLEASE-BE-BRIEF").unwrap());
    assert_eq!(o.agent("w").unwrap().inbox.len(), 1);
    o.run("w", TaskSpec::new("go")).await.unwrap();
    assert!(m.texts()[0].contains("from user: PLEASE-BE-BRIEF"));
    assert!(o.agent("w").unwrap().inbox.is_empty());
    assert!(o.graph().edges.iter().any(|e| e.kind == EdgeKind::Message));
}

#[tokio::test]
async fn tool_permissions_are_per_agent() {
    struct Two;
    #[async_trait::async_trait]
    impl Adapter for Two {
        fn name(&self) -> &str {
            "files"
        }
        fn actions(&self) -> Vec<ActionDescriptor> {
            vec![
                ActionDescriptor::new("files.read", "read", RiskTier::Read),
                ActionDescriptor::new("files.write", "write", RiskTier::Mutate),
            ]
        }
        async fn run(
            &self,
            a: &str,
            _: serde_json::Value,
            _: &RunCtx<'_>,
        ) -> ark_tools::Result<ActionOutput> {
            Ok(ActionOutput::json(json!({ "ran": a })))
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let ark = Ark::builder()
        .store_root(dir.path())
        .tool(Arc::new(Two))
        .provider(Arc::new(ark_model::MockProvider::new("unused", vec![])))
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("p", "").unwrap();
    let m = Script::new("m", vec![answer("a"), answer("b")]);
    let mm = m.clone();
    let o = Orchestrator::builder(&ark, &project)
        .configure_models(move |b| b.provider("m", mm as Arc<dyn Provider>))
        .build()
        .unwrap();
    o.spawn(AgentSpec::new("reader").model("m").tools(["files.read"]))
        .await
        .unwrap();
    o.spawn(
        AgentSpec::new("nobody")
            .model("m")
            .tools(Vec::<String>::new()),
    )
    .await
    .unwrap();
    o.run("reader", TaskSpec::new("x")).await.unwrap();
    o.run("nobody", TaskSpec::new("y")).await.unwrap();
    let t = m.texts();
    assert!(
        t[0].contains("files.read") && !t[0].contains("files.write"),
        "{}",
        t[0]
    );
    assert!(!t[1].contains("files."), "no app tools at all");
}

#[tokio::test]
async fn diagrams_are_artifacts_that_flow_back_to_the_delegator() {
    let f = fixture();
    let lead = Script::new(
        "lead",
        vec![
            calls(vec![delegate("drafter", "draw the flow")]),
            answer("here it is [inv:1]"),
        ],
    );
    let drafter = Script::new(
        "drafter",
        vec![
            calls(vec![(
                "diagram.create",
                json!({"kind": "sequence", "title": "Request flow", "source": "sequenceDiagram\n  A->>B: request\n  B-->>A: reply"}),
            )]),
            answer("stored the diagram [inv:1]"),
        ],
    );
    let o = orch(&f, vec![("lead", lead), ("drafter", drafter)], vec![]);
    o.spawn(
        AgentSpec::new("lead")
            .model("lead")
            .delegates_to(["drafter"]),
    )
    .await
    .unwrap();
    o.spawn(
        AgentSpec::new("drafter")
            .model("drafter")
            .diagrams(true)
            .tools(Vec::<String>::new()),
    )
    .await
    .unwrap();
    let done = o
        .run("lead", TaskSpec::new("document the flow"))
        .await
        .unwrap();
    assert_eq!(done.state, TaskState::Done, "{:?}", done.error);
    let child = o.task(&done.children[0]).unwrap();
    assert_eq!(child.outputs.len(), 1, "{child:?}");
    assert_eq!(child.outputs[0].kind, "diagram");
    // The delegator's project now holds it too.
    let lead_scope = ProjectId(o.agent("lead").unwrap().scope);
    let art = f
        .ark
        .engine()
        .store
        .resolve_artifact(&lead_scope, &child.outputs[0].id)
        .unwrap()
        .unwrap();
    let text = String::from_utf8(f.ark.engine().store.read_blob(&art.id).unwrap()).unwrap();
    let d = ark_core::Diagram::from_text(&text).unwrap();
    assert_eq!(d.kind, DiagramKind::Sequence);
    assert!(d.validate().is_ok());
    assert!(o.graph().edges.iter().any(|e| e.kind == EdgeKind::Produced));
}

/// A session backend factory for `fake:*` specs, sharing one scripted backend per model.
fn fake_sessions(
    backends: Vec<(&str, Arc<MockSessions>)>,
    policy: SessionPolicy,
) -> SubscriptionRuntime {
    let map: std::collections::HashMap<String, Arc<MockSessions>> = backends
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    SubscriptionRuntime::builder()
        .policy(policy)
        .backends(Arc::new(move |p: &str, m: &str| {
            if p != "fake" {
                return None;
            }
            map.get(m).map(|b| b.clone() as Arc<dyn SessionBackend>)
        }))
        .build()
}

#[tokio::test]
async fn each_agent_gets_its_own_managed_session_and_turns_are_incremental() {
    let f = fixture();
    let lead = Arc::new(MockSessions::new("fake:lead", "fake", vec![]));
    for c in [
        calls(vec![delegate("w", "part one")]),
        answer("first done [inv:1]"),
        calls(vec![delegate("w", "part two")]),
        answer("second done [inv:1]"),
    ] {
        lead.push(c);
    }
    let w = Arc::new(MockSessions::new(
        "fake:w",
        "fake",
        vec![answer("W1"), answer("W2")],
    ));
    let subs = fake_sessions(
        vec![("lead", lead.clone()), ("w", w.clone())],
        SessionPolicy::default(),
    );
    let o = Orchestrator::builder(&f.ark, &f.project)
        .configure_models(|b| b.subscriptions(subs))
        .build()
        .unwrap();
    o.spawn(
        AgentSpec::new("lead")
            .model("fake:lead")
            .delegates_to(["w"]),
    )
    .await
    .unwrap();
    o.spawn(AgentSpec::new("w").model("fake:w")).await.unwrap();
    let one = o.run("lead", TaskSpec::new("first")).await.unwrap();
    assert_eq!(one.state, TaskState::Done, "{:?}", one.error);
    let two = o.run("lead", TaskSpec::new("second")).await.unwrap();
    assert_eq!(two.state, TaskState::Done, "{:?}", two.error);

    let lead_calls = lead.calls();
    let opens = lead_calls
        .iter()
        .filter(|c| matches!(c, Call::Open { .. }))
        .count();
    let resumes = lead_calls
        .iter()
        .filter(|c| matches!(c, Call::Resume { .. }))
        .count();
    assert_eq!(
        opens, 1,
        "one vendor session for the coordinator: {lead_calls:?}"
    );
    assert_eq!(resumes, 3, "every later call continues it");
    // The second task's first call sends the new brief, not the first task again.
    let Call::Resume { delta, .. } = &lead_calls[2] else {
        panic!()
    };
    let sent: String = delta.messages.iter().map(|m| m.content.clone()).collect();
    assert!(sent.contains("second"));
    assert!(
        !sent.contains("== OBJECTIVE ==\nfirst"),
        "the first brief is not resent: {sent}"
    );

    let g = o.graph();
    let ls = g.agent("lead").unwrap().session.clone().unwrap();
    let ws = g.agent("w").unwrap().session.clone().unwrap();
    assert_ne!(ls.vendor, ws.vendor, "never a shared vendor session");
    assert_eq!(ls.opens, 1);
    assert!(ls.saved_tokens > 0, "incremental turns sent less: {ls:?}");
    // Session events are part of the trace.
    let evs = o.events(0, 10_000).unwrap();
    assert!(evs
        .iter()
        .any(|e| matches!(&e.kind, EventKind::Session { agent, .. } if agent == "lead")));
}

#[tokio::test]
async fn a_vendor_session_rotates_from_arks_request() {
    let f = fixture();
    let w = Arc::new(MockSessions::new("fake:w", "fake", vec![]));
    for i in 0..3 {
        w.push(Completion {
            content: format!("answer {i}"),
            tool_calls: vec![],
            usage: Usage::measured(900, 10),
        });
    }
    let subs = fake_sessions(
        vec![("w", w.clone())],
        SessionPolicy {
            max_context_tokens: 950,
            ..Default::default()
        },
    );
    let o = Orchestrator::builder(&f.ark, &f.project)
        .configure_models(|b| b.subscriptions(subs))
        .build()
        .unwrap();
    o.spawn(AgentSpec::new("w").model("fake:w")).await.unwrap();
    for t in ["one", "two", "three"] {
        let done = o.run("w", TaskSpec::new(t)).await.unwrap();
        assert_eq!(done.state, TaskState::Done);
    }
    let snap = o.graph().agent("w").unwrap().session.clone().unwrap();
    assert!(snap.rotations >= 1, "{snap:?}");
    // A rotation opens a new vendor session with ARK's bounded request, not the vendor's history.
    let opens = w
        .calls()
        .iter()
        .filter(|c| matches!(c, Call::Open { .. }))
        .count();
    assert!(opens >= 2, "{:?}", w.calls());
}

#[tokio::test]
async fn the_harness_roles_use_the_agents_own_models() {
    // Planner and reasoning go to the agent's model, never another agent's.
    let f = fixture();
    let a = Script::new(
        "a",
        vec![answer("plan step one\nplan step two"), answer("A done")],
    );
    let b = Script::new("b", vec![answer("B done")]);
    let o = orch(&f, vec![("a", a.clone()), ("b", b.clone())], vec![]);
    o.spawn(AgentSpec::new("a").model("a").planner(true))
        .await
        .unwrap();
    o.spawn(AgentSpec::new("b").model("b")).await.unwrap();
    let x = o
        .submit(
            "a",
            TaskSpec::new("investigate why the cache misses and explain"),
        )
        .unwrap();
    let y = o.submit("b", TaskSpec::new("y")).unwrap();
    assert_eq!(o.wait(&x).await.unwrap().state, TaskState::Done);
    assert_eq!(o.wait(&y).await.unwrap().state, TaskState::Done);
    assert_eq!(
        a.seen.lock().unwrap().len(),
        2,
        "plan + reason on a's model"
    );
    assert_eq!(b.seen.lock().unwrap().len(), 1);
}
