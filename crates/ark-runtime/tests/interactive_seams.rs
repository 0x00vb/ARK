//! The seams an interactive front-end relies on: streaming text, approvals through the gate,
//! cooperative cancellation, context reports, and session carry-over. All driven by the mock
//! provider; no live model.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ark_core::{ActionDescriptor, RiskTier};
use ark_model::{Completion, MockProvider, Provider, ToolCall, Usage};
use ark_runtime::{
    AgentProfile, ApprovalDecision, ApprovalGate, ApprovalRequest, Approver, CancelToken, Engine,
    Harness, HarnessEvent, ModelRouter, Session,
};
use ark_tools::{ActionOutput, Adapter, Registry, RunCtx};

struct Notes {
    saves: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Adapter for Notes {
    fn name(&self) -> &str {
        "notes"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new("notes.read", "read the notes", RiskTier::Read),
            ActionDescriptor::new("notes.save", "save the notes", RiskTier::Mutate)
                .with_input_schema(serde_json::json!({
                    "type": "object", "properties": { "text": { "type": "string" } }
                })),
        ]
    }
    async fn run(
        &self,
        action: &str,
        _input: serde_json::Value,
        _c: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        if action == "notes.save" {
            self.saves.fetch_add(1, Ordering::SeqCst);
            return Ok(ActionOutput::json(serde_json::json!({ "saved": true })));
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
        Ok(ActionOutput::json(
            serde_json::json!({ "text": "remember the locket" }),
        ))
    }
}

fn call(id: &str, name: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: serde_json::json!({ "text": "hello" }),
    }
}

fn tools_then(calls: Vec<ToolCall>, answer: &str) -> Vec<Completion> {
    vec![
        Completion {
            content: String::new(),
            tool_calls: calls,
            usage: Usage::measured(100, 5),
        },
        Completion {
            content: answer.into(),
            tool_calls: vec![],
            usage: Usage::measured(120, 10),
        },
    ]
}

fn setup(saves: Arc<AtomicUsize>) -> (tempfile::TempDir, Engine, ark_core::ProjectId) {
    let dir = tempfile::tempdir().unwrap();
    let engine =
        Engine::open_with(dir.path(), Registry::new().with(Arc::new(Notes { saves }))).unwrap();
    let p = engine.create_project("t", "").unwrap();
    (dir, engine, p.id)
}

fn profile() -> AgentProfile {
    AgentProfile::builder()
        .core_actions(["notes.read", "notes.save"])
        .build()
}

struct Always(ApprovalDecision, Arc<AtomicUsize>);
#[async_trait::async_trait]
impl Approver for Always {
    async fn decide(&self, _r: &ApprovalRequest) -> ApprovalDecision {
        self.1.fetch_add(1, Ordering::SeqCst);
        self.0
    }
}

#[tokio::test]
async fn answer_text_streams_as_deltas_and_is_marked_streamed() {
    let (_d, engine, project) = setup(Arc::default());
    let script = vec![Completion {
        content: "The notes mention a locket.".into(),
        tool_calls: vec![],
        usage: Usage::measured(50, 6),
    }];
    let p: Arc<dyn Provider> = Arc::new(MockProvider::new("m", script));
    let h = Harness::new(ModelRouter::single(p))
        .planner(false)
        .critic(false);
    let mut deltas = String::new();
    let mut final_streamed = None;
    let out = h
        .ask(
            &engine,
            &profile(),
            &project,
            "what do the notes say",
            |e| match e {
                HarnessEvent::TextDelta { text } => deltas.push_str(&text),
                HarnessEvent::Answer { streamed, .. } => final_streamed = Some(streamed),
                _ => {}
            },
        )
        .await
        .unwrap();
    assert_eq!(deltas, "The notes mention a locket.");
    assert_eq!(out.answer, deltas);
    assert_eq!(final_streamed, Some(true));
}

