//! Every model request is explained: a Context event for the same role comes right before
//! each model call, and its total is the size of the request the provider received, part by
//! part (system prompt and messages, by the estimate the budget uses).

use std::sync::Arc;

use ark_core::{ActionDescriptor, RiskTier};
use ark_model::{estimate_tokens, Completion, MockProvider, Provider, ToolCall, Usage};
use ark_runtime::budget::WorkingSet;
use ark_runtime::{AgentProfile, Engine, Harness, HarnessEvent, ModelRouter};
use ark_tools::{ActionOutput, Adapter, Registry, RunCtx};
use serde_json::json;

struct Kv;

#[async_trait::async_trait]
impl Adapter for Kv {
    fn name(&self) -> &str {
        "kv"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            "kv.get",
            "read a value",
            RiskTier::Read,
        )]
    }
    async fn run(
        &self,
        _a: &str,
        i: serde_json::Value,
        _c: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        Ok(ActionOutput::json(json!({ "key": i["key"], "value": 42 })))
    }
}

/// A search of stored files: nothing to cite, so the final answer goes to the critic.
fn call(key: &str) -> Completion {
    Completion {
        content: "Checking.".into(),
        tool_calls: vec![ToolCall {
            id: key.into(),
            name: "search_evidence".into(),
            arguments: json!({ "query": key }),
        }],
        usage: Usage::measured(10, 5),
    }
}

#[tokio::test]
async fn every_request_is_explained_by_role() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open_with(dir.path(), Registry::new().with(Arc::new(Kv))).unwrap();
    let project = engine.create_project("t", "").unwrap();
    // Plan, three steps that never answer (the step limit forces a final answer), then an
    // ungrounded final answer that sends the critic in.
    let mut script = vec![Completion {
        content: "- read a\n- read b".into(),
        tool_calls: vec![],
        usage: Usage::measured(10, 5),
    }];
    script.extend([call("a"), call("b"), call("c")]);
    script.push(Completion {
        content: "It is probably fine.".into(),
        tool_calls: vec![],
        usage: Usage::measured(10, 5),
    });
    script.push(Completion {
        content: "Unknown: no evidence was gathered for that.".into(),
        tool_calls: vec![],
        usage: Usage::measured(10, 5),
    });
    let model = Arc::new(MockProvider::new("m", script));
    let p: Arc<dyn Provider> = model.clone();
    let mut events = Vec::new();
    Harness::new(ModelRouter::single(p))
        .planner(true)
        .max_steps(3)
        .ask(
            &engine,
            &AgentProfile::builder().core_actions(["kv.get"]).build(),
            &project.id,
            "investigate why the values differ and explain the cause",
            |e| events.push(e),
        )
        .await
        .unwrap();

    // Pair each model call with the Context just before it.
    let mut pending = None;
    let mut roles = Vec::new();
    let mut reports = Vec::new();
    for e in &events {
        match e {
            HarnessEvent::Context { report } => pending = Some(report.clone()),
            HarnessEvent::ModelCall { role, .. } => {
                let r = pending
                    .take()
                    .expect("a Context event before every model call");
                assert_eq!(&r.role, role, "the report is for this call");
                roles.push(role.clone());
                reports.push(r);
            }
            _ => {}
        }
    }
    assert_eq!(
        roles,
        ["plan", "reason", "reason", "reason", "synthesize", "critic"]
    );

    for (r, req) in reports.iter().zip(model.recorded()) {
        let size = estimate_tokens(&req.system)
            + req
                .messages
                .iter()
                .map(WorkingSet::message_tokens)
                .sum::<u32>();
        assert_eq!(
            r.total, size,
            "{} report matches the request it explains",
            r.role
        );
        let parts: u32 = r.rows().iter().map(|(_, n)| n).sum();
        assert!(
            parts.abs_diff(r.total) <= 4,
            "{}: the parts add up to the total ({parts} vs {})",
            r.role,
            r.total
        );
    }
    let last_step = &reports[3];
    assert!(
        last_step.model_output > 0,
        "earlier tool calls are counted as model output"
    );
    assert!(last_step.action_schemas > 0 && last_step.action_schemas < last_step.tool_schemas);
}

/// The skills catalog rides in the first message after the rules, is reported as `skills`,
/// and the parts still add up to the request.
#[tokio::test]
async fn the_skills_catalog_is_counted_as_its_own_part() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open_with(dir.path(), Registry::new().with(Arc::new(Kv))).unwrap();
    let project = engine.create_project("t", "").unwrap();
    let skill = ark_runtime::Skill {
        name: "release-notes".into(),
        description: "Write release notes from a list of changes".into(),
        dir: dir.path().to_path_buf(),
        scope: ark_runtime::SkillScope::Store,
    };
    let catalog = ark_runtime::skills::catalog(&[skill], ark_runtime::skills::CATALOG_BUDGET)
        .unwrap()
        .text;
    let model = Arc::new(MockProvider::new(
        "m",
        vec![Completion {
            content: "Done.".into(),
            tool_calls: vec![],
            usage: Usage::measured(10, 5),
        }],
    ));
    let p: Arc<dyn Provider> = model.clone();
    let mut reports = Vec::new();
    Harness::new(ModelRouter::single(p))
        .critic(false)
        .skills(Some(catalog.clone()))
        .ask(
            &engine,
            &AgentProfile::builder().core_actions(["kv.get"]).build(),
            &project.id,
            "write the notes",
            |e| {
                if let HarnessEvent::Context { report } = e {
                    reports.push(report);
                }
            },
        )
        .await
        .unwrap();
    let req = &model.recorded()[0];
    let first = &req.messages[0].content;
    let at = first
        .find("== SKILLS ==")
        .expect("the catalog is in the first message");
    assert!(at < first.find("== TASK ==").unwrap(), "before the task");
    assert!(first.contains("- release-notes: Write release notes"));
    let r = &reports[0];
    assert_eq!(r.skills, estimate_tokens(&catalog));
    let parts: u32 = r.rows().iter().map(|(_, n)| n).sum();
    assert!(parts.abs_diff(r.total) <= 4, "{parts} vs {}", r.total);
    assert!(r.task < r.skills, "the catalog is not counted as the task");
}

/// Agents call the actions a task needs and let the host ask, rather than asking in text or
/// claiming they cannot change anything.
#[tokio::test]
async fn requests_explain_how_to_reach_the_approval_gate() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open_with(dir.path(), Registry::new()).unwrap();
    let project = engine.create_project("t", "").unwrap();
    let model = Arc::new(MockProvider::new(
        "m",
        vec![Completion {
            content: "Done.".into(),
            tool_calls: vec![],
            usage: Usage::measured(10, 5),
        }],
    ));
    let provider: Arc<dyn Provider> = model.clone();
    Harness::new(ModelRouter::single(provider))
        .planner(false)
        .critic(false)
        .ask(
            &engine,
            &AgentProfile::builder().build(),
            &project.id,
            "hello",
            |_| {},
        )
        .await
        .unwrap();
    let requests = model.recorded();
    assert_eq!(requests.len(), 1);
    let system = &requests[0].system;
    for line in [
        "Call the actions the task needs, including ones that change things",
        "the host asks them before it runs",
        "Never ask for permission in text",
        "never say you are read-only",
    ] {
        assert!(system.contains(line), "missing: {line}");
    }
    // A sentence that every change needs approval made agents answer "I am read-only" in Auto,
    // where nothing asks (an owner's named agent, 2026-11-20, ADR 0129).
    assert!(!system.contains("need explicit human approval"));
}
