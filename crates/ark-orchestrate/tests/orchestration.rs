//! Orchestration driven by scripted models: delegation, isolation, capacity, budgets,
//! lifecycle, recovery, rules, managed sessions and the graph. No model or CLI is needed.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ark_core::{RuleBinding, RuleScope, RuleSet};
use ark_model::capacity::Capacity;
use ark_model::session::mock::{Call, MockSessions};
use ark_model::session::{SessionBackend, SessionPolicy, SubscriptionRuntime};
use ark_model::{Completion, Message, Provider, ToolCall, ToolDef, Usage};
use ark_orchestrate::*;
use ark_sdk::prelude::*;
use ark_tools::diagram::{validate, DiagramKind};
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
    let reader_a_task = done
        .children
        .iter()
        .find(|id| {
            o.task(id)
                .is_some_and(|t| t.objective.contains("Summarize part A"))
        })
        .unwrap()
        .clone();
    let st = f
        .ark
        .engine()
        .store
        .get_research_state(&scope_a, &reader_a_task)
        .unwrap()
        .unwrap();
    assert!(st.goal.contains("Summarize part A"), "{}", st.goal);
    assert!(f
        .ark
        .engine()
        .store
        .get_research_state(&scope_lead, &done.id)
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
async fn a_named_profile_is_chosen_by_name_and_survives_a_restart() {
    let f = fixture();
    let m = Script::new("m", vec![answer("one"), answer("two")]);
    let open = || {
        Orchestrator::builder(&f.ark, &f.project)
            .configure_models({
                let m = m.clone();
                move |b| b.provider("m", m as Arc<dyn Provider>)
            })
            .profile(
                "terse",
                "answers in one line",
                AgentProfile::builder()
                    .instructions("PROFILE-TERSE: answer in one line.")
                    .build(),
            )
            .build()
            .unwrap()
    };
    {
        let o = open();
        assert_eq!(o.profiles()[0].name, "terse");
        let err = o
            .spawn(AgentSpec::new("x").model("m").profile_named("nope"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("offered: terse"), "{err}");
        o.spawn(AgentSpec::new("w").model("m").profile_named("terse"))
            .await
            .unwrap();
        assert_eq!(o.graph().agents[0].profile.as_deref(), Some("terse"));
        o.run("w", TaskSpec::new("first")).await.unwrap();
    }
    // A new process: the agent comes back from the store with its profile.
    let o = open();
    o.run("w", TaskSpec::new("second")).await.unwrap();
    let texts = m.texts();
    assert_eq!(texts.len(), 2);
    assert!(
        texts.iter().all(|t| t.contains("PROFILE-TERSE")),
        "{texts:?}"
    );
}

/// A state-changing tool that counts its runs: a change must happen once, whatever stops.
struct Change(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Adapter for Change {
    fn name(&self) -> &str {
        "doc"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new("doc.save", "save", RiskTier::Mutate)]
    }
    async fn run(
        &self,
        _: &str,
        input: serde_json::Value,
        _: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ActionOutput::json(json!({ "saved": input["text"] })))
    }
}

/// An app with the counting tool, kept across "restarts": each `orch()` is the orchestrator a
/// new process builds over the same store.
struct Restarts {
    _dir: tempfile::TempDir,
    ark: Ark,
    project: Project,
    runs: Arc<AtomicUsize>,
}

impl Restarts {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let runs = Arc::new(AtomicUsize::new(0));
        let ark = Ark::builder()
            .store_root(dir.path())
            .tool(Arc::new(Change(runs.clone())))
            .provider(Arc::new(ark_model::MockProvider::new("unused", vec![])))
            .planner(false)
            .critic(false)
            .build()
            .unwrap();
        let project = ark.create_project("p", "").unwrap();
        Restarts {
            _dir: dir,
            ark,
            project,
            runs,
        }
    }
    fn orch(&self, m: &Arc<Script>, gate: Option<Arc<ApprovalGate>>) -> Orchestrator {
        let mm = m.clone();
        let mut b = Orchestrator::builder(&self.ark, &self.project)
            .configure_models(move |b| b.provider("m", mm as Arc<dyn Provider>));
        if let Some(g) = gate {
            b = b.approvals(g);
        }
        b.build().unwrap()
    }
    /// Store a task as a process that died left it.
    fn left_open(&self, id: &str, state: &str, extra: serde_json::Value) {
        let mut record = json!({
            "id": id, "project": self.project.id.to_string(), "owner": "w",
            "objective": "unfinished", "origin": { "by": "external" }, "state": state,
            "attempts": 1, "created_ms": 1, "started_ms": 1, "pid": 999_999_u32
        });
        for (k, v) in extra.as_object().unwrap() {
            record[k] = v.clone();
        }
        self.ark
            .engine()
            .store
            .put_record("orch_task", id, self.project.id.as_str(), &record)
            .unwrap();
    }
    fn scope(&self, o: &Orchestrator) -> ProjectId {
        ProjectId(o.agent("w").unwrap().scope)
    }
}

/// An approver nobody answers, counting the questions it gets.
struct Nobody(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Approver for Nobody {
    async fn decide(&self, _: &ApprovalRequest) -> ApprovalDecision {
        self.0.fetch_add(1, Ordering::SeqCst);
        std::future::pending().await
    }
}

#[tokio::test]
async fn work_a_stopped_process_left_open_continues_after_a_restart() {
    let r = Restarts::new();
    let m = Script::new("m", vec![answer("recovered"), answer("fresh")]);
    {
        let o = r.orch(&m, None);
        o.spawn(AgentSpec::new("w").model("m")).await.unwrap();
        o.spawn(AgentSpec::new("v").model("m")).await.unwrap();
    }
    // One was running, one never started.
    r.left_open("t-dead0001", "running", json!({}));
    r.left_open(
        "t-dead0002",
        "queued",
        json!({ "owner": "v", "attempts": 0, "started_ms": null }),
    );
    let o = r.orch(&m, None);
    let done = o.wait("t-dead0001").await.unwrap();
    assert_eq!(done.state, TaskState::Done, "{:?}", done.error);
    let fresh = o.wait("t-dead0002").await.unwrap();
    assert_eq!(fresh.state, TaskState::Done, "{:?}", fresh.error);
    let texts = m.texts();
    let briefs: Vec<&String> = texts.iter().filter(|t| t.contains("unfinished")).collect();
    assert_eq!(briefs.len(), 2);
    // Only the one that had run is told it continues.
    assert_eq!(
        briefs
            .iter()
            .filter(|t| t.contains("It was interrupted"))
            .count(),
        1
    );
    let events = o.events(0, 1000).unwrap();
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        EventKind::TaskQueued { task, reason } if task == "t-dead0001" && reason.contains("restart")
    )));
}

#[tokio::test]
async fn a_change_cut_short_by_a_restart_is_not_run_again_on_its_own() {
    let r = Restarts::new();
    let m = Script::new("m", vec![answer("ran again")]);
    let scope = {
        let o = r.orch(&m, None);
        o.spawn(AgentSpec::new("w").model("m").allow_mutate(true))
            .await
            .unwrap();
        r.scope(&o)
    };
    r.left_open("t-dead0003", "running", json!({ "report_to": null }));
    // The engine recorded the call as running, in a process that is gone.
    let mut inv = ark_core::Invocation::new("doc.save", RiskTier::Mutate, json!({"text": "a"}));
    inv.status = ark_core::InvocationStatus::Running;
    inv.pid = Some(999_999);
    r.ark.engine().store.put_invocation(&scope, &inv).unwrap();

    let o = r.orch(&m, None);
    let t = o.task("t-dead0003").unwrap();
    assert_eq!(t.state, TaskState::Failed);
    let error = t.error.unwrap();
    assert!(
        error.starts_with(INTERRUPTED) && error.contains("doc.save"),
        "{error}"
    );
    assert!(m.texts().is_empty(), "nothing ran on its own");
    // The person checks, then runs it again.
    o.retry("t-dead0003").unwrap();
    assert_eq!(o.wait("t-dead0003").await.unwrap().state, TaskState::Done);

    // Run again and stopped again with no change under way: the old cut-short call is not
    // this run's, so it continues.
    let m2 = Script::new("m2", vec![answer("third time")]);
    drop(o);
    let mut rec = r
        .ark
        .engine()
        .store
        .get_record("orch_task", "t-dead0003")
        .unwrap()
        .unwrap();
    rec["state"] = json!("running");
    rec["pid"] = json!(999_999);
    r.ark
        .engine()
        .store
        .put_record("orch_task", "t-dead0003", r.project.id.as_str(), &rec)
        .unwrap();
    let o = r.orch(&m2, None);
    let t = o.wait("t-dead0003").await.unwrap();
    assert_eq!(t.state, TaskState::Done, "{:?}", t.error);
}

