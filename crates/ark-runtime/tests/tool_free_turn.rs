use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use ark_core::{ActionDescriptor, MemorySource, RiskTier, RuleRef, SessionId};
use ark_model::{Completion, MockProvider, Provider, ToolCall, Usage};
use ark_runtime::{AgentProfile, Engine, Harness, HarnessEvent, ModelRouter, RuleSelection};
use ark_tools::{ActionOutput, Adapter, Registry, RunCtx};
use serde_json::json;

const TOOL_FREE_RULE: &str = "Do not call tools. For a simple exact-output request, answer directly with only the requested content; add no explanation or tool-use instructions.";
const TOOL_COACHING: &str = "Actions that change state need explicit human approval; observation and analysis are free. Reason from what tools return. For an action you are allowed to propose, call it to request any required approval. The host handles approval before execution. Do not substitute a text-only permission request.";

struct Probe(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Adapter for Probe {
    fn name(&self) -> &str {
        "probe"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            "probe.read",
            "Read a probe value",
            RiskTier::Read,
        )]
    }
    async fn run(
        &self,
        _action: &str,
        _input: serde_json::Value,
        _ctx: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ActionOutput::json(json!({"value": 42})))
    }
}

fn answer(calls: Vec<ToolCall>) -> Completion {
    Completion {
        content: "The report is ready.".into(),
        tool_calls: calls,
        usage: Usage::measured(10, 5),
    }
}

fn harness(model: Arc<dyn Provider>) -> Harness {
    Harness::new(ModelRouter::single(model)).budget(6000)
}

#[tokio::test]
async fn explicit_tool_free_turn_preserves_context_and_matches_preview() {
    let dir = tempfile::tempdir().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let engine = Engine::open_with(
        dir.path(),
        Registry::new().with(Arc::new(Probe(runs.clone()))),
    )
    .unwrap();
    let project = engine
        .create_project("report", "retained project target")
        .unwrap();
    let session = SessionId::new();
    engine
        .store
        .create_session(session.as_str(), &project.id, &json!({}))
        .unwrap();
    engine
        .store
        .record_memory(
            &project.id,
            "report",
            "RETAINED_MEMORY",
            MemorySource::Person { session },
            None,
        )
        .unwrap();
    let model = Arc::new(MockProvider::new("mock", vec![answer(vec![])]));
    let profile = AgentProfile::builder()
        .instructions("RETAINED_INSTRUCTIONS")
        .core_actions(["probe.read"])
        .build();
    let h = harness(model.clone())
        .conversation(Some("RETAINED_CONVERSATION".into()))
        .rules(Some(RuleSelection {
            applied: vec![RuleRef {
                set: "fixture".into(),
                version: 1,
                id: "retained".into(),
            }],
            held_back: 0,
            text: "RETAINED_RULES".into(),
        }))
        .skills(Some("RETAINED_SKILLS".into()));
    let question = "Explain why the report differs. Do not use tools.";
    let preview = h
        .preview_context(&engine, &profile, &project.id, question)
        .await
        .unwrap();
    let mut contexts = Vec::new();
    let result = h
        .ask(&engine, &profile, &project.id, question, |event| {
            if let HarnessEvent::Context { report } = event {
                contexts.push(report);
            }
        })
        .await
        .unwrap();
    assert_eq!(result.answer, "The report is ready.");
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    let recorded = model.recorded();
    assert_eq!(recorded.len(), 1, "this answer needs one reasoning call");
    assert!(recorded[0].tools.is_empty());
    assert!(recorded[0].system.contains("RETAINED_INSTRUCTIONS"));
    assert!(recorded[0].system.contains("retained project target"));
    assert!(!recorded[0].system.contains("probe.read"));
    assert!(recorded[0].system.contains(TOOL_FREE_RULE));
    assert!(!recorded[0].system.contains(TOOL_COACHING));
    let messages = serde_json::to_string(&recorded[0].messages).unwrap();
    for retained in [
        "Explain why the report differs.",
        "RETAINED_CONVERSATION",
        "RETAINED_RULES",
        "RETAINED_MEMORY",
        "RETAINED_SKILLS",
    ] {
        assert!(messages.contains(retained), "missing {retained}");
    }
    assert!(!messages.contains(question));
    assert_eq!(result.state.goal, question);
    assert_eq!(contexts.len(), 1);
    assert_eq!(preview.instructions, contexts[0].instructions);
    for report in [&preview, &contexts[0]] {
        assert_eq!(report.capability_index, 0);
        assert_eq!(report.tool_schemas, 0);
        assert_eq!(report.action_schemas, 0);
        assert_eq!(report.loaded_actions, 0);
        assert!(report.memory > 0 && report.conversation > 0 && report.rules > 0);
    }
}

