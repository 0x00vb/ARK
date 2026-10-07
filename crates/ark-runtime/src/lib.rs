//! # ark-runtime
//!
//! The domain-agnostic agent runtime: it ties the store and the tool registry together,
//! ingests artifacts, runs actions through the approval gate, records every call as an
//! invocation, and drives the LLM agent loop. Two loops are provided:
//!
//! - [`Agent`] — a simple ReAct-style tool-calling loop.
//! - [`Harness`] — an evidence-driven loop with a managed [`budget`] context window, model
//!   [`router`]ing across roles, deterministic result [`distill`]ation into evidence cards,
//!   progressive tool disclosure ([`catalog`]), a persistent working state, a self-check and
//!   optional critic, and full [`metrics`].
//!
//! Everything domain-specific is injected through an [`AgentProfile`]: the persona, the
//! capability taxonomy, the result distillers, and an optional external-knowledge source. The
//! runtime itself names no tool and no domain.

pub mod acquire;
pub mod agent;
pub mod approval;
pub mod budget;
pub mod cancel;
pub mod capmap;
pub mod catalog;
pub mod context_report;
pub mod distill;
pub mod engine;
pub mod error;
pub mod evidence;
pub mod folder;
pub mod harness;
pub mod knowledge;
pub mod ledger;
pub mod metrics;
pub mod notebook;
pub mod per_agent;
pub mod plugins;
mod process;
pub mod profile;
pub mod router;
pub mod rules;
pub mod session;
pub mod skills;
pub mod steer;
pub mod web;
pub mod worker;

mod blobs;

pub use acquire::{AcquisitionProvider, AcquisitionResult, Sources};
pub use agent::{Agent, AgentEvent, AgentOutcome};
pub use approval::{
    lasting_grants, remove_lasting_grant, ApprovalClassifier, ApprovalDecision, ApprovalGate,
    ApprovalRequest, Approver, FixedApprover, LastingGrant, LASTING_GRANT,
};
pub use cancel::CancelToken;
pub use context_report::ContextReport;
pub use distill::{call_digest, distill, Distiller, DistillerSet};
pub use engine::{Engine, Restored};
pub use error::AgentError;
pub use harness::{FileBefore, Harness, HarnessEvent, HarnessOutcome, STATE_DIGEST_ITEMS};
pub use knowledge::{KnowledgeHit, KnowledgeProvider};
pub use ledger::StoreLedger;
pub use metrics::{role_name, Metrics};
pub use plugins::{PluginSpec, PluginStatus, Plugins};
pub use profile::{AgentProfile, AgentProfileBuilder, Prompts};
pub use router::{ModelRouter, Role};
pub use rules::{RuleBook, RuleSelection, RulesTool};
pub use session::{
    CarryPolicy, CompactReport, Post, Session, SessionRecord, SessionUsage, Turn, POSTS_CARRIED,
};
pub use skills::{Skill, SkillCatalog, SkillScope, SkillsTool};
pub use steer::Steering;

// Re-export the blob bridge for consumers wiring their own engine.
pub use blobs::StoreBlobs;

#[cfg(test)]
mod tests {
    use super::*;
    use ark_model::{Completion, MockProvider, ToolCall, Usage};
    use ark_tools::{ActionOutput, Adapter, Registry, RunCtx};
    use std::sync::Arc;

    // A trivial in-process tool so we can drive the loop end-to-end with a mock model.
    struct WordCount;

    #[async_trait::async_trait]
    impl Adapter for WordCount {
        fn name(&self) -> &str {
            "text"
        }
        fn actions(&self) -> Vec<ark_core::ActionDescriptor> {
            vec![ark_core::ActionDescriptor::new(
                "text.wordcount",
                "count words in a string",
                ark_core::RiskTier::Read,
            )
            .with_input_schema(serde_json::json!({
                "type": "object", "required": ["text"],
                "properties": { "text": { "type": "string" } }
            }))]
        }
        async fn run(
            &self,
            _a: &str,
            input: serde_json::Value,
            _c: &RunCtx<'_>,
        ) -> ark_tools::Result<ActionOutput> {
            let text = input.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let words = text.split_whitespace().count();
            Ok(ActionOutput::json(serde_json::json!({ "words": words })))
        }
    }