#[tokio::test]
async fn approval_gate_allows_and_remembers_session_grants() {
    let saves = Arc::new(AtomicUsize::new(0));
    let (_d, engine, project) = setup(saves.clone());
    let asked = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(ApprovalGate::new(Arc::new(Always(
        ApprovalDecision::AllowSession,
        asked.clone(),
    ))));

    for _ in 0..2 {
        let p: Arc<dyn Provider> = Arc::new(MockProvider::new(
            "m",
            tools_then(vec![call("c1", "notes.save")], "Saved."),
        ));
        let h = Harness::new(ModelRouter::single(p))
            .planner(false)
            .critic(false)
            .approvals(Some(gate.clone()));
        let mut requested = 0;
        h.ask(&engine, &profile(), &project, "save the notes", |e| {
            if matches!(e, HarnessEvent::ApprovalRequested { .. }) {
                requested += 1;
            }
        })
        .await
        .unwrap();
        assert!(requested <= 1);
    }
    assert_eq!(
        saves.load(Ordering::SeqCst),
        2,
        "both saves ran (never deduplicated)"
    );
    assert_eq!(
        asked.load(Ordering::SeqCst),
        1,
        "the human was asked once; the grant covered the second"
    );
    assert_eq!(gate.grants(), vec!["notes.save".to_string()]);
}

#[tokio::test]
async fn denied_approval_does_not_run_and_tells_the_model() {
    let saves = Arc::new(AtomicUsize::new(0));
    let (_d, engine, project) = setup(saves.clone());
    let gate = Arc::new(ApprovalGate::new(Arc::new(Always(
        ApprovalDecision::Deny,
        Arc::default(),
    ))));
    let mock = Arc::new(MockProvider::new(
        "m",
        tools_then(vec![call("c1", "notes.save")], "Okay, not saved."),
    ));
    let p: Arc<dyn Provider> = mock.clone();
    let h = Harness::new(ModelRouter::single(p))
        .planner(false)
        .critic(false)
        .approvals(Some(gate));
    let mut resolved = None;
    h.ask(&engine, &profile(), &project, "save the notes", |e| {
        if let HarnessEvent::ApprovalResolved { decision, .. } = e {
            resolved = Some(decision);
        }
    })
    .await
    .unwrap();
    assert_eq!(saves.load(Ordering::SeqCst), 0);
    assert_eq!(resolved, Some(ApprovalDecision::Deny));
    let second = &mock.recorded()[1];
    assert!(
        second
            .messages
            .iter()
            .any(|m| m.content.contains("declined")),
        "the model learns it was declined"
    );
}

#[tokio::test]
async fn profile_classifier_keys_grants() {
    let saves = Arc::new(AtomicUsize::new(0));
    let (_d, engine, project) = setup(saves);
    let gate = Arc::new(ApprovalGate::new(Arc::new(Always(
        ApprovalDecision::AllowSession,
        Arc::default(),
    ))));
    let profile = AgentProfile::builder()
        .core_actions(["notes.save"])
        .approval_classifier(|action, input| {
            format!("{action}:{}", input["text"].as_str().unwrap_or(""))
        })
        .build();
    let p: Arc<dyn Provider> = Arc::new(MockProvider::new(
        "m",
        tools_then(vec![call("c1", "notes.save")], "ok"),
    ));
    Harness::new(ModelRouter::single(p))
        .planner(false)
        .critic(false)
        .approvals(Some(gate.clone()))
        .ask(&engine, &profile, &project, "save", |_| {})
        .await
        .unwrap();
    assert_eq!(gate.grants(), vec!["notes.save:hello".to_string()]);
}

#[tokio::test]
async fn cancellation_stops_mid_stream_and_saves_state() {
    let (_d, engine, project) = setup(Arc::default());
    let long = "word ".repeat(400);
    let script = vec![Completion {
        content: long,
        tool_calls: vec![],
        usage: Usage::measured(50, 400),
    }];
    let p: Arc<dyn Provider> =
        Arc::new(MockProvider::new("m", script).with_chunk_delay(Duration::from_millis(2)));
    let token = CancelToken::new();
    let h = Harness::new(ModelRouter::single(p))
        .planner(false)
        .critic(false)
        .cancel(Some(token.clone()));
    let t2 = token.clone();
    let mut deltas = 0;
    let mut saw_cancel = false;
    let out = h
        .ask(&engine, &profile(), &project, "write a lot", |e| match e {
            HarnessEvent::TextDelta { .. } => {
                deltas += 1;
                if deltas == 10 {
                    t2.cancel();
                }
            }
            HarnessEvent::Cancelled => saw_cancel = true,
            HarnessEvent::Answer { .. } => panic!("no answer after cancel"),
            _ => {}
        })
        .await
        .unwrap();
    assert!(out.cancelled);
    assert!(saw_cancel);
    assert!(deltas < 400, "stopped early ({deltas} chunks)");
    assert_eq!(
        engine
            .store
            .get_research_state(&project)
            .unwrap()
            .unwrap()
            .goal,
        "write a lot"
    );
}

