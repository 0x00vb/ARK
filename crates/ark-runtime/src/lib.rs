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

pub mod agent;
pub mod budget;
pub mod capmap;
pub mod catalog;
pub mod distill;
pub mod engine;
pub mod error;
pub mod harness;
pub mod knowledge;
pub mod metrics;
pub mod notebook;
pub mod profile;
pub mod router;

mod blobs;

pub use agent::{Agent, AgentEvent, AgentOutcome};
pub use distill::{call_digest, distill, Distiller, DistillerSet};
pub use engine::Engine;
pub use error::AgentError;
pub use harness::{Harness, HarnessEvent, HarnessOutcome};
pub use knowledge::{KnowledgeHit, KnowledgeProvider};
pub use metrics::Metrics;
pub use profile::{AgentProfile, AgentProfileBuilder, Prompts};
pub use router::{ModelRouter, Role};

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
            vec![ark_core::ActionDescriptor::new("text.wordcount", "count words in a string", ark_core::RiskTier::Read)
                .with_input_schema(serde_json::json!({
                    "type": "object", "required": ["text"],
                    "properties": { "text": { "type": "string" } }
                }))]
        }
        async fn run(&self, _a: &str, input: serde_json::Value, _c: &RunCtx<'_>) -> ark_tools::Result<ActionOutput> {
            let text = input.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let words = text.split_whitespace().count();
            Ok(ActionOutput::json(serde_json::json!({ "words": words })))
        }
    }

    #[tokio::test]
    async fn harness_runs_a_tool_then_answers() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open_with(dir.path(), Registry::new().with(Arc::new(WordCount))).unwrap();
        let project = engine.create_project("t", "unit test").unwrap();

        // Script: step 1 loads the action, step 2 calls it, step 3 answers.
        let load = ToolCall { id: "c1".into(), name: "load_action".into(), arguments: serde_json::json!({ "name": "text.wordcount" }) };
        let run = ToolCall { id: "c2".into(), name: "text.wordcount".into(), arguments: serde_json::json!({ "text": "one two three" }) };
        let script = vec![
            Completion { content: String::new(), tool_calls: vec![load], usage: Usage::measured(100, 10) },
            Completion { content: String::new(), tool_calls: vec![run], usage: Usage::measured(120, 10) },
            Completion { content: "The text has 3 words [inv:1].".into(), tool_calls: vec![], usage: Usage::measured(130, 12) },
        ];
        let provider: Arc<dyn ark_model::Provider> = Arc::new(MockProvider::new("mock", script));
        let harness = Harness::new(ModelRouter::single(provider)).planner(false).critic(false);
        let profile = AgentProfile::default();

        let outcome = harness
            .ask(&engine, &profile, &project.id, "how many words is 'one two three'", |_e| {})
            .await
            .unwrap();

        assert!(outcome.answer.contains('3'), "answer: {}", outcome.answer);
        assert_eq!(outcome.metrics.tool_calls, 1, "the tool ran once");
        assert!(outcome.metrics.cards_created >= 1, "an evidence card was distilled");
        // The reproducible record was persisted.
        assert_eq!(engine.store.list_invocations(&project.id).unwrap().len(), 1);
        let md = notebook::render_session_log(&engine.store, &project.id).unwrap();
        assert!(md.contains("text.wordcount"));
    }
}