    #[tokio::test]
    async fn harness_runs_a_tool_then_answers() {
        let dir = tempfile::tempdir().unwrap();
        let engine =
            Engine::open_with(dir.path(), Registry::new().with(Arc::new(WordCount))).unwrap();
        let project = engine.create_project("t", "unit test").unwrap();

        // Script: step 1 loads the action, step 2 calls it, step 3 answers.
        let load = ToolCall {
            id: "c1".into(),
            name: "load_action".into(),
            arguments: serde_json::json!({ "name": "text.wordcount" }),
        };
        let run = ToolCall {
            id: "c2".into(),
            name: "text.wordcount".into(),
            arguments: serde_json::json!({ "text": "one two three" }),
        };
        let script = vec![
            Completion {
                content: String::new(),
                tool_calls: vec![load],
                usage: Usage::measured(100, 10),
            },
            Completion {
                content: String::new(),
                tool_calls: vec![run],
                usage: Usage::measured(120, 10),
            },
            Completion {
                content: "The text has 3 words [inv:1].".into(),
                tool_calls: vec![],
                usage: Usage::measured(130, 12),
            },
        ];
        let provider: Arc<dyn ark_model::Provider> = Arc::new(MockProvider::new("mock", script));
        let harness = Harness::new(ModelRouter::single(provider))
            .planner(false)
            .critic(false);
        let profile = AgentProfile::default();

        let outcome = harness
            .ask(
                &engine,
                &profile,
                &project.id,
                "how many words is 'one two three'",
                |_e| {},
            )
            .await
            .unwrap();

        assert!(outcome.answer.contains('3'), "answer: {}", outcome.answer);
        assert_eq!(outcome.metrics.tool_calls, 1, "the tool ran once");
        assert!(
            outcome.metrics.cards_created >= 1,
            "an evidence card was distilled"
        );
        // The reproducible record was persisted.
        assert_eq!(engine.store.list_invocations(&project.id).unwrap().len(), 1);
        let md = notebook::render_session_log(&engine.store, &project.id).unwrap();
        assert!(md.contains("text.wordcount"));
    }

    // A stub acquisition provider proving the seam is wired: the meta-tools appear only when a
    // provider is supplied, and the harness dispatches to it and emits a Capability event.
    struct StubAcq;
    #[async_trait::async_trait]
    impl acquire::AcquisitionProvider for StubAcq {
        async fn find(&self, need: &str) -> String {
            format!("could serve '{need}' via provider X")
        }
        async fn propose(&self, need: &str) -> String {
            format!("plan for '{need}': install X")
        }
        async fn acquire(&self, need: &str, approved: bool) -> acquire::AcquisitionResult {
            if approved {
                acquire::AcquisitionResult::changed(format!("acquired X for '{need}'"))
            } else {
                acquire::AcquisitionResult::info(format!("'{need}' needs approval to acquire"))
            }
        }
    }

    #[tokio::test]
    async fn acquisition_seam_dispatches_and_events() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open_with(dir.path(), Registry::new()).unwrap();
        let project = engine.create_project("t", "unit test").unwrap();

        let find = ToolCall {
            id: "c1".into(),
            name: "find_capability".into(),
            arguments: serde_json::json!({ "need": "disassembly" }),
        };
        let script = vec![
            Completion {
                content: String::new(),
                tool_calls: vec![find],
                usage: Usage::measured(100, 10),
            },
            Completion {
                content: "Understood; provider X could serve it.".into(),
                tool_calls: vec![],
                usage: Usage::measured(110, 8),
            },
        ];
        let provider: Arc<dyn ark_model::Provider> =
            Arc::new(MockProvider::new("mock", script.clone()));
        let harness = Harness::new(ModelRouter::single(provider))
            .planner(false)
            .critic(false);
        let profile = AgentProfile::builder()
            .acquisition(Arc::new(StubAcq))
            .build();

        let mut saw_capability = false;
        let _ = harness
            .ask(&engine, &profile, &project.id, "find a disassembler", |e| {
                if let HarnessEvent::Capability { action, detail } = e {
                    assert_eq!(action, "find_capability");
                    assert!(detail.contains("provider X"));
                    saw_capability = true;
                }
            })
            .await
            .unwrap();
        assert!(saw_capability, "a Capability event was emitted");

        // Gating: with a provider the acquisition meta-tools are offered; without one they aren't.
        let with_acq = Arc::new(MockProvider::new("m", vec![answer_only()]));
        let dyn_with: Arc<dyn ark_model::Provider> = with_acq.clone();
        let _ = Harness::new(ModelRouter::single(dyn_with))
            .planner(false)
            .critic(false)
            .ask(&engine, &profile, &project.id, "hi", |_| {})
            .await
            .unwrap();
        let offered: Vec<String> = with_acq.recorded()[0]
            .tools
            .iter()
            .map(|t| t.name.clone())
            .collect();
        assert!(
            offered.iter().any(|n| n == "find_capability"),
            "acquisition meta-tools offered when wired"
        );

        let no_acq = Arc::new(MockProvider::new("m", vec![answer_only()]));
        let dyn_no: Arc<dyn ark_model::Provider> = no_acq.clone();
        let _ = Harness::new(ModelRouter::single(dyn_no))
            .planner(false)
            .critic(false)
            .ask(&engine, &AgentProfile::default(), &project.id, "hi", |_| {})
            .await
            .unwrap();
        let plain: Vec<String> = no_acq.recorded()[0]
            .tools
            .iter()
            .map(|t| t.name.clone())
            .collect();
        assert!(
            !plain.iter().any(|n| n == "find_capability"),
            "not offered without a provider"
        );
    }

    fn answer_only() -> Completion {
        Completion {
            content: "done. [inv:none] nothing to do; unknown for now.".into(),
            tool_calls: vec![],
            usage: Usage::measured(50, 5),
        }
    }
}
