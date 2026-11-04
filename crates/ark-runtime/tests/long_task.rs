//! A long task never loses its goal (ADR 0050). Many steps with large results under a small
//! request budget drop old rounds; every request the model gets still holds the goal, what the
//! person said while it ran, and a ledger of what the dropped steps did. A vendor session that
//! rotates starts from such a request, so it gets them too.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ark_core::{ActionDescriptor, RiskTier};
use ark_model::session::{Base, Delta, SessionBackend, SessionPolicy, SubscriptionRuntime};
use ark_model::{Completion, Message, MockProvider, Provider, ToolCall, ToolDef, Usage};
use ark_runtime::session::Goal;
use ark_runtime::{
    AgentProfile, DistillerSet, Engine, Harness, HarnessEvent, ModelRouter, Steering,
};
use ark_tools::{ActionOutput, Adapter, Registry, RunCtx};
use serde_json::json;

const REQUEST: &str =
    "On the editor file, support syntax highlighting according to the user's theme";
const STEER: &str = "use the theme's own colour tokens, not new ones";
const CALLS: usize = 80;

/// `docs.read`: a page of about 1,500 tokens.
struct Docs;

#[async_trait::async_trait]
impl Adapter for Docs {
    fn name(&self) -> &str {
        "docs"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            "docs.read",
            "read a page",
            RiskTier::Read,
        )]
    }
    async fn run(
        &self,
        _a: &str,
        i: serde_json::Value,
        _c: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        let page = i["path"].as_str().unwrap_or("").to_string();
        Ok(ActionOutput::json(json!({
            "path": page,
            "body": format!("{page}: {}", "const token = theme.colour; ".repeat(200)),
        })))
    }
}

/// The page whole, as a file read gives a model its text.
fn profile() -> AgentProfile {
    AgentProfile::builder()
        .core_actions(["docs.read"])
        .distillers(DistillerSet::with_common().with(Arc::new(
            |a: &str, out: &serde_json::Value| {
                (a == "docs.read").then(|| {
                    (
                        out["body"].as_str().unwrap_or("").to_string(),
                        vec![out["path"].as_str().unwrap_or("").to_string()],
                    )
                })
            },
        )))
        .build()
}

/// Step `n` of the scripted model: a read of page `n` with its plan, then the answer.
fn step(n: usize) -> Completion {
    if n >= CALLS {
        return Completion {
            content: "Highlighting is in place.".into(),
            tool_calls: vec![],
            usage: Usage::default(),
        };
    }
    Completion {
        content: format!("Reading web/js/part{n}.js to see how the editor draws."),
        tool_calls: vec![ToolCall {
            id: format!("c{n}"),
            name: "docs.read".into(),
            arguments: json!({ "path": format!("web/js/part{n}.js") }),
        }],
        usage: Usage::default(),
    }
}

