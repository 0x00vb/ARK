//! # ark-sdk
//!
//! The small, stable facade over the ARK runtime. A new agent should need very little
//! boilerplate: register some tools, describe the agent's persona and capabilities, point it
//! at a model, and run. Everything deeper (the loop internals, the context budget, the
//! evidence model) stays available through the re-exported crates but is never required.
//!
//! ```ignore
//! use ark_sdk::prelude::*;
//!
//! let ark = Ark::builder()
//!     .store_root("./workspace")
//!     .tool(std::sync::Arc::new(my_tool()))
//!     .profile(AgentProfile::builder().instructions("You are a helpful research agent.").build())
//!     .provider(ark_model::from_env_arc()?)
//!     .build()?;
//!
//! let project = ark.create_project("demo", "a quick task")?;
//! let outcome = ark.ask(&project.id, "what can you find out?").await?;
//! println!("{}", outcome.answer);
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use ark_core::{Project, ProjectId};
use ark_model::Provider;
use ark_runtime::{
    Agent, AgentProfile, Engine, Harness, HarnessEvent, HarnessOutcome, ModelRouter,
};
use ark_tools::{Adapter, Registry};

/// A convenient, curated prelude for consumers.
pub mod prelude {
    pub use crate::{Ark, ArkBuilder, ArkError};
    pub use ark_core::{
        ActionDescriptor, Artifact, ArtifactId, ArtifactKind, Capability, CapabilityCatalog,
        CapabilitySpec, Project, ProjectId, Provenance, RiskTier, Sniffer,
    };
    pub use ark_runtime::{
        AgentProfile, Distiller, DistillerSet, Engine, Harness, HarnessEvent, KnowledgeHit,
        KnowledgeProvider, ModelRouter, Prompts,
    };
    pub use ark_tools::{ActionOutput, Adapter, AdapterError, AdapterHealth, ProducedArtifact, Registry, RunCtx};
    // The underlying crates, so tool authors can name `ark_tools::Result`, build a provider
    // with `ark_model::from_env_arc()`, etc., from the prelude alone.
    pub use {ark_core, ark_model, ark_tools};
}

