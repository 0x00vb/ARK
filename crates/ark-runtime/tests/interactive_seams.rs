//! The seams an interactive front-end relies on: streaming text, approvals through the gate,
//! cooperative cancellation, context reports, and session carry-over. All driven by the mock
//! provider; no live model.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ark_core::{ActionDescriptor, ExecutionMode, RiskTier};
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
            .get_research_state(&project, "")
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
        let script = tools_then(
            vec![call("c1", "notes.read"), call("c2", "notes.read")],
            "The notes say locket.",
        );
        let p: Arc<dyn Provider> = Arc::new(MockProvider::new("m", script));
        let out = Harness::new(ModelRouter::single(p))
            .planner(false)
            .critic(false)
            .ask(&engine, &profile(), &project, "read", |_| {})
            .await
            .unwrap();
        runs.push((out.metrics.tool_calls, out.metrics.duplicate_tool_calls));
    }
    // Each turn runs the read once and reuses it once within the turn; the second turn does not
    // trust the first turn's result, because the notes may have changed in between.
    assert_eq!(runs, vec![(1, 1), (1, 1)]);
}

#[tokio::test]
async fn steering_reaches_the_model_at_the_next_step() {
    let (_d, engine, project) = setup(Arc::default());
    let mock = Arc::new(MockProvider::new(
        "m",
        tools_then(vec![call("c1", "notes.read")], "Checked the door too."),
    ));
    let p: Arc<dyn Provider> = mock.clone();
    let steer = ark_runtime::Steering::new();
    let s2 = steer.clone();
    let mut delivered = Vec::new();
    let mut context_tokens = 0;
    let out = Harness::new(ModelRouter::single(p))
        .planner(false)
        .critic(false)
        .steering(Some(steer.clone()))
        .ask(
            &engine,
            &profile(),
            &project,
            "read the notes",
            |e| match e {
                // The user types while the tool runs.
                HarnessEvent::ToolCall { .. } => s2.push("also check the door"),
                HarnessEvent::ToolResult {
                    context_tokens: n, ..
                } => context_tokens = n,
                HarnessEvent::Steered { text, .. } => delivered.push(text),
                _ => {}
            },
        )
        .await
        .unwrap();
    assert_eq!(delivered, vec!["also check the door"]);
    assert_eq!(out.steered, delivered);
    assert!(
        context_tokens > 0,
        "a tool result reports what it adds to the context"
    );
    let second = &mock.recorded()[1];
    assert!(second
        .messages
        .iter()
        .any(|m| m.content.contains("also check the door")));
    assert!(steer.take_undelivered().is_empty());
}

#[tokio::test]
async fn steering_context_reaches_the_model_and_recovery_without_changing_visible_text() {
    let (_d, engine, project) = setup(Arc::default());
    let mock = Arc::new(MockProvider::new(
        "m",
        tools_then(vec![call("c1", "notes.read")], "Updated."),
    ));
    let steer = ark_runtime::Steering::new();
    let sending = steer.clone();
    let context = r#"{"resource":"notes","revision":7}"#;
    let mut delivered = Vec::new();
    let out = Harness::new(ModelRouter::single(mock.clone()))
        .planner(false)
        .critic(false)
        .steering(Some(steer.clone()))
        .ask(&engine, &profile(), &project, "read", |event| match event {
            HarnessEvent::ToolCall { .. } => {
                sending.push_with_context("Change this", context).unwrap()
            }
            HarnessEvent::Steered { text, context } => delivered.push((text, context)),
            _ => {}
        })
        .await
        .unwrap();
    assert_eq!(delivered, vec![("Change this".into(), context.into())]);
    assert!(mock.recorded()[1]
        .messages
        .iter()
        .any(|message| message.content.contains(context)));
    assert!(out.steered[0].contains(context));
    assert!(steer.take_undelivered_inputs().is_empty());
}