fn all_text(messages: &[Message]) -> String {
    messages
        .iter()
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn engine() -> (tempfile::TempDir, Engine, ark_core::ProjectId) {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open_with(dir.path(), Registry::new().with(Arc::new(Docs))).unwrap();
    let project = engine.create_project("editor", "").unwrap().id;
    (dir, engine, project)
}

/// A turn that continues an unfinished one ("cointinue"), 80 large reads under a 6,000 budget,
/// with a message from the person midway.
#[tokio::test]
async fn a_long_task_keeps_its_goal_what_the_person_said_and_what_it_did() {
    let (_dir, engine, project) = engine();
    let steer = Steering::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let (s, c) = (steer.clone(), calls.clone());
    let model = Arc::new(MockProvider::new("m", vec![]).with_fallback(move |_| {
        let n = c.fetch_add(1, Ordering::SeqCst);
        if n == CALLS / 2 {
            s.push(STEER);
        }
        step(n)
    }));
    let mut goal = Goal::new(REQUEST);
    goal.then("cointinue");
    let mut steered = Vec::new();
    let mut dropped = 0;
    let out = Harness::new(ModelRouter::single(model.clone() as Arc<dyn Provider>))
        .planner(false)
        .critic(false)
        .budget(6_000)
        .max_steps(CALLS + 5)
        .goal(Some(goal))
        .steering(Some(steer))
        .ask(&engine, &profile(), &project, "cointinue", |e| match e {
            HarnessEvent::Steered { text, .. } => steered.push(text),
            HarnessEvent::Context { report } => dropped = dropped.max(report.compressed_rounds),
            _ => {}
        })
        .await
        .unwrap();
    assert_eq!(out.answer, "Highlighting is in place.");
    assert_eq!(steered, [STEER]);
    assert!(dropped > 60, "most rounds were dropped: {dropped}");

    let requests = model.recorded();
    assert_eq!(requests.len(), CALLS + 1);
    let mut steer_seen_from = None;
    for (i, r) in requests.iter().enumerate() {
        let text = all_text(&r.messages);
        assert!(text.contains(REQUEST), "request {i} lost the goal");
        assert!(
            text.contains("cointinue"),
            "request {i} lost the person's newest message"
        );
        assert!(
            r.messages[0].content.contains(REQUEST),
            "request {i}: the goal is the first message"
        );
        if steer_seen_from.is_none() && text.contains(STEER) {
            steer_seen_from = Some(i);
        }
        if let Some(from) = steer_seen_from {
            let tail = &r.messages.last().unwrap().content;
            assert!(
                tail.contains(STEER),
                "request {i}: what the person said stays after its round is dropped (seen from {from})"
            );
        }
        if i > 10 {
            assert!(
                text.contains("[Earlier steps of this task"),
                "request {i} has no ledger"
            );
            // The ledger names the reads it stands for.
            assert!(text.contains("docs.read path=web/js/part"), "request {i}");
            assert!(
                text.contains("Your last note in those steps"),
                "request {i}"
            );
        }
    }
    assert!(
        steer_seen_from.is_some(),
        "the person's message reached the model"
    );
    let state = engine.store.get_research_state(&project).unwrap().unwrap();
    assert_eq!(
        state.goal, REQUEST,
        "the research state's goal is the request"
    );
    let peak = model.peak_prompt_tokens();
    assert!(
        peak < 6_000 + 3_000,
        "requests stay near the budget: {peak}"
    );
}

/// A vendor session over a scripted backend: it keeps what it was sent and reports the
/// thread's size, so the managed session rotates; every new vendor session must start with
/// the goal.
struct Vendor {
    opened: Mutex<Vec<Vec<Message>>>,
    thread: AtomicUsize,
    step: AtomicUsize,
}

impl Vendor {
    fn reply(&self, sent: u32) -> Completion {
        let size = self.thread.fetch_add(sent as usize, Ordering::SeqCst) + sent as usize;
        let mut c = step(self.step.fetch_add(1, Ordering::SeqCst));
        c.usage = Usage::measured(size as u32, 50);
        c
    }
}

fn tokens(messages: &[Message]) -> u32 {
    messages
        .iter()
        .map(|m| ark_model::estimate_tokens(&m.content) + 4)
        .sum()
}

#[async_trait::async_trait]
impl SessionBackend for Vendor {
    fn label(&self) -> String {
        "vendor".into()
    }
    fn model(&self) -> String {
        "vendor:m".into()
    }
    fn account(&self) -> String {
        "vendor".into()
    }
    async fn open(
        &self,
        _system: &str,
        _tools: &[ToolDef],
        messages: &[Message],
    ) -> ark_model::Result<(String, Completion)> {
        self.opened.lock().unwrap().push(messages.to_vec());
        self.thread.store(0, Ordering::SeqCst);
        let n = self.opened.lock().unwrap().len();
        Ok((format!("v{n}"), self.reply(tokens(messages))))
    }
    async fn resume(
        &self,
        _vendor: &str,
        _base: Base<'_>,
        delta: &Delta,
    ) -> ark_model::Result<Completion> {
        Ok(self.reply(tokens(&delta.messages)))
    }
}

#[tokio::test]
async fn a_vendor_session_that_rotates_starts_from_the_goal() {
    let (_dir, engine, project) = engine();
    let rt = SubscriptionRuntime::builder()
        .policy(SessionPolicy {
            max_context_tokens: 12_000,
            ..Default::default()
        })
        .build();
    let vendor = Arc::new(Vendor {
        opened: Mutex::new(Vec::new()),
        thread: AtomicUsize::new(0),
        step: AtomicUsize::new(0),
    });
    let session = rt.session_with("chat", vendor.clone()).await;
    let out = Harness::new(ModelRouter::single(session as Arc<dyn Provider>))
        .planner(false)
        .critic(false)
        .budget(6_000)
        .max_steps(CALLS + 5)
        .ask(&engine, &profile(), &project, REQUEST, |_| {})
        .await
        .unwrap();
    assert_eq!(out.answer, "Highlighting is in place.");
    let opened = vendor.opened.lock().unwrap();
    assert!(
        opened.len() > 3,
        "the vendor session rotated: {}",
        opened.len()
    );
    for (i, messages) in opened.iter().enumerate() {
        assert!(
            messages[0].content.contains(REQUEST),
            "vendor session {i} started without the goal"
        );
        if i > 0 {
            assert!(
                all_text(messages).contains("[Earlier steps of this task"),
                "vendor session {i} started without the ledger"
            );
        }
    }
}

/// A small model (an 8k window): the budget is half of it, the goal's request is clipped to
/// fit, and no request the model gets is over its window.
#[tokio::test]
async fn a_small_window_is_never_overrun() {
    let (_dir, engine, project) = engine();
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let model = Arc::new(
        MockProvider::new("small", vec![])
            .with_context_window(8_192)
            .with_fallback(move |_| {
                step(c.fetch_add(1, Ordering::SeqCst).saturating_add(CALLS - 30))
            }),
    );
    let mut goal = Goal::new(format!(
        "{REQUEST}. {}",
        "Keep every existing behaviour. ".repeat(300)
    ));
    for i in 0..12 {
        goal.then(&format!("and also point {i}: {}", "details ".repeat(80)));
    }
    let harness = Harness::new(ModelRouter::single(model.clone() as Arc<dyn Provider>))
        .planner(false)
        .critic(false)
        .max_steps(40)
        .goal(Some(goal));
    assert_eq!(harness.request_budget(), 4_096, "half the window");
    let out = harness
        .ask(&engine, &profile(), &project, "continue", |_| {})
        .await
        .unwrap();
    assert_eq!(out.answer, "Highlighting is in place.");
    let peak = model.peak_prompt_tokens();
    assert!(peak < 8_192, "a request went over the window: {peak}");
    let head = &model.recorded()[0].messages[0].content;
    assert!(head.contains(REQUEST), "the request leads, clipped: {head}");
    assert!(head.contains("(+8 earlier messages)"), "{head}");
}

/// A turn that reaches its step limit answers, and says the work may not be done, so the next
/// turn continues its goal.
#[tokio::test]
async fn a_turn_at_its_step_limit_says_it_ran_out() {
    let (_dir, engine, project) = engine();
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let model = Arc::new(MockProvider::new("m", vec![]).with_fallback(move |r| {
        if r.tools.is_empty() {
            return Completion {
                content: "Here is where I got to.".into(),
                tool_calls: vec![],
                usage: Usage::default(),
            };
        }
        step(c.fetch_add(1, Ordering::SeqCst))
    }));
    let out = Harness::new(ModelRouter::single(model as Arc<dyn Provider>))
        .planner(false)
        .critic(false)
        .max_steps(3)
        .ask(&engine, &profile(), &project, REQUEST, |_| {})
        .await
        .unwrap();
    assert!(out.ran_out, "the limit ended the work");
    assert!(!out.answer.is_empty());

    let model = Arc::new(MockProvider::new("m", vec![step(CALLS)]));
    let out = Harness::new(ModelRouter::single(model as Arc<dyn Provider>))
        .planner(false)
        .critic(false)
        .ask(&engine, &profile(), &project, REQUEST, |_| {})
        .await
        .unwrap();
    assert!(!out.ran_out, "its own answer");
}