#[derive(Debug, thiserror::Error)]
pub enum ArkError {
    #[error("no model configured: set .provider(), .router(), or the ARK_PROVIDER env var")]
    NoModel,
    #[error(transparent)]
    Provider(#[from] ark_model::ProviderError),
    #[error(transparent)]
    Runtime(#[from] ark_runtime::AgentError),
}

/// A fully-wired agent: an [`Engine`] (store + tools), a [`ModelRouter`], and an
/// [`AgentProfile`]. Build one with [`Ark::builder`].
pub struct Ark {
    engine: Engine,
    router: ModelRouter,
    profile: AgentProfile,
    max_steps: usize,
    budget: u32,
    planner: bool,
    critic: bool,
    allow_mutate: bool,
}

impl Ark {
    pub fn builder() -> ArkBuilder {
        ArkBuilder::default()
    }

    /// Access the underlying engine (to ingest artifacts, list invocations, export the
    /// notebook, or register more tools/sniffers).
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn engine_mut(&mut self) -> &mut Engine {
        &mut self.engine
    }

    pub fn profile(&self) -> &AgentProfile {
        &self.profile
    }

    /// Create a project (a working target scope) in the store.
    pub fn create_project(&self, name: &str, target: &str) -> Result<Project, ArkError> {
        Ok(self.engine.create_project(name, target)?)
    }

    /// Run the advanced [`Harness`] loop on a task, discarding progress events.
    pub async fn ask(&self, project: &ProjectId, task: &str) -> Result<HarnessOutcome, ArkError> {
        self.ask_with(project, task, |_| {}).await
    }

    /// Run the [`Harness`] loop, streaming progress events to `on_event`.
    pub async fn ask_with(
        &self,
        project: &ProjectId,
        task: &str,
        on_event: impl FnMut(HarnessEvent),
    ) -> Result<HarnessOutcome, ArkError> {
        // ModelRouter isn't Clone; build a fresh single-tier view sharing the same providers.
        let harness = Harness::new(self.router.clone_view())
            .max_steps(self.max_steps)
            .budget(self.budget)
            .planner(self.planner)
            .critic(self.critic)
            .allow_mutate(self.allow_mutate);
        Ok(harness.ask(&self.engine, &self.profile, project, task, on_event).await?)
    }

    /// Run the simpler [`Agent`] loop (no context budget/router/state). Handy for small tasks.
    pub async fn ask_simple(&self, provider: Box<dyn Provider>, project: &ProjectId, task: &str) -> Result<String, ArkError> {
        let agent = Agent::new(provider).max_steps(self.max_steps).allow_mutate(self.allow_mutate);
        let outcome = agent.ask(&self.engine, &self.profile, project, task, |_| {}).await?;
        Ok(outcome.answer)
    }
}

/// Builder for [`Ark`]. Sane defaults everywhere; override only what you need.
pub struct ArkBuilder {
    store_root: PathBuf,
    registry: Registry,
    profile: AgentProfile,
    router: Option<ModelRouter>,
    provider: Option<Arc<dyn Provider>>,
    max_steps: usize,
    budget: u32,
    planner: bool,
    critic: bool,
    allow_mutate: bool,
}

impl Default for ArkBuilder {
    fn default() -> Self {
        ArkBuilder {
            store_root: PathBuf::from("./ark-workspace"),
            registry: Registry::new(),
            profile: AgentProfile::default(),
            router: None,
            provider: None,
            max_steps: 16,
            budget: 6000,
            planner: true,
            critic: true,
            allow_mutate: false,
        }
    }
}

impl ArkBuilder {
    /// Where the store (blobs, metadata, index) lives. Also loads `<root>/plugins.json`.
    pub fn store_root(mut self, path: impl Into<PathBuf>) -> Self {
        self.store_root = path.into();
        self
    }
    /// Register a tool adapter.
    pub fn tool(mut self, adapter: Arc<dyn Adapter>) -> Self {
        self.registry.register(adapter);
        self
    }
    /// Replace the whole registry (e.g. one built elsewhere).
    pub fn registry(mut self, registry: Registry) -> Self {
        self.registry = registry;
        self
    }
    /// The agent's persona, capabilities, distillers and knowledge.
    pub fn profile(mut self, profile: AgentProfile) -> Self {
        self.profile = profile;
        self
    }
    /// Use a single model for every role.
    pub fn provider(mut self, provider: Arc<dyn Provider>) -> Self {
        self.provider = Some(provider);
        self
    }
    /// Use an explicit two-tier model router.
    pub fn router(mut self, router: ModelRouter) -> Self {
        self.router = Some(router);
        self
    }
    pub fn max_steps(mut self, n: usize) -> Self {
        self.max_steps = n;
        self
    }
    pub fn context_budget(mut self, tokens: u32) -> Self {
        self.budget = tokens;
        self
    }
    pub fn planner(mut self, yes: bool) -> Self {
        self.planner = yes;
        self
    }
    pub fn critic(mut self, yes: bool) -> Self {
        self.critic = yes;
        self
    }
    /// Allow state-changing (`Mutate`) actions to run without a separate approval step.
    pub fn allow_mutate(mut self, yes: bool) -> Self {
        self.allow_mutate = yes;
        self
    }

    /// Wire everything together. Resolves the model in order: explicit router → explicit
    /// provider → the `ARK_PROVIDER`/`ARK_MODEL` environment (defaulting to Ollama).
    pub fn build(self) -> Result<Ark, ArkError> {
        let router = match (self.router, self.provider) {
            (Some(r), _) => r,
            (None, Some(p)) => ModelRouter::single(p),
            (None, None) => ModelRouter::from_env()?,
        };
        let engine = Engine::open_with(&self.store_root, self.registry)?;
        Ok(Ark {
            engine,
            router,
            profile: self.profile,
            max_steps: self.max_steps,
            budget: self.budget,
            planner: self.planner,
            critic: self.critic,
            allow_mutate: self.allow_mutate,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::prelude::*;
    use ark_model::{Completion, MockProvider, ToolCall, Usage};
    use std::sync::Arc;

    struct Reverse;
    #[async_trait::async_trait]
    impl Adapter for Reverse {
        fn name(&self) -> &str {
            "str"
        }
        fn actions(&self) -> Vec<ActionDescriptor> {
            vec![ActionDescriptor::new("str.reverse", "reverse a string", RiskTier::Read)
                .with_input_schema(serde_json::json!({
                    "type": "object", "required": ["text"], "properties": { "text": { "type": "string" } }
                }))]
        }
        async fn run(&self, _a: &str, input: serde_json::Value, _c: &RunCtx<'_>) -> ark_tools::Result<ActionOutput> {
            let t = input.get("text").and_then(|v| v.as_str()).unwrap_or("");
            Ok(ActionOutput::json(serde_json::json!({ "reversed": t.chars().rev().collect::<String>() })))
        }
    }

    #[tokio::test]
    async fn builds_and_runs_a_tiny_agent() {
        let dir = tempfile::tempdir().unwrap();
        let call = ToolCall { id: "c1".into(), name: "str.reverse".into(), arguments: serde_json::json!({ "text": "abc" }) };
        let script = vec![
            Completion { content: String::new(), tool_calls: vec![call], usage: Usage::measured(50, 5) },
            Completion { content: "Reversed to 'cba' [inv:1].".into(), tool_calls: vec![], usage: Usage::measured(60, 6) },
        ];
        let provider: Arc<dyn ark_model::Provider> = Arc::new(MockProvider::new("mock", script));

        let ark = Ark::builder()
            .store_root(dir.path())
            .tool(Arc::new(Reverse))
            .profile(AgentProfile::builder().instructions("You reverse strings.").core_actions(["str.reverse"]).build())
            .provider(provider)
            .planner(false)
            .critic(false)
            .build()
            .unwrap();

        let project = ark.create_project("t", "reverse test").unwrap();
        let outcome = ark.ask(&project.id, "reverse abc").await.unwrap();
        assert!(outcome.answer.contains("cba"), "answer: {}", outcome.answer);
        assert_eq!(outcome.metrics.tool_calls, 1);
    }
}