#[tokio::test]
async fn quoted_and_historical_directives_keep_tools_available() {
    for question in [
        "Explain the phrase \"Do not use tools.\"",
        "Use tools to read the report. Do not use tools.",
        "Explain the report.",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open_with(
            dir.path(),
            Registry::new().with(Arc::new(Probe(Arc::new(AtomicUsize::new(0))))),
        )
        .unwrap();
        let project = engine.create_project("report", "").unwrap();
        let model = Arc::new(MockProvider::new("mock", vec![answer(vec![])]));
        let profile = AgentProfile::builder().core_actions(["probe.read"]).build();
        let h = harness(model.clone())
            .planner(false)
            .critic(false)
            .conversation(Some("Earlier person: Do not use tools.".into()));
        let preview = h
            .preview_context(&engine, &profile, &project.id, question)
            .await
            .unwrap();
        h.ask(&engine, &profile, &project.id, question, |_| {})
            .await
            .unwrap();
        assert!(preview.capability_index > 0 && preview.tool_schemas > 0);
        assert!(!model.recorded()[0].system.contains(TOOL_FREE_RULE));
        assert!(model.recorded()[0].system.starts_with(&format!(
            "{}\n\n{TOOL_COACHING}\n\n",
            profile.prompts.instructions
        )));
        assert!(model.recorded()[0]
            .tools
            .iter()
            .any(|tool| tool.name == "probe.read"));
    }
}

#[tokio::test]
async fn exact_output_separates_current_control_but_keeps_stored_input_and_answer() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open_with(dir.path(), Registry::new()).unwrap();
    let project = engine.create_project("report", "").unwrap();
    let question = "Reply with exactly: Flint is running locally. Do not use tools.";
    let supplied_answer = "Flint is running locally. Do not use tools.";
    let model = Arc::new(MockProvider::new(
        "mock",
        vec![Completion {
            content: supplied_answer.into(),
            tool_calls: vec![],
            usage: Usage::measured(10, 5),
        }],
    ));
    let h = harness(model.clone());
    let profile = AgentProfile::default();
    let preview = h
        .preview_context(&engine, &profile, &project.id, question)
        .await
        .unwrap();
    let mut instructions = None;
    let outcome = h
        .ask(&engine, &profile, &project.id, question, |event| {
            if let HarnessEvent::Context { report } = event {
                instructions = Some(report.instructions);
            }
        })
        .await
        .unwrap();
    assert_eq!(
        outcome.answer, supplied_answer,
        "the host must not strip model output"
    );
    assert_eq!(Some(preview.instructions), instructions);
    let recorded = model.recorded();
    assert_eq!(recorded.len(), 1);
    assert!(recorded[0].tools.is_empty());
    assert!(recorded[0]
        .messages
        .iter()
        .any(|message| message.content == "Reply with exactly: Flint is running locally."));
    let messages = serde_json::to_string(&recorded[0].messages).unwrap();
    assert!(!messages.contains("Do not use tools."));
    assert!(messages.contains("GOAL: Reply with exactly: Flint is running locally."));
    assert_eq!(outcome.state.goal, question);
    assert_eq!(
        engine
            .store
            .get_research_state(&project.id)
            .unwrap()
            .unwrap()
            .goal,
        question
    );
    assert!(recorded[0].system.contains(TOOL_FREE_RULE));
    assert!(!recorded[0].system.contains(TOOL_COACHING));
    assert!(!recorded[0].system.contains("Flint is running locally."));
}