#[tokio::test]
async fn a_change_made_before_a_restart_is_not_made_twice() {
    let r = Restarts::new();
    let m = Script::new(
        "m",
        vec![
            // The resumed run makes the same change again, then a new one.
            calls(vec![("doc.save", json!({"text": "a"}))]),
            calls(vec![("doc.save", json!({"text": "b"}))]),
            answer("saved a and b [inv:1]"),
        ],
    );
    let scope = {
        let o = r.orch(&m, None);
        o.spawn(
            AgentSpec::new("w")
                .model("m")
                .tools(["doc.*"])
                .allow_mutate(true),
        )
        .await
        .unwrap();
        r.scope(&o)
    };
    r.left_open("t-dead0004", "running", json!({}));
    // The stopped run had saved "a".
    let mut inv = ark_core::Invocation::new("doc.save", RiskTier::Mutate, json!({"text": "a"}));
    inv.mark_ok(json!({"saved": "a"}));
    r.ark.engine().store.put_invocation(&scope, &inv).unwrap();

    let o = r.orch(&m, None);
    let done = o.wait("t-dead0004").await.unwrap();
    assert_eq!(done.state, TaskState::Done, "{:?}", done.error);
    assert_eq!(r.runs.load(Ordering::SeqCst), 1, "only \"b\" ran");
    let texts = m.texts();
    assert!(texts[0].contains("you already did, do not do again: doc.save"));
    assert!(texts[1].contains("not run again"));
}