#[tokio::test]
async fn steering_during_the_answer_continues_the_turn() {
    let (_d, engine, project) = setup(Arc::default());
    let script = vec![
        Completion {
            content: "The notes are fine.".into(),
            tool_calls: vec![],
            usage: Usage::measured(50, 5),
        },
        Completion {
            content: "The door is locked too.".into(),
            tool_calls: vec![],
            usage: Usage::measured(60, 6),
        },
    ];
    let mock = Arc::new(MockProvider::new("m", script));
    let p: Arc<dyn Provider> = mock.clone();
    let steer = ark_runtime::Steering::new();
    let s2 = steer.clone();
    let mut pushed = false;
    let out = Harness::new(ModelRouter::single(p))
        .planner(false)
        .critic(false)
        .steering(Some(steer))
        .ask(&engine, &profile(), &project, "check the notes", |e| {
            if matches!(e, HarnessEvent::TextDelta { .. }) && !pushed {
                pushed = true;
                s2.push("and the door?");
            }
        })
        .await
        .unwrap();
    assert_eq!(out.answer, "The door is locked too.");
    assert_eq!(out.steered, vec!["and the door?"]);
    assert_eq!(
        mock.recorded().len(),
        2,
        "the model was asked again with the new message"
    );
}

#[tokio::test]
async fn steering_after_the_turn_is_returned_undelivered() {
    let (_d, engine, project) = setup(Arc::default());
    let p: Arc<dyn Provider> = Arc::new(MockProvider::new(
        "m",
        vec![Completion {
            content: "Done.".into(),
            tool_calls: vec![],
            usage: Usage::measured(50, 2),
        }],
    ));
    let steer = ark_runtime::Steering::new();
    let s2 = steer.clone();
    let out = Harness::new(ModelRouter::single(p))
        .planner(false)
        .critic(false)
        .steering(Some(steer.clone()))
        .ask(&engine, &profile(), &project, "hi", |e| {
            if matches!(e, HarnessEvent::Answer { .. }) {
                s2.push("too late");
            }
        })
        .await
        .unwrap();
    assert!(out.steered.is_empty());
    assert_eq!(steer.take_undelivered(), vec!["too late"]);
}

#[tokio::test]
async fn retrieved_context_is_reported_apart_from_tool_results() {
    let (_d, engine, project) = setup(Arc::default());
    let search = ToolCall {
        id: "s1".into(),
        name: "search_evidence".into(),
        arguments: serde_json::json!({ "query": "locket" }),
    };
    let script = vec![
        Completion {
            content: String::new(),
            tool_calls: vec![call("c1", "notes.read")],
            usage: Usage::measured(100, 5),
        },
        Completion {
            content: String::new(),
            tool_calls: vec![search],
            usage: Usage::measured(110, 5),
        },
        Completion {
            content: "Locket [inv:1].".into(),
            tool_calls: vec![],
            usage: Usage::measured(120, 5),
        },
    ];
    let p: Arc<dyn Provider> = Arc::new(MockProvider::new("m", script));
    let mut reports = Vec::new();
    Harness::new(ModelRouter::single(p))
        .planner(false)
        .critic(false)
        .ask(&engine, &profile(), &project, "find the locket", |e| {
            if let HarnessEvent::Context { report } = e {
                reports.push(report);
            }
        })
        .await
        .unwrap();
    assert_eq!(reports.len(), 3);
    assert_eq!(reports[1].retrieved, 0, "no lookup yet");
    assert!(
        reports[2].retrieved > 0,
        "the search result is counted as retrieved"
    );
    assert!(
        reports[2].tool_rounds > 0,
        "the tool result is still counted as a tool result"
    );
    let parts: u32 = reports[2].rows().iter().map(|(_, n)| n).sum();
    assert!(
        parts.abs_diff(reports[2].total) <= 16,
        "rows still cover the total"
    );
}

#[tokio::test]
async fn plan_cannot_mutate_even_with_an_allowing_gate() {
    let saves = Arc::new(AtomicUsize::new(0));
    let (_d, engine, project) = setup(saves.clone());
    let asked = Arc::new(AtomicUsize::new(0));
    let mock = Arc::new(MockProvider::new(
        "m",
        tools_then(vec![call("c1", "notes.save")], "Here is the plan."),
    ));
    let provider: Arc<dyn Provider> = mock.clone();
    Harness::new(ModelRouter::single(provider))
        .planner(false)
        .critic(false)
        .execution_mode(ExecutionMode::Plan)
        .approvals(Some(Arc::new(ApprovalGate::new(Arc::new(Always(
            ApprovalDecision::AllowOnce,
            asked.clone(),
        ))))))
        .ask(&engine, &profile(), &project, "save the notes", |_| {})
        .await
        .unwrap();
    assert_eq!(saves.load(Ordering::SeqCst), 0);
    assert_eq!(asked.load(Ordering::SeqCst), 0);
    assert!(mock.recorded()[1]
        .messages
        .iter()
        .any(|message| message.content.contains("unavailable in Plan")));
}