#[tokio::test]
async fn unsolicited_action_or_meta_call_is_refused_before_dispatch() {
    for (name, max_steps) in [
        ("probe.read", 1),
        ("record_state", 1),
        ("load_action", 1),
        ("acquire_capability", 1),
        ("probe.read", 0),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let runs = Arc::new(AtomicUsize::new(0));
        let engine = Engine::open_with(
            dir.path(),
            Registry::new().with(Arc::new(Probe(runs.clone()))),
        )
        .unwrap();
        let project = engine.create_project("report", "").unwrap();
        let model = Arc::new(MockProvider::new(
            "hostile",
            vec![answer(vec![ToolCall {
                id: "forbidden".into(),
                name: name.into(),
                arguments: json!({}),
            }])],
        ));
        let result = harness(model.clone())
            .max_steps(max_steps)
            .ask(
                &engine,
                &AgentProfile::builder().core_actions(["probe.read"]).build(),
                &project.id,
                "Explain why the report differs. Do not use tools.",
                |_| {},
            )
            .await;
        assert!(result
            .err()
            .expect("unsolicited tool call must fail")
            .to_string()
            .contains("explicitly tool-free"));
        assert_eq!(runs.load(Ordering::SeqCst), 0);
        assert_eq!(model.recorded().len(), 1);
        assert!(model.recorded()[0].tools.is_empty());
        assert!(engine
            .store
            .get_research_state(&project.id)
            .unwrap()
            .unwrap()
            .findings
            .is_empty());
    }
}

#[tokio::test]
async fn explicitly_enabled_planner_is_preserved_without_tools() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open_with(
        dir.path(),
        Registry::new().with(Arc::new(Probe(Arc::new(AtomicUsize::new(0))))),
    )
    .unwrap();
    let project = engine.create_project("report", "").unwrap();
    let model = Arc::new(MockProvider::new(
        "mock",
        vec![
            Completion {
                content: "- Compare the report values".into(),
                tool_calls: vec![],
                usage: Usage::measured(10, 5),
            },
            answer(vec![]),
        ],
    ));
    let profile = AgentProfile::builder()
        .instructions("Use detailed reasoning when complex work needs it.")
        .core_actions(["probe.read"])
        .investigation_markers(["investigate"])
        .build();
    let mut roles = Vec::new();
    let outcome = harness(model.clone())
        .planner(true)
        .ask(
            &engine,
            &profile,
            &project.id,
            "Investigate why the report values differ. Do not use tools.",
            |event| {
                if let HarnessEvent::ModelCall { role, .. } = event {
                    roles.push(role);
                }
            },
        )
        .await
        .unwrap();
    assert_eq!(outcome.answer, "The report is ready.");
    assert_eq!(roles, ["plan", "reason"]);
    let recorded = model.recorded();
    assert_eq!(recorded.len(), 2);
    assert!(recorded.iter().all(|request| request.tools.is_empty()));
    assert!(!recorded[1].system.contains("probe.read"));
    assert!(recorded[1].system.starts_with(&format!(
        "{}\n\n{TOOL_FREE_RULE}\n\n",
        profile.prompts.instructions
    )));
    assert!(!recorded[1].system.contains(TOOL_COACHING));
    for request in &recorded {
        let messages = serde_json::to_string(&request.messages).unwrap();
        assert!(messages.contains("Investigate why the report values differ."));
        assert!(!messages.contains("Do not use tools."));
        assert!(request.system.contains(TOOL_FREE_RULE));
    }
}

#[tokio::test]
async fn synthesis_receives_normalized_task_and_current_goal() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open_with(dir.path(), Registry::new()).unwrap();
    let project = engine.create_project("report", "").unwrap();
    let model = Arc::new(MockProvider::new("mock", vec![answer(vec![])]));
    let question = "Explain why the report differs. Do not use tools.";
    let outcome = harness(model.clone())
        .max_steps(0)
        .ask(
            &engine,
            &AgentProfile::default(),
            &project.id,
            question,
            |_| {},
        )
        .await
        .unwrap();
    let recorded = model.recorded();
    assert_eq!(recorded.len(), 1);
    let messages = serde_json::to_string(&recorded[0].messages).unwrap();
    assert!(messages.contains("Explain why the report differs."));
    assert!(messages.contains("GOAL: Explain why the report differs."));
    assert!(!messages.contains("Do not use tools."));
    assert_eq!(outcome.state.goal, question);
}