#[tokio::test]
async fn context_reports_attribute_every_request() {
    let (_d, engine, project) = setup(Arc::default());
    let p: Arc<dyn Provider> = Arc::new(MockProvider::new(
        "m",
        tools_then(vec![call("c1", "notes.read")], "It says locket [inv:1]."),
    ));
    let h = Harness::new(ModelRouter::single(p))
        .planner(false)
        .critic(false)
        .conversation(Some(
            "  [user] earlier question\n  [you] earlier answer".into(),
        ));
    let mut reports = Vec::new();
    h.ask(
        &engine,
        &profile(),
        &project,
        "what do the notes say",
        |e| {
            if let HarnessEvent::Context { report } = e {
                reports.push(report);
            }
        },
    )
    .await
    .unwrap();
    assert_eq!(reports.len(), 2, "one report per request");
    let (first, second) = (&reports[0], &reports[1]);
    assert!(first.conversation > 0 && first.task > 0 && first.instructions > 0);
    assert_eq!(first.tool_rounds, 0);
    assert!(
        second.tool_rounds > 0,
        "the tool result is in the second request"
    );
    for r in &reports {
        let parts: u32 = r.rows().iter().map(|(_, n)| n).sum();
        // The parts cover the estimated total (a few framing tokens are unattributed).
        assert!(
            parts.abs_diff(r.total) <= 16,
            "parts {parts} vs total {}",
            r.total
        );
    }
    let preview = h
        .preview_context(&engine, &profile(), &project, "")
        .await
        .unwrap();
    assert_eq!(preview.step, 0);
    assert!(preview.conversation > 0);
}

#[tokio::test]
async fn session_carry_over_reaches_the_model_bounded() {
    let (_d, engine, project) = setup(Arc::default());
    let mock = Arc::new(
        MockProvider::new("m", vec![]).with_fallback(|_| Completion {
            content: "A reply that is fairly long. ".repeat(30),
            tool_calls: vec![],
            usage: Usage::default(),
        }),
    );
    let p: Arc<dyn Provider> = mock.clone();
    let router = ModelRouter::single(p);
    let mut session = Session::start(&engine, &project).unwrap();
    let mut first_msg_sizes = Vec::new();
    for i in 0..60 {
        let h = Harness::new(router.clone_view())
            .planner(false)
            .critic(false)
            .conversation(session.conversation_digest());
        let q = format!("turn {i}: continue the chapter");
        let out = h
            .ask(&engine, &profile(), &project, &q, |_| {})
            .await
            .unwrap();
        session.record_turn(&engine, &q, &out).unwrap();
        let last = mock.recorded().last().unwrap().clone();
        first_msg_sizes.push(last.messages[0].content.len());
    }
    let recorded = mock.recorded();
    assert!(recorded.last().unwrap().messages[0]
        .content
        .contains("EARLIER IN THIS CONVERSATION"));
    assert!(
        first_msg_sizes[59] <= first_msg_sizes[15] + 64,
        "carry-over plateaus: {} vs {}",
        first_msg_sizes[15],
        first_msg_sizes[59]
    );
}

#[tokio::test]
async fn workspace_reads_are_not_reused_across_turns() {
    let (_d, engine, project) = setup(Arc::default());
    let mut runs = Vec::new();
    for _ in 0..2 {
        let script = tools_then(vec![call("c1", "notes.read"), call("c2", "notes.read")], "The notes say locket.");
        let p: Arc<dyn Provider> = Arc::new(MockProvider::new("m", script));
        let out = Harness::new(ModelRouter::single(p)).planner(false).critic(false).ask(&engine, &profile(), &project, "read", |_| {}).await.unwrap();
        runs.push((out.metrics.tool_calls, out.metrics.duplicate_tool_calls));
    }
    // Each turn runs the read once and reuses it once within the turn; the second turn does not
    // trust the first turn's result, because the notes may have changed in between.
    assert_eq!(runs, vec![(1, 1), (1, 1)]);
}