#[tokio::test]
async fn manual_without_an_approval_gate_refuses_mutation() {
    let saves = Arc::new(AtomicUsize::new(0));
    let (_d, engine, project) = setup(saves.clone());
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(
        "m",
        tools_then(vec![call("c1", "notes.save")], "Approval is needed."),
    ));
    let mut needs_approval = 0;
    Harness::new(ModelRouter::single(provider))
        .planner(false)
        .critic(false)
        .execution_mode(ExecutionMode::Manual)
        .ask(&engine, &profile(), &project, "save the notes", |event| {
            if matches!(event, HarnessEvent::NeedsApproval { .. }) {
                needs_approval += 1;
            }
        })
        .await
        .unwrap();
    assert_eq!(saves.load(Ordering::SeqCst), 0);
    assert_eq!(needs_approval, 1);
}

#[tokio::test]
async fn auto_with_nobody_to_ask_refuses_what_is_not_contained() {
    // A one-shot client (`ask: false`) has no approval gate: Auto runs only `auto_eligible`.
    let saves = Arc::new(AtomicUsize::new(0));
    let (_d, engine, project) = setup(saves.clone());
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(
        "m",
        tools_then(vec![call("c1", "notes.save")], "Approval is needed."),
    ));
    let mut needs_approval = 0;
    Harness::new(ModelRouter::single(provider))
        .planner(false)
        .critic(false)
        .execution_mode(ExecutionMode::Auto)
        .ask(&engine, &profile(), &project, "save the notes", |event| {
            if matches!(event, HarnessEvent::NeedsApproval { .. }) {
                needs_approval += 1;
            }
        })
        .await
        .unwrap();
    assert_eq!(saves.load(Ordering::SeqCst), 0, "the change did not run");
    assert_eq!(needs_approval, 1);
}

struct AlwaysAskNotes(Notes);

#[async_trait::async_trait]
impl Adapter for AlwaysAskNotes {
    fn name(&self) -> &str {
        "notes"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new("notes.save", "save", RiskTier::Mutate)
                .auto_eligible()
                .ask_each_time(),
        ]
    }
    async fn run(
        &self,
        action: &str,
        input: serde_json::Value,
        ctx: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        self.0.run(action, input, ctx).await
    }
}

#[tokio::test]
async fn auto_asks_only_for_what_asks_at_every_call() {
    // `auto_eligible` is for unattended runs: a chat in Auto does not need it.
    for always_ask in [false, true] {
        let saves = Arc::new(AtomicUsize::new(0));
        let dir = tempfile::tempdir().unwrap();
        let notes = Notes {
            saves: saves.clone(),
        };
        let adapter: Arc<dyn Adapter> = if always_ask {
            Arc::new(AlwaysAskNotes(notes))
        } else {
            Arc::new(notes)
        };
        let engine = Engine::open_with(dir.path(), Registry::new().with(adapter)).unwrap();
        let project = engine.create_project("t", "").unwrap();
        let asked = Arc::new(AtomicUsize::new(0));
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(
            "m",
            tools_then(vec![call("c1", "notes.save")], "Declined."),
        ));
        Harness::new(ModelRouter::single(provider))
            .planner(false)
            .critic(false)
            .execution_mode(ExecutionMode::Auto)
            .approvals(Some(Arc::new(ApprovalGate::new(Arc::new(Always(
                ApprovalDecision::Deny,
                asked.clone(),
            ))))))
            .ask(&engine, &profile(), &project.id, "save the notes", |_| {})
            .await
            .unwrap();
        let (asks, runs) = if always_ask { (1, 0) } else { (0, 1) };
        assert_eq!(
            asked.load(Ordering::SeqCst),
            asks,
            "always_ask={always_ask}"
        );
        assert_eq!(
            saves.load(Ordering::SeqCst),
            runs,
            "always_ask={always_ask}"
        );
    }
}