#[tokio::test]
async fn a_task_waiting_for_the_person_shows_it_and_any_client_answers() {
    let r = Restarts::new();
    let m = Script::new(
        "m",
        vec![
            calls(vec![("doc.save", json!({"text": "a"}))]),
            answer("saved [inv:1]"),
        ],
    );
    let asked = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(ApprovalGate::new(Arc::new(Nobody(asked.clone()))));
    let o = r.orch(&m, Some(gate));
    o.spawn(AgentSpec::new("w").model("m").tools(["doc.*"]))
        .await
        .unwrap();
    let id = o.submit("w", TaskSpec::new("save a")).unwrap();
    for _ in 0..200 {
        if !o.questions().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let waiting = o.task(&id).unwrap();
    assert_eq!(waiting.state, TaskState::Waiting);
    assert_eq!(waiting.waiting_on.as_deref(), Some(WAITING_PERSON));
    let q = waiting.question.clone().unwrap();
    assert_eq!(q.request.action, "doc.save");
    assert_eq!(o.agent("w").unwrap().state, AgentState::Waiting);
    let g = o.graph();
    let node = g.tasks.iter().find(|t| t.task == id).unwrap();
    assert_eq!(node.question.as_ref().unwrap().id, q.id);
    assert!(node.waiting_for.is_empty());
    assert_eq!(asked.load(Ordering::SeqCst), 1);

    // Another client answers through the orchestrator, not the gate's approver.
    o.answer(&id, ApprovalDecision::AllowOnce).unwrap();
    let done = o.wait(&id).await.unwrap();
    assert_eq!(done.state, TaskState::Done, "{:?}", done.error);
    assert!(done.question.is_none());
    assert_eq!(r.runs.load(Ordering::SeqCst), 1);
    assert!(o.questions().is_empty());
    let events = o.events(0, 1000).unwrap();
    assert!(events
        .iter()
        .any(|e| matches!(e.kind, EventKind::Asked { .. })));
    assert!(events.iter().any(|e| matches!(
        e.kind,
        EventKind::Answered {
            decision: ApprovalDecision::AllowOnce,
            ..
        }
    )));
    assert!(
        o.answer(&id, ApprovalDecision::Deny).is_err(),
        "nothing waits now"
    );
}

#[tokio::test]
async fn a_question_survives_a_restart_and_its_answer_runs_the_call_once() {
    let r = Restarts::new();
    // The resumed run asks for the same call again.
    let m = Script::new(
        "m",
        vec![
            calls(vec![("doc.save", json!({"text": "a"}))]),
            answer("saved [inv:1]"),
        ],
    );
    {
        let o = r.orch(&m, None);
        o.spawn(AgentSpec::new("w").model("m").tools(["doc.*"]))
            .await
            .unwrap();
    }
    let request = ApprovalRequest {
        invocation: None,
        project: None,
        action: "doc.save".into(),
        summary: "save".into(),
        risk: RiskTier::Mutate,
        input: json!({"text": "a"}),
        class: "doc.save".into(),
        preview: "doc.save(text=\"a\")".into(),
        agent: Some("w".into()),
        task: Some("t-dead0005".into()),
        ask_each_time: false,
        cut_short: false,
    };
    r.left_open(
        "t-dead0005",
        "waiting",
        json!({
            "waiting_on": "your answer",
            "question": { "id": "q-1", "request": request, "asked_ms": 5 },
            "tool_calls": [{ "id": "c0", "action": "doc.save", "ok": null }]
        }),
    );
    let asked = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(ApprovalGate::new(Arc::new(Nobody(asked.clone()))));
    let o = r.orch(&m, Some(gate));
    // Still waiting for the person, and nothing ran.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let t = o.task("t-dead0005").unwrap();
    assert_eq!(t.state, TaskState::Waiting);
    assert_eq!(
        t.tool_calls[0].ok,
        Some(false),
        "the stopped run's call is not running"
    );
    assert_eq!(o.questions().len(), 1);
    assert!(m.texts().is_empty());

    o.answer("t-dead0005", ApprovalDecision::AllowOnce).unwrap();
    let done = o.wait("t-dead0005").await.unwrap();
    assert_eq!(done.state, TaskState::Done, "{:?}", done.error);
    assert_eq!(r.runs.load(Ordering::SeqCst), 1);
    assert_eq!(asked.load(Ordering::SeqCst), 0, "the kept answer was used");
    assert!(m.texts()[0].contains("the person allowed it"));
}

#[tokio::test]
async fn cancelling_a_task_that_waits_for_an_answer_after_a_restart_ends_it() {
    let r = Restarts::new();
    let m = Script::new("m", vec![]);
    {
        let o = r.orch(&m, None);
        o.spawn(AgentSpec::new("w").model("m")).await.unwrap();
    }
    let request = json!({ "action": "doc.save", "summary": "", "risk": "mutate", "input": {}, "class": "doc.save", "preview": "doc.save()" });
    r.left_open(
        "t-dead0006",
        "waiting",
        json!({ "question": { "id": "q-2", "request": request, "asked_ms": 5 } }),
    );
    let o = r.orch(&m, None);
    o.cancel("t-dead0006").unwrap();
    let t = o.wait("t-dead0006").await.unwrap();
    assert_eq!(t.state, TaskState::Cancelled);
    assert!(t.question.is_none());
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
async fn message_context_survives_restart_and_keeps_visible_text_separate() {
    let f = fixture();
    let m = Script::new("m", vec![answer("noted")]);
    let o = orch(&f, vec![("m", m.clone())], vec![]);
    o.spawn(AgentSpec::new("w").model("m")).await.unwrap();
    let context = r#"{"resource":"document","revision":7,"selection":["title"]}"#;
    assert!(!o
        .message_with_context("user", "w", "Make it clearer", context)
        .unwrap());
    let inbox = o.agent("w").unwrap().inbox;
    assert_eq!(inbox[0].text, "Make it clearer");
    assert_eq!(inbox[0].context, context);
    drop(o);
    let o = orch(&f, vec![("m", m.clone())], vec![]);
    assert_eq!(o.agent("w").unwrap().inbox[0].context, context);
    o.run("w", TaskSpec::new("go")).await.unwrap();
    assert!(m.texts()[0].contains(context));
    assert!(o.agent("w").unwrap().inbox.is_empty());
}

#[tokio::test]
async fn message_context_admission_counts_utf8_bytes_and_pending_inbox() {
    let f = fixture();
    let o = orch(&f, vec![], vec![]);
    o.spawn(AgentSpec::new("w")).await.unwrap();
    let chunk = "é".repeat(4096);
    assert!(o
        .message_with_context("user", "w", "too much", &(chunk.clone() + "é"))
        .is_err());
    for _ in 0..8 {
        assert!(!o.message_with_context("user", "w", "look", &chunk).unwrap());
    }
    assert!(o.message_with_context("user", "w", "full", "x").is_err());
    assert_eq!(o.agent("w").unwrap().inbox.len(), 8);
    assert!(!o.message("user", "w", "ordinary message").unwrap());
    assert_eq!(o.agent("w").unwrap().inbox.len(), 9);
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
    let d = ark_tools::diagram::Diagram::from_text(&text).unwrap();
    assert_eq!(d.kind, DiagramKind::Sequence);
    assert!(d.validate().is_ok());
    assert!(o.graph().edges.iter().any(|e| e.kind == EdgeKind::Produced));
}

/// A session backend factory for `fake:*` specs, sharing one scripted backend per model.
#[tokio::test]
async fn a_files_version_before_is_not_a_task_output() {
    // A call that changes a file keeps the file's version before (ADR 0034). That copy is
    // for Restore, not something the task made, so it does not go back to the delegator.
    struct Rewrite;
    #[async_trait::async_trait]
    impl Adapter for Rewrite {
        fn name(&self) -> &str {
            "doc"
        }
        fn actions(&self) -> Vec<ActionDescriptor> {
            vec![ActionDescriptor::new(
                "doc.rewrite",
                "rewrite",
                RiskTier::Mutate,
            )]
        }
        async fn run(
            &self,
            _: &str,
            _: serde_json::Value,
            _: &RunCtx<'_>,
        ) -> ark_tools::Result<ActionOutput> {
            let mut now = ProducedArtifact::text("notes.md", "new text");
            now.path = Some("notes.md".into());
            let mut before = ProducedArtifact::text("notes.md", "old text");
            before.path = Some("notes.md".into());
            before.before = true;
            let mut out = ActionOutput::json(json!({ "saved": "notes.md" }));
            out.produced = vec![now, before];
            Ok(out)
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let ark = Ark::builder()
        .store_root(dir.path())
        .tool(Arc::new(Rewrite))
        .provider(Arc::new(ark_model::MockProvider::new("unused", vec![])))
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("p", "").unwrap();
    let m = Script::new(
        "m",
        vec![
            calls(vec![("doc.rewrite", json!({}))]),
            answer("rewrote notes.md [inv:1]"),
        ],
    );
    let mm = m.clone();
    let o = Orchestrator::builder(&ark, &project)
        .configure_models(move |b| b.provider("m", mm as Arc<dyn Provider>))
        .build()
        .unwrap();
    o.spawn(
        AgentSpec::new("editor")
            .model("m")
            .tools(["doc.*"])
            .allow_mutate(true),
    )
    .await
    .unwrap();
    let done = o
        .run("editor", TaskSpec::new("rewrite notes"))
        .await
        .unwrap();
    assert_eq!(done.state, TaskState::Done, "{:?}", done.error);
    assert_eq!(done.outputs.len(), 1, "{:?}", done.outputs);
    let art = ark
        .engine()
        .store
        .resolve_artifact(
            &ProjectId(o.agent("editor").unwrap().scope),
            &done.outputs[0].id,
        )
        .unwrap()
        .unwrap();
    assert_eq!(ark.engine().store.read_blob(&art.id).unwrap(), b"new text");
}

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

/// A scripted model with a web search of its own, which records the tools each request offers.
struct Searcher {
    script: Arc<Script>,
    tools: Mutex<Vec<Vec<String>>>,
}

#[async_trait::async_trait]
impl Provider for Searcher {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> ark_model::Result<Completion> {
        self.tools
            .lock()
            .unwrap()
            .push(tools.iter().map(|t| t.name.clone()).collect());
        self.script.complete(system, messages, tools).await
    }
    async fn web_search(
        &self,
        query: &str,
        _limit: usize,
    ) -> Option<ark_model::Result<Vec<ark_model::WebHit>>> {
        Some(Ok(vec![ark_model::WebHit {
            title: format!("{query} as {} found it", self.script.id),
            url: format!("https://{}.example/", self.script.id),
            snippet: String::new(),
        }]))
    }
    fn describe(&self) -> String {
        self.script.id.clone()
    }
}

#[tokio::test]
async fn each_agent_searches_the_web_on_its_own_model_when_its_tools_allow_it() {
    let dir = tempfile::tempdir().unwrap();
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(Arc::new(ark_model::MockProvider::new("unused", vec![])))
        .web_with(ark_tools::web::WebAdapter::new().backends(vec![]))
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("p", "").unwrap();
    let researcher = Arc::new(Searcher {
        script: Script::new(
            "researcher",
            vec![
                calls(vec![("web.search", json!({ "query": "tea" }))]),
                answer("found it [inv:1]"),
            ],
        ),
        tools: Mutex::new(Vec::new()),
    });
    let writer = Arc::new(Searcher {
        script: Script::new("writer", vec![answer("written")]),
        tools: Mutex::new(Vec::new()),
    });
    let (r, w) = (researcher.clone(), writer.clone());
    let o = Orchestrator::builder(&ark, &project)
        .configure_models(move |b| {
            b.provider("researcher", r as Arc<dyn Provider>)
                .provider("writer", w as Arc<dyn Provider>)
        })
        .build()
        .unwrap();
    o.spawn(
        AgentSpec::new("researcher")
            .model("researcher")
            .tools(["web.search"]),
    )
    .await
    .unwrap();
    o.spawn(AgentSpec::new("writer").model("writer").tools(["notes.*"]))
        .await
        .unwrap();
    o.run("researcher", TaskSpec::new("find tea"))
        .await
        .unwrap();
    o.run("writer", TaskSpec::new("write")).await.unwrap();

    let offered = researcher.tools.lock().unwrap()[0].clone();
    assert!(offered.contains(&"web.search".to_string()), "{offered:?}");
    assert!(
        !offered.contains(&"web.fetch".to_string()),
        "only what its tools allow"
    );
    let seen = researcher.script.texts();
    assert!(
        seen[1].contains("1 result(s) for \"tea\" (researcher):"),
        "searched on its own model: {}",
        seen[1]
    );
    let offered = writer.tools.lock().unwrap()[0].clone();
    assert!(
        !offered.iter().any(|t| t.starts_with("web.")),
        "{offered:?}"
    );
}

// ---- the session's model reaches the agents it creates (ADR 0014) ----------------------

/// The person starts on an external model ("cloud") and switches the conversation to a local
/// one. An agent the conversation then creates, and one created before the switch, both run
/// on the local model: the external one is never called again.
#[tokio::test]
async fn agents_without_a_model_follow_the_sessions_model_when_it_changes() {
    let dir = tempfile::tempdir().unwrap();
    let cloud = Script::new("cloud", vec![]);
    let mut ark = Ark::builder()
        .store_root(dir.path())
        .provider(cloud.clone() as Arc<dyn Provider>)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("team", "a test").unwrap();
    let o = Orchestrator::builder(&ark, &project).build().unwrap();
    let early = o.spawn(AgentSpec::new("early")).await.unwrap();
    assert_eq!(early.model, "cloud");

    let local = Script::new(
        "local",
        vec![
            calls(vec![(
                "agents.create",
                json!({"name": "helper", "role": "checks one thing"}),
            )]),
            calls(vec![delegate("helper", "check it")]),
            answer("checked [inv:1]"),
            answer("the helper checked it [inv:1]"),
            answer("early is done [inv:1]"),
        ],
    );
    ark.set_router(ModelRouter::single(local.clone() as Arc<dyn Provider>));

    let mut session = ark.start_session(&project.id).unwrap();
    let opts = TurnOptions {
        tools: vec![o.chat_tools()],
        ..Default::default()
    };
    ark.ask_session(&mut session, "get the helper to check it", opts, |_| {})
        .await
        .unwrap();
    let helper = o.agent("helper").expect("the conversation created it");
    assert_eq!(helper.model, "local");
    assert_eq!(helper.account, "local");

    // Created before the switch: it moves to the new model before its next task.
    let t = o.run("early", TaskSpec::new("finish")).await.unwrap();
    assert_eq!(t.state, TaskState::Done, "{:?}", t.error);
    assert_eq!(o.agent("early").unwrap().model, "local");

    assert!(
        cloud.texts().is_empty(),
        "the external model was called after the person chose a local one: {:?}",
        cloud.texts()
    );
    assert_eq!(local.texts().len(), 5);
}

/// The person's explicit choice for one agent wins over the session's model, and the agent
/// keeps it when the session's model changes.
#[tokio::test]
async fn an_agents_own_model_is_kept_when_the_sessions_model_changes() {
    let dir = tempfile::tempdir().unwrap();
    let local = Script::new("local", vec![]);
    let other = Script::new("other", vec![answer("done [inv:1]")]);
    let mut ark = Ark::builder()
        .store_root(dir.path())
        .provider(Script::new("first", vec![]) as Arc<dyn Provider>)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("team", "a test").unwrap();
    let o = Orchestrator::builder(&ark, &project)
        .configure_models(move |b| b.provider("other", other as Arc<dyn Provider>))
        .build()
        .unwrap();
    o.spawn(AgentSpec::new("pinned").model("other"))
        .await
        .unwrap();
    ark.set_router(ModelRouter::single(local.clone() as Arc<dyn Provider>));
    let t = o.run("pinned", TaskSpec::new("work")).await.unwrap();
    assert_eq!(t.state, TaskState::Done, "{:?}", t.error);
    assert_eq!(o.agent("pinned").unwrap().model, "other");
    assert!(local.texts().is_empty());
}

/// An agent on the session's subscription model leaves its vendor session when the person
/// switches to a local model: the session is released and the subscription is not called.
#[tokio::test]
async fn moving_off_a_subscription_releases_the_agents_vendor_session() {
    let dir = tempfile::tempdir().unwrap();
    let vendor = Arc::new(MockSessions::new(
        "fake:cloud",
        "fake",
        vec![answer("first [inv:1]")],
    ));
    let subs = fake_sessions(vec![("cloud", vendor.clone())], SessionPolicy::default());
    let mut ark = Ark::builder()
        .store_root(dir.path())
        .provider(Script::new("fake:cloud", vec![]) as Arc<dyn Provider>)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("team", "a test").unwrap();
    let o = Orchestrator::builder(&ark, &project)
        .configure_models(|b| b.subscriptions(subs))
        .build()
        .unwrap();
    o.spawn(AgentSpec::new("w")).await.unwrap();
    let one = o.run("w", TaskSpec::new("first")).await.unwrap();
    assert_eq!(one.state, TaskState::Done, "{:?}", one.error);
    assert_eq!(o.agent("w").unwrap().account, "fake");
    let before = vendor.calls().len();

    let local = Script::new("local", vec![answer("second [inv:1]")]);
    ark.set_router(ModelRouter::single(local.clone() as Arc<dyn Provider>));
    let two = o.run("w", TaskSpec::new("second")).await.unwrap();
    assert_eq!(two.state, TaskState::Done, "{:?}", two.error);
    assert_eq!(o.agent("w").unwrap().model, "local");
    let after: Vec<Call> = vendor.calls()[before..].to_vec();
    assert!(
        after.iter().all(|c| matches!(c, Call::Discard { .. })),
        "only the release reached the subscription: {after:?}"
    );
    assert_eq!(local.texts().len(), 1);
    assert!(o.graph().agent("w").unwrap().session.is_none());
}

#[tokio::test]
async fn an_agent_gets_its_own_browser_when_its_tools_allow_it() {
    let dir = tempfile::tempdir().unwrap();
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(Arc::new(ark_model::MockProvider::new("unused", vec![])))
        .browser_with(ark_sdk::prelude::ark_browser::BrowserService::new(
            dir.path().join("browser"),
        ))
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("p", "").unwrap();
    let browsing = Arc::new(Searcher {
        script: Script::new("browsing", vec![answer("looked")]),
        tools: Mutex::new(Vec::new()),
    });
    let writing = Arc::new(Searcher {
        script: Script::new("writing", vec![answer("wrote")]),
        tools: Mutex::new(Vec::new()),
    });
    let (b, w) = (browsing.clone(), writing.clone());
    let o = Orchestrator::builder(&ark, &project)
        .configure_models(move |m| {
            m.provider("browsing", b as Arc<dyn Provider>)
                .provider("writing", w as Arc<dyn Provider>)
        })
        .build()
        .unwrap();
    o.spawn(
        AgentSpec::new("researcher")
            .model("browsing")
            .tools(["browser.*"]),
    )
    .await
    .unwrap();
    o.spawn(AgentSpec::new("writer").model("writing").tools(["notes.*"]))
        .await
        .unwrap();
    o.run("researcher", TaskSpec::new("look")).await.unwrap();
    o.run("writer", TaskSpec::new("write")).await.unwrap();
    let offered = browsing.tools.lock().unwrap()[0].clone();
    for a in [
        "browser.open",
        "browser.read",
        "browser.click",
        "browser.type",
        "browser.back",
    ] {
        assert!(offered.contains(&a.to_string()), "{a} in {offered:?}");
    }
    let offered = writing.tools.lock().unwrap()[0].clone();
    assert!(
        !offered.iter().any(|t| t.starts_with("browser.")),
        "{offered:?}"
    );
}

/// The conversation creates a researcher with only the web and the browser; a capability
/// that does not exist is refused with the ones that do.
#[tokio::test]
async fn the_conversation_gives_a_new_agent_only_the_capabilities_its_role_needs() {
    let dir = tempfile::tempdir().unwrap();
    let chat = Script::new(
        "chat",
        vec![
            calls(vec![(
                "agents.create",
                json!({"name": "researcher", "role": "finds things on the web", "capabilities": ["web", "browser"]}),
            )]),
            calls(vec![(
                "agents.create",
                json!({"name": "pilot", "role": "flies", "capabilities": ["teleport"]}),
            )]),
            answer("made a researcher [inv:1]"),
        ],
    );
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(chat.clone() as Arc<dyn Provider>)
        .web_with(ark_tools::web::WebAdapter::new().backends(vec![]))
        .browser_with(ark_sdk::prelude::ark_browser::BrowserService::new(
            dir.path().join("browser"),
        ))
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("team", "").unwrap();
    let o = Orchestrator::builder(&ark, &project).build().unwrap();
    let caps = o.capabilities();
    assert!(
        caps.contains(&"web".to_string()) && caps.contains(&"browser".to_string()),
        "{caps:?}"
    );

    let mut session = ark.start_session(&project.id).unwrap();
    let opts = TurnOptions {
        tools: vec![o.chat_tools()],
        ..Default::default()
    };
    ark.ask_session(&mut session, "make a researcher", opts, |_| {})
        .await
        .unwrap();
    assert_eq!(
        o.agent("researcher").unwrap().spec.tools,
        vec!["web.*".to_string(), "browser.*".to_string()]
    );
    assert!(o.agent("pilot").is_none());
    let seen = chat.texts();
    assert!(
        seen[2].contains("no capability `teleport`; one of:"),
        "{}",
        seen[2]
    );
}

#[tokio::test]
async fn a_schedule_runs_its_task_at_its_times_and_survives_a_restart() {
    let f = fixture();
    let m = Script::new("m", vec![answer("first digest"), answer("caught up")]);
    let id = {
        let o = orch(&f, vec![("m", m.clone())], vec![]);
        o.spawn(AgentSpec::new("w").model("m")).await.unwrap();
        let s = o
            .schedule(
                "w",
                TaskSpec::new("write the digest"),
                When::every_minutes(10),
            )
            .unwrap();
        assert_eq!(s.says, "every 10 minutes");
        let next = s.next_ms.unwrap();
        // Not due yet.
        assert!(o.tick_schedules(next - 1).await.is_empty());
        let started = o.tick_schedules(next).await;
        assert_eq!(started.len(), 1);
        let done = o.wait(&started[0]).await.unwrap();
        assert_eq!(done.result.as_deref(), Some("first digest"));
        assert_eq!(done.schedule.as_deref(), Some(s.id.as_str()));
        let after = o.schedules()[0].clone();
        assert_eq!((after.runs, after.next_ms), (1, Some(next + 10 * 60_000)));
        assert_eq!(o.graph().schedules.len(), 1);
        assert_eq!(
            o.graph().task(&started[0]).unwrap().schedule,
            Some(s.id.clone())
        );
        s.id
    };
    // A restart keeps it; runs missed while the process was down run once, not once each.
    let o = orch(&f, vec![("m", m.clone())], vec![]);
    let s = o.schedules();
    assert_eq!(s.len(), 1);
    let late = s[0].next_ms.unwrap() + 5 * 60 * 60_000;
    let started = o.tick_schedules(late).await;
    assert_eq!(started.len(), 1);
    assert!(o.tick_schedules(late).await.is_empty());
    o.wait(&started[0]).await.unwrap();
    assert_eq!(o.schedules()[0].next_ms, Some(late + 10 * 60_000));
    // Stopped for good, also after a restart.
    o.unschedule(&id).unwrap();
    assert!(o.schedules().is_empty() && o.graph().schedules.is_empty());
    assert!(scheduled_projects(&f.ark.engine().store).is_empty());
    drop(o);
    assert!(orch(&f, vec![("m", m)], vec![]).schedules().is_empty());
}

#[tokio::test]
async fn a_scheduled_run_is_skipped_while_the_last_one_is_still_open() {
    let f = fixture();
    let gate = Arc::new(Notify::new());
    let m = Script::with(
        "m",
        vec![answer("slow")],
        Duration::ZERO,
        Some(gate.clone()),
    );
    let o = orch(&f, vec![("m", m)], vec![]);
    o.spawn(AgentSpec::new("w").model("m")).await.unwrap();
    let s = o
        .schedule("w", TaskSpec::new("poll"), When::every_minutes(5))
        .unwrap();
    assert_eq!(
        scheduled_projects(&f.ark.engine().store),
        vec![f.project.id.to_string()]
    );
    let t0 = s.next_ms.unwrap();
    let first = o.tick_schedules(t0).await;
    assert_eq!(first.len(), 1);
    // Still running at the next time: skipped, and the time moves on.
    assert!(o.tick_schedules(t0 + 5 * 60_000).await.is_empty());
    let now = o.schedules()[0].clone();
    assert_eq!((now.runs, now.skipped), (1, 1));
    gate.notify_one();
    assert_eq!(o.wait(&first[0]).await.unwrap().state, TaskState::Done);
    // Bad times are refused with words the model can act on.
    let e = o
        .schedule("w", TaskSpec::new("x"), When::daily_at("9am"))
        .unwrap_err();
    assert!(e.to_string().contains("HH:MM"), "{e}");
    assert!(o
        .schedule("nobody", TaskSpec::new("x"), When::every_minutes(5))
        .is_err());
    // Standing work no more often than every 5 minutes.
    let e = o
        .schedule("w", TaskSpec::new("x"), When::every_minutes(1))
        .unwrap_err();
    assert!(e.to_string().contains("at least 5"), "{e}");
    // A stopped schedule stays stopped, also when it was due; an ended agent's schedules end.
    let due = o
        .schedule("w", TaskSpec::new("again"), When::every_minutes(5))
        .unwrap();
    o.unschedule(&due.id).unwrap();
    assert!(o.tick_schedules(due.next_ms.unwrap()).await.is_empty());
    assert_eq!(o.schedules().len(), 1);
    o.terminate("w").await.unwrap();
    assert!(o.schedules().is_empty());
    assert!(scheduled_projects(&f.ark.engine().store).is_empty());
    let stopped = o
        .events(0, 500)
        .unwrap()
        .into_iter()
        .filter(|e| matches!(&e.kind, EventKind::Unscheduled { agent, .. } if agent == "w"))
        .count();
    assert_eq!(stopped, 2);
}

#[tokio::test]
async fn a_scheduled_run_posts_its_result_in_the_chat_that_asked() {
    let dir = tempfile::tempdir().unwrap();
    let chat = Script::new(
        "chat",
        vec![
            calls(vec![(
                "agents.schedule",
                json!({"to": "news", "objective": "summarize the news", "every_minutes": 30}),
            )]),
            answer("scheduled [inv:1]"),
            // The scheduled run, by the news agent on the same model.
            answer("3 stories about the harbour"),
            // The chat's next turn.
            answer("Glad it helps."),
        ],
    );
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(chat.clone() as Arc<dyn Provider>)
        .allow_mutate(true)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("team", "").unwrap();
    let o = Orchestrator::builder(&ark, &project).build().unwrap();
    o.spawn(AgentSpec::new("news").role("reads the news"))
        .await
        .unwrap();
    let mut session = ark.start_session(&project.id).unwrap();
    let id = session.id().as_str().to_string();
    let opts = || TurnOptions {
        tools: vec![o.chat_tools_for(&id)],
        ..Default::default()
    };
    ark.ask_session(
        &mut session,
        "every 30 minutes have news summarize the news",
        opts(),
        |_| {},
    )
    .await
    .unwrap();
    assert!(chat.texts()[0].contains("its result is posted in this chat"));
    let s = o.schedules();
    assert_eq!(s[0].task.report_to.as_deref(), Some(id.as_str()));

    let run = o.tick_schedules(s[0].next_ms.unwrap()).await;
    let done = o.wait(&run[0]).await.unwrap();
    assert_eq!(done.report_to.as_deref(), Some(id.as_str()));
    let posts = ark_runtime::session::posts(ark.engine(), &id, 0, 10).unwrap();
    assert_eq!(posts.len(), 1);
    let p = &posts[0].1;
    assert_eq!(
        (p.from.as_str(), p.text.as_str(), p.ok, p.after),
        ("news", "3 stories about the harbour", true, Some(0))
    );
    assert!(o
        .events(0, 200)
        .unwrap()
        .iter()
        .any(|e| matches!(&e.kind, EventKind::Posted { session, .. } if *session == id)));

    // The chat's next turn knows what was posted.
    ark.ask_session(&mut session, "thanks for the digest", opts(), |_| {})
        .await
        .unwrap();
    let last = chat.texts().last().unwrap().clone();
    assert!(
        last.contains("[news · summarize the news] 3 stories about the harbour"),
        "{last}"
    );
}

#[tokio::test]
async fn the_conversation_schedules_work_and_stops_it() {
    let dir = tempfile::tempdir().unwrap();
    let chat = Script::new(
        "chat",
        vec![
            calls(vec![(
                "agents.schedule",
                json!({"to": "news", "objective": "summarize the news", "at": "08:30", "days": ["weekdays"]}),
            )]),
            answer("scheduled [inv:1]"),
        ],
    );
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(chat.clone() as Arc<dyn Provider>)
        .allow_mutate(true)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("team", "").unwrap();
    let o = Orchestrator::builder(&ark, &project).build().unwrap();
    o.spawn(AgentSpec::new("news").role("reads the news"))
        .await
        .unwrap();
    let mut session = ark.start_session(&project.id).unwrap();
    let opts = || TurnOptions {
        tools: vec![o.chat_tools()],
        ..Default::default()
    };
    ark.ask_session(
        &mut session,
        "every weekday at 8:30 have news summarize the news",
        opts(),
        |_| {},
    )
    .await
    .unwrap();
    let s = o.schedules();
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].says, "every weekday at 08:30");
    assert!(chat.texts()[1].contains("every weekday at 08:30"));
    // The next turn's tools list what runs, so the chat can stop it by id.
    chat.replies.lock().unwrap().extend([
        calls(vec![("agents.unschedule", json!({ "schedule": s[0].id }))]),
        answer("stopped"),
    ]);
    ark.ask_session(&mut session, "stop the news digest", opts(), |_| {})
        .await
        .unwrap();
    assert!(o.schedules().is_empty());
    let events = o.events(0, 100).unwrap();
    assert!(events
        .iter()
        .any(|e| matches!(e.kind, EventKind::Scheduled { .. })));
    assert!(events
        .iter()
        .any(|e| matches!(e.kind, EventKind::Unscheduled { .. })));
}

/// A chat (the person's conversation) and an agent on its own model, for hand-offs.
struct ChatAndAgent {
    _dir: tempfile::TempDir,
    ark: Ark,
    project: Project,
    o: Orchestrator,
}

async fn chat_and_agent(chat: &Arc<Script>, agent: &Arc<Script>) -> ChatAndAgent {
    let dir = tempfile::tempdir().unwrap();
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(chat.clone() as Arc<dyn Provider>)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("team", "").unwrap();
    let a = agent.clone();
    let o = Orchestrator::builder(&ark, &project)
        .configure_models(move |b| b.provider("m", a as Arc<dyn Provider>))
        .build()
        .unwrap();
    o.spawn(AgentSpec::new("w").model("m")).await.unwrap();
    ChatAndAgent {
        _dir: dir,
        ark,
        project,
        o,
    }
}

impl ChatAndAgent {
    async fn turn(&self, input: &str) -> String {
        let mut session = self.ark.start_session(&self.project.id).unwrap();
        let id = session.id().as_str().to_string();
        self.ark
            .ask_session(
                &mut session,
                input,
                TurnOptions {
                    tools: vec![self.o.chat_tools_for(&id)],
                    ..Default::default()
                },
                |_| {},
            )
            .await
            .unwrap();
        id
    }
    fn posts(&self, chat: &str) -> Vec<ark_runtime::session::Post> {
        ark_runtime::session::posts(self.ark.engine(), chat, 0, 100)
            .unwrap()
            .into_iter()
            .map(|(_, p)| p)
            .collect()
    }
}

#[tokio::test]
async fn a_handoff_the_chat_waits_for_is_its_answer_and_is_not_posted_too() {
    let chat = Script::new(
        "chat",
        vec![
            calls(vec![(
                "agents.delegate",
                json!({"to": "w", "objective": "count the chapters"}),
            )]),
            answer("w counted 12 [inv:1]"),
        ],
    );
    let agent = Script::new("m", vec![answer("12 chapters")]);
    let c = chat_and_agent(&chat, &agent).await;
    let id = c.turn("how many chapters?").await;
    let t = &c.o.tasks()[0];
    assert_eq!(t.report_to.as_deref(), Some(id.as_str()));
    assert_eq!(t.result.as_deref(), Some("12 chapters"));
    assert!(t.reported, "the chat's own call had it");
    assert!(c.posts(&id).is_empty());
    assert!(chat.texts()[1].contains("12 chapters"));
}

#[tokio::test]
async fn a_handoff_the_chat_does_not_wait_for_is_posted_in_it_once() {
    let chat = Script::new(
        "chat",
        vec![
            calls(vec![(
                "agents.delegate",
                json!({"to": "w", "objective": "count the chapters", "wait": false}),
            )]),
            answer("w is on it [inv:1]"),
        ],
    );
    let gate = Arc::new(Notify::new());
    let agent = Script::with(
        "m",
        vec![answer("12 chapters")],
        Duration::ZERO,
        Some(gate.clone()),
    );
    let c = chat_and_agent(&chat, &agent).await;
    let id = c.turn("count the chapters").await;
    assert!(chat.texts()[1].contains("posted in this chat when it ends"));
    // The turn is over; the task ends after it.
    let task = c.o.tasks()[0].id.clone();
    gate.notify_one();
    let t = c.o.wait(&task).await.unwrap();
    assert!(t.reported);
    let posts = c.posts(&id);
    assert_eq!(posts.len(), 1);
    assert_eq!(
        (posts[0].text.as_str(), posts[0].work.as_deref()),
        ("12 chapters", Some(task.as_str()))
    );
}

#[tokio::test]
async fn a_result_that_never_reached_its_chat_is_posted_after_the_restart_once() {
    let r = Restarts::new();
    let chat = ark_runtime::Session::start(r.ark.engine(), &r.project.id).unwrap();
    let chat = chat.id().to_string();
    let m = Script::new("m", vec![answer("12 chapters")]);
    {
        let o = r.orch(&m, None);
        o.spawn(AgentSpec::new("w").model("m")).await.unwrap();
    }
    let posts = |chat: &str| ark_runtime::session::posts(r.ark.engine(), chat, 0, 100).unwrap();
    // The chat waited for it in a process that stopped: it continues and reports here.
    r.left_open("t-hand0001", "running", json!({ "report_to": chat }));
    // It ended, but the process stopped before its result reached the chat.
    r.left_open(
        "t-hand0002",
        "done",
        json!({ "report_to": chat, "reported": false, "result": "finished before" }),
    );
    // Posted, and the process stopped before it marked that.
    r.left_open(
        "t-hand0003",
        "done",
        json!({ "report_to": chat, "reported": false, "result": "posted once" }),
    );
    let post = ark_runtime::session::Post::new("w", "x", "posted once", true).work("t-hand0003");
    ark_runtime::session::post(r.ark.engine(), &chat, post).unwrap();
    // Saved by a version without the mark: it was posted when it finished.
    r.left_open(
        "t-hand0004",
        "done",
        json!({ "report_to": chat, "result": "long ago" }),
    );

    let o = r.orch(&m, None);
    let done = o.wait("t-hand0001").await.unwrap();
    assert_eq!(done.state, TaskState::Done, "{:?}", done.error);
    let texts: Vec<String> = posts(&chat).into_iter().map(|(_, p)| p.text).collect();
    assert_eq!(texts.len(), 3, "{texts:?}");
    for t in ["posted once", "finished before", "12 chapters"] {
        assert_eq!(
            texts.iter().filter(|x| *x == t).count(),
            1,
            "{t}: {texts:?}"
        );
    }
    assert!(["t-hand0001", "t-hand0002", "t-hand0003", "t-hand0004"]
        .iter()
        .all(|id| o.task(id).unwrap().reported));
    // Another restart posts nothing again.
    drop(o);
    let _o = r.orch(&m, None);
    assert_eq!(posts(&chat).len(), 3);
}

/// A state-changing tool that takes a while, counting the runs that started and finished.
struct SlowChange {
    started: Arc<Notify>,
    finished: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Adapter for SlowChange {
    fn name(&self) -> &str {
        "slow"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            "slow.save",
            "save, slowly",
            RiskTier::Mutate,
        )]
    }
    async fn run(
        &self,
        _: &str,
        input: serde_json::Value,
        _: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        self.started.notify_one();
        tokio::time::sleep(Duration::from_millis(400)).await;
        self.finished.fetch_add(1, Ordering::SeqCst);
        Ok(ActionOutput::json(json!({ "saved": input["text"] })))
    }
}

/// One process of an app with the slow tool, over the store in `dir`.
fn slow_process(dir: &std::path::Path, finished: &Arc<AtomicUsize>) -> (Ark, Arc<Notify>) {
    let started = Arc::new(Notify::new());
    let ark = Ark::builder()
        .store_root(dir)
        .tool(Arc::new(SlowChange {
            started: started.clone(),
            finished: finished.clone(),
        }))
        .provider(Arc::new(ark_model::MockProvider::new("unused", vec![])))
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    (ark, started)
}

async fn until(what: &str, mut ok: impl FnMut() -> bool) {
    for _ in 0..400 {
        if ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test]
async fn a_stopping_process_lets_a_change_under_way_finish_and_the_task_continues_after() {
    let dir = tempfile::tempdir().unwrap();
    let finished = Arc::new(AtomicUsize::new(0));
    let build = |ark: &Ark, project: &Project, m: &Arc<Script>| {
        let mm = m.clone();
        Orchestrator::builder(ark, project)
            .configure_models(move |b| b.provider("m", mm as Arc<dyn Provider>))
            .build()
            .unwrap()
    };
    let m = Script::new(
        "m",
        vec![
            calls(vec![("slow.save", json!({"text": "a"}))]),
            answer("never asked"),
        ],
    );
    let (ark, started) = slow_process(dir.path(), &finished);
    let project = ark.create_project("p", "").unwrap();
    let o = build(&ark, &project, &m);
    o.spawn(
        AgentSpec::new("w")
            .model("m")
            .tools(["slow.*"])
            .allow_mutate(true),
    )
    .await
    .unwrap();
    let id = o.submit("w", TaskSpec::new("save a")).unwrap();
    started.notified().await;

    // A deploy: the change under way finishes, nothing new starts, the task waits for the next
    // process.
    ark.engine().stop_soon();
    tokio::time::timeout(Duration::from_secs(5), ark.engine().runs_ended())
        .await
        .unwrap();
    assert_eq!(finished.load(Ordering::SeqCst), 1);
    until("the task to wait for the next process", || {
        o.task(&id).unwrap().state == TaskState::Queued
    })
    .await;
    let t = o.task(&id).unwrap();
    assert_eq!(t.waiting_on.as_deref(), Some(RESTARTED));
    assert_eq!((t.pid, t.error), (None, None));
    assert_eq!(m.texts().len(), 1, "no model call after the stop");
    let invs = ark
        .engine()
        .store
        .list_invocations(&ProjectId(o.agent("w").unwrap().scope))
        .unwrap();
    assert!(invs
        .iter()
        .all(|i| i.status == ark_core::InvocationStatus::Ok));
    drop((o, ark));

    // The next process continues it; the change made before is not made again.
    let m2 = Script::new(
        "m2",
        vec![
            calls(vec![("slow.save", json!({"text": "a"}))]),
            answer("saved a [inv:1]"),
        ],
    );
    let (ark, _) = slow_process(dir.path(), &finished);
    let project = ark.find_or_create_project("p", "").unwrap();
    let o = build(&ark, &project, &m2);
    let t = o.wait(&id).await.unwrap();
    assert_eq!(t.state, TaskState::Done, "{:?}", t.error);
    assert_eq!(finished.load(Ordering::SeqCst), 1, "saved once");
    assert!(m2.texts()[0].contains("It was interrupted"));
}

#[tokio::test]
async fn a_stopping_process_drops_a_model_call_and_the_task_continues_after() {
    let r = Restarts::new();
    let never = Arc::new(Notify::new());
    let m = Script::with("m", vec![], Duration::ZERO, Some(never));
    let o = r.orch(&m, None);
    o.spawn(AgentSpec::new("w").model("m")).await.unwrap();
    let id = o.submit("w", TaskSpec::new("think")).unwrap();
    until("the model call", || m.texts().len() == 1).await;
    r.ark.engine().stop_soon();
    until("the task to wait for the next process", || {
        o.task(&id).unwrap().state == TaskState::Queued
    })
    .await;
    // Work handed to it now waits for the next process too.
    let later = o.submit("w", TaskSpec::new("later")).unwrap();
    until("the new task to wait", || {
        o.task(&later)
            .is_some_and(|t| t.waiting_on.as_deref() == Some(RESTARTED))
    })
    .await;
    assert_eq!(o.agent("w").unwrap().state, AgentState::Idle);
    assert_eq!(m.texts().len(), 1);
}

/// Says yes to every question, counting them.
struct Yes(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Approver for Yes {
    async fn decide(&self, _: &ApprovalRequest) -> ApprovalDecision {
        self.0.fetch_add(1, Ordering::SeqCst);
        ApprovalDecision::AllowOnce
    }
}

/// ADR 0045: an agent whose tools allow a computer action sees the offer, asks, and gets only
/// what its tools allow; an agent without one is not offered the Computer.
#[tokio::test]
async fn an_agent_asks_for_the_computer_only_when_its_tools_allow_it() {
    use ark_sdk::prelude::ark_computer::{fake::FakeScreen, ComputerService};
    let dir = tempfile::tempdir().unwrap();
    let svc = ComputerService::with_screen(Arc::new(
        FakeScreen::new().app("Lumen", &[("button", "Go")]),
    ))
    .switched(dir.path().join("computer.json"));
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(Arc::new(ark_model::MockProvider::new("unused", vec![])))
        .computer_with(svc.clone())
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("p", "").unwrap();
    let looking = Arc::new(Searcher {
        script: Script::new(
            "looking",
            vec![
                calls(vec![("acquire_capability", json!({ "need": "computer" }))]),
                answer("looked"),
            ],
        ),
        tools: Mutex::new(Vec::new()),
    });
    let writing = Arc::new(Searcher {
        script: Script::new("writing", vec![answer("wrote")]),
        tools: Mutex::new(Vec::new()),
    });
    let asked = Arc::new(AtomicUsize::new(0));
    let (l, w) = (looking.clone(), writing.clone());
    let o = Orchestrator::builder(&ark, &project)
        .approvals(Arc::new(ApprovalGate::new(Arc::new(Yes(asked.clone())))))
        .configure_models(move |m| {
            m.provider("looking", l as Arc<dyn Provider>)
                .provider("writing", w as Arc<dyn Provider>)
        })
        .build()
        .unwrap();
    o.spawn(
        AgentSpec::new("checker")
            .model("looking")
            .tools(["computer.look"]),
    )
    .await
    .unwrap();
    o.spawn(AgentSpec::new("writer").model("writing").tools(["notes.*"]))
        .await
        .unwrap();
    o.run("writer", TaskSpec::new("write")).await.unwrap();
    let offered = writing.tools.lock().unwrap()[0].clone();
    assert!(
        !offered
            .iter()
            .any(|t| t.starts_with("computer.") || t == "acquire_capability"),
        "{offered:?}"
    );

    o.run("checker", TaskSpec::new("look at Lumen"))
        .await
        .unwrap();
    let steps = looking.tools.lock().unwrap().clone();
    assert!(
        steps[0].iter().any(|t| t == "acquire_capability"),
        "{:?}",
        steps[0]
    );
    assert!(
        !steps[0].iter().any(|t| t.starts_with("computer.")),
        "{:?}",
        steps[0]
    );
    assert_eq!(asked.load(Ordering::SeqCst), 1);
    assert!(svc.is_on());
    let computer: Vec<&String> = steps[1]
        .iter()
        .filter(|t| t.starts_with("computer."))
        .collect();
    assert_eq!(computer, ["computer.look"], "only what its tools allow");
}

/// ADR 0048: a chat's turn handed work to an agent and stopped before it finished (here the
/// person stopped it; a crash leaves the same records). The turn that continues it, asked in
/// other words, takes that hand-off up instead of handing the work off a second time, and is
/// told it is under way.
#[tokio::test]
async fn a_turn_that_continues_a_stopped_one_takes_up_its_handoff_instead_of_making_another() {
    let chat = Script::new(
        "chat",
        vec![
            calls(vec![(
                "agents.delegate",
                json!({"to": "w", "objective": "count the chapters"}),
            )]),
            // The continuation words the same work differently (and would wait for it).
            calls(vec![(
                "agents.delegate",
                json!({"to": "w", "objective": "count all chapters in the book"}),
            )]),
            answer("w is counting them"),
        ],
    );
    let gate = Arc::new(Notify::new());
    let agent = Script::with(
        "m",
        vec![answer("12 chapters")],
        Duration::ZERO,
        Some(gate.clone()),
    );
    let c = chat_and_agent(&chat, &agent).await;
    let mut session = c.ark.start_session(&c.project.id).unwrap();
    let id = session.id().as_str().to_string();
    let tools = || TurnOptions {
        tools: vec![c.o.chat_tools_for(&id)],
        ..Default::default()
    };

    // The first turn waits for the agent; it is stopped while the agent works.
    let cancel = CancelToken::new();
    let stop = cancel.clone();
    let o = c.o.clone();
    tokio::spawn(async move {
        while o.tasks().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        stop.cancel();
    });
    let first = c
        .ark
        .ask_session(
            &mut session,
            "count the chapters",
            TurnOptions {
                cancel: Some(cancel),
                ..tools()
            },
            |_| {},
        )
        .await
        .unwrap();
    assert!(first.cancelled);
    let task = c.o.tasks()[0].clone();
    assert_eq!(task.work.as_deref(), Some(session.work_of(0).as_str()));

    c.ark
        .ask_session(&mut session, "continue", tools(), |_| {})
        .await
        .unwrap();
    let tasks = c.o.tasks();
    assert_eq!(tasks.len(), 1, "handed off once: {tasks:#?}");
    // It now belongs to the turn that took it up, so a later continuation finds it there.
    assert_eq!(tasks[0].work.as_deref(), Some(session.work_of(1).as_str()));
    let texts = chat.texts();
    assert!(
        texts[1].contains(&format!("handed off to w (task {})", task.id)),
        "the model is told: {}",
        texts[1]
    );
    assert!(
        texts[2].contains("hand-off made before the interruption"),
        "{}",
        texts[2]
    );

    // The work ends once and reports to the chat once.
    gate.notify_one();
    let done = c.o.wait(&task.id).await.unwrap();
    assert_eq!(done.state, TaskState::Done);
    assert_eq!(c.posts(&id).len(), 1);
}

struct ConversationModels {
    source: Arc<Script>,
    colleague: Arc<Script>,
}

#[async_trait::async_trait]
impl Provider for ConversationModels {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> ark_model::Result<Completion> {
        let model = if system.contains("You are the agent \"colleague\"") {
            &self.colleague
        } else {
            &self.source
        };
        model.complete(system, messages, tools).await
    }
    fn describe(&self) -> String {
        "conversations".into()
    }
}

#[tokio::test]
async fn a_persistent_agent_creates_and_starts_a_colleague_without_copying_its_chat() {
    let dir = tempfile::tempdir().unwrap();
    let source = Script::new(
        "source",
        vec![
            calls(vec![(
                "agents.create",
                json!({"name":"colleague", "role":"Reviews documents", "instructions":"Review the supplied brief."}),
            )]),
            calls(vec![(
                "agents.start",
                json!({"to":"colleague", "objective":"Review this proposal", "context":"PUBLIC-BRIEF-ONLY"}),
            )]),
            calls(vec![(
                "agents.start",
                json!({"to":"colleague", "objective":"Review this proposal", "context":"PUBLIC-BRIEF-ONLY"}),
            )]),
            answer("The colleague has its own conversation."),
        ],
    );
    let colleague = Script::new(
        "colleague",
        vec![
            answer("REVIEW-DECISION-ALPHA"),
            answer("Continuing my review."),
        ],
    );
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(Arc::new(ConversationModels {
            source: source.clone(),
            colleague: colleague.clone(),
        }))
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("team", "test").unwrap();
    let o = Orchestrator::builder(&ark, &project).build().unwrap();
    o.spawn(AgentSpec::new("source")).await.unwrap();
    let done = o
        .run(
            "source",
            TaskSpec::new("Create a colleague and start its review. PRIVATE-SOURCE-SECRET"),
        )
        .await
        .unwrap();
    assert_eq!(done.state, TaskState::Done, "{:?}", done.error);
    let child = o
        .tasks()
        .into_iter()
        .find(|t| t.owner == "colleague")
        .expect("agents.start created a separate turn");
    assert_eq!(o.wait(&child.id).await.unwrap().state, TaskState::Done);
    assert!(done.children.is_empty());
    assert!(child.parent.is_none());
    let first = format!("{:?}", colleague.seen.lock().unwrap());
    assert!(first.contains("PUBLIC-BRIEF-ONLY"));
    assert!(!first.contains("PRIVATE-SOURCE-SECRET"));
    assert_eq!(
        o.tasks()
            .iter()
            .filter(|task| task.owner == "colleague")
            .count(),
        1,
        "distinct engine invocations of the same sender brief must converge"
    );
    let identity = o.agent("colleague").unwrap();
    let followup = o
        .run("colleague", TaskSpec::new("Continue yesterday's review"))
        .await
        .unwrap();
    assert_eq!(followup.state, TaskState::Done);
    assert_eq!(identity.session, o.agent("colleague").unwrap().session);
    let later = format!("{:?}", colleague.seen.lock().unwrap().last().unwrap());
    assert!(later.contains("REVIEW-DECISION-ALPHA"), "{later}");
}

#[tokio::test]
async fn client_creation_provenance_survives_restart_and_preserves_later_edits() {
    let f = fixture();
    let model = Script::new("model", vec![]);
    let o = orch(&f, vec![("model", model.clone())], vec![]);
    let created = o
        .create_once(AgentSpec::new("writer"), "device:epoch:1")
        .await
        .unwrap();
    assert_eq!(created.creation_request.as_deref(), Some("device:epoch:1"));
    assert!(!created.spec.allow_mutate);
    assert!(created.spec.delegates_to.is_empty());
    let mut edited = created.spec.clone();
    edited.role = "Later Desktop edit".into();
    o.spawn(edited).await.unwrap();
    drop(o);
    let o = orch(&f, vec![("model", model.clone())], vec![]);
    let replay = o
        .create_once(AgentSpec::new("writer"), "device:epoch:1")
        .await
        .unwrap();
    assert_eq!(replay.scope, created.scope);
    assert_eq!(replay.spec.role, "Later Desktop edit");
    assert!(o
        .create_once(AgentSpec::new("writer"), "other:epoch:1")
        .await
        .is_err());
    assert_eq!(o.agents().len(), 1);
    assert!(o.tasks().is_empty());
    assert!(model.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn client_create_racing_ordinary_spawn_never_adopts_foreign_provenance() {
    let f = fixture();
    let model = Script::new("model", vec![]);
    let o = orch(&f, vec![("model", model.clone())], vec![]);
    let (created, ordinary) = tokio::join!(
        o.create_once(AgentSpec::new("writer"), "client:1"),
        o.spawn(AgentSpec::new("writer"))
    );
    ordinary.unwrap();
    let record = o.agent("writer").unwrap();
    if created.is_ok() {
        assert_eq!(record.creation_request.as_deref(), Some("client:1"));
    } else {
        assert_eq!(record.creation_request, None);
    }
    assert!(o.tasks().is_empty());
}

#[tokio::test]
async fn completed_model_effect_replay_preserves_newer_edits_and_busy_state() {
    let f = fixture();
    let model = Script::new("model", vec![]);
    let o = orch(&f, vec![("model", model.clone())], vec![]);
    o.spawn(AgentSpec::new("writer").model("model"))
        .await
        .unwrap();
    o.select_idle_model("writer", "model", "phone:1")
        .await
        .unwrap();
    let mut edited = o.agent("writer").unwrap().spec;
    edited.role = "Desktop edit".into();
    o.spawn(edited).await.unwrap();
    o.suspend("writer").unwrap();
    drop(o);
    let o = orch(&f, vec![("model", model.clone())], vec![]);
    let replay = o
        .select_idle_model("writer", "model", "phone:1")
        .await
        .unwrap();
    assert_eq!(replay.spec.role, "Desktop edit");
    assert_eq!(replay.state, AgentState::Suspended);
    assert!(o
        .select_idle_model("writer", "model", "phone:2")
        .await
        .is_err());
    assert!(o.tasks().is_empty());
    assert!(model.seen.lock().unwrap().is_empty());
}

fn narrated_diagram(text: &str) -> Completion {
    Completion {
        content: text.into(),
        ..calls(vec![(
            "diagram.create",
            json!({"kind": "sequence", "title": "Flow", "source": "sequenceDiagram\n  A->>B: request\n  B-->>A: reply"}),
        )])
    }
}

const STEP_NOTE: &str = "Checking the project files and verifying the seed leads.";
const STEP_NOTE_AGAIN: &str = "Reading the price lines now.";

#[tokio::test]
async fn a_delegated_worker_that_answers_with_a_note_after_the_nudge_ends_done_with_it() {
    for last in [STEP_NOTE_AGAIN, "Renamed 3 leads."] {
        delegated_note_ends_done(last).await;
    }
}

async fn delegated_note_ends_done(last: &str) {
    let f = fixture();
    let lead = Script::new(
        "lead",
        vec![
            calls(vec![delegate("drafter", "qualify the seed leads")]),
            answer("got it"),
        ],
    );
    let drafter = Script::new(
        "drafter",
        vec![
            narrated_diagram("Reading the seed file."),
            answer(STEP_NOTE),
            answer(last),
        ],
    );
    let o = orch(
        &f,
        vec![("lead", lead.clone()), ("drafter", drafter.clone())],
        vec![],
    );
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
    let t = o.submit("lead", TaskSpec::new("get the leads")).unwrap();
    o.wait(&t).await.unwrap();
    let worker = o
        .tasks()
        .into_iter()
        .find(|t| t.owner == "drafter")
        .expect("the delegated task");
    assert_eq!(worker.state, TaskState::Done);
    assert_eq!(worker.result.as_deref(), Some(last));
    assert_eq!(worker.error, None);
    assert_eq!(drafter.texts().len(), 3, "one nudge, no third call");
}

#[tokio::test]
async fn an_agents_prompt_says_whether_its_changes_ask_the_person_first() {
    let asked = "Your changes ask the person first: call the action and they approve it. You are not read-only.";
    let free = "You make changes without asking.";
    for gate in [false, true] {
        let f = fixture();
        let m = Script::new("m", vec![]);
        let model = m.clone();
        let mut b = Orchestrator::builder(&f.ark, &f.project)
            .configure_models(move |b| b.provider("m", model as Arc<dyn Provider>));
        if gate {
            b = b.approvals(Arc::new(ApprovalGate::new(Arc::new(Yes(Arc::new(
                AtomicUsize::new(0),
            ))))));
        }
        let o = b.build().unwrap();
        o.spawn(AgentSpec::new("asks").model("m")).await.unwrap();
        o.spawn(AgentSpec::new("acts").model("m").allow_mutate(true))
            .await
            .unwrap();
        for agent in ["asks", "acts"] {
            let t = o.submit(agent, TaskSpec::new("say hello")).unwrap();
            o.wait(&t).await.unwrap();
        }
        let seen = m.seen.lock().unwrap();
        let system = |agent: &str| {
            seen.iter()
                .map(|(s, _)| s.clone())
                .find(|s| s.contains(&format!("You are the agent \"{agent}\"")))
                .unwrap()
        };
        let (asks, acts) = (system("asks"), system("acts"));
        assert!(acts.contains(free) && !acts.contains(asked), "{acts}");
        if gate {
            assert!(asks.contains(asked) && !asks.contains(free), "{asks}");
        } else {
            assert!(
                !asks.contains(asked) && !asks.contains(free),
                "no one to ask, so no authority line: {asks}"
            );
        }
    }
}
