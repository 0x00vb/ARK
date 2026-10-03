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

use ark_core::{Focus, Project, ProjectId, RuleScope};
use ark_model::session::SubscriptionRuntime;
use ark_model::Provider;
use ark_runtime::{
    Agent, AgentProfile, ApprovalGate, CancelToken, ContextReport, Distiller, Engine, Harness,
    HarnessEvent, HarnessOutcome, ModelRouter, Role, RuleBook, RuleSelection, RulesTool, Session,
    Steering, StoreLedger,
};
use ark_tools::{Adapter, Registry};

/// A convenient, curated prelude for consumers.
pub mod prelude {
    pub use crate::{Ark, ArkBuilder, ArkError};
    pub use crate::{Settings, TurnOptions, TurnTools};
    pub use ark_core::{
        ActionDescriptor, Artifact, ArtifactId, ArtifactKind, Capability, CapabilityCatalog,
        CapabilitySpec, Project, ProjectId, Provenance, RiskTier, Rule, RuleBinding, RuleScope,
        RuleSet, Sniffer,
    };
    pub use ark_runtime::{
        AcquisitionProvider, AcquisitionResult, AgentProfile, ApprovalDecision, ApprovalGate,
        ApprovalRequest, Approver, CancelToken, ContextReport, Distiller, DistillerSet, Engine,
        FixedApprover, Harness, HarnessEvent, HarnessOutcome, KnowledgeHit, KnowledgeProvider,
        ModelRouter, Prompts, Role, RuleBook, RuleSelection, Session, Steering,
    };
    pub use ark_tools::diagram::{Diagram, DiagramKind};
    pub use ark_tools::{
        ActionOutput, Adapter, AdapterError, AdapterHealth, ProducedArtifact, Registry, RunCtx,
    };
    // The underlying crates, so tool authors can name `ark_tools::Result`, build a provider
    // with `ark_model::from_env_arc()`, etc., from the prelude alone.
    pub use {ark_core, ark_model, ark_runtime, ark_tools};
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

/// Per-turn options for [`Ark::ask_session`]: the interactive seams a front-end supplies.
#[derive(Clone, Default)]
pub struct TurnOptions {
    /// Trip to stop the turn cooperatively.
    pub cancel: Option<CancelToken>,
    /// Ask a human before state-changing actions (otherwise they are refused).
    pub approvals: Option<Arc<ApprovalGate>>,
    /// What the user is looking at, if the front-end knows.
    pub focus: Option<Focus>,
    /// Messages the user sends while the turn runs.
    pub steer: Option<Steering>,
    /// The front-end can show things beside the conversation, so the model is offered the
    /// `present` tool.
    pub present: bool,
    /// Tools for this turn only, beside the app's (see [`TurnTools`]).
    pub tools: Vec<TurnTools>,
}

/// Tools a front-end adds to one turn, such as an orchestrator's for the person's own
/// conversation. Their actions are offered directly, without a discovery step.
#[derive(Clone)]
pub struct TurnTools {
    pub adapter: Arc<dyn Adapter>,
    /// Writes the evidence cards of the adapter's results.
    pub distiller: Option<Distiller>,
}

/// The effective runtime configuration, for display.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Settings {
    pub store_root: PathBuf,
    pub context_budget: u32,
    pub max_steps: usize,
    pub planner: bool,
    pub critic: bool,
    pub allow_mutate: bool,
}

/// A fully-wired agent: an [`Engine`] (store + tools), a [`ModelRouter`], and an
/// [`AgentProfile`]. Build one with [`Ark::builder`].
pub struct Ark {
    engine: Arc<Engine>,
    router: ModelRouter,
    subscriptions: SubscriptionRuntime,
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

    /// Mutable access, for registering tools or sniffers before the agent runs. Panics once
    /// the engine has been shared with [`shared_engine`](Ark::shared_engine).
    pub fn engine_mut(&mut self) -> &mut Engine {
        Arc::get_mut(&mut self.engine).expect("engine_mut after the engine was shared")
    }

    /// A handle on the engine that outlives borrows of `self`: a front-end reads the store
    /// (artifacts, invocations, working state) with it while a turn holds the session.
    pub fn shared_engine(&self) -> Arc<Engine> {
        self.engine.clone()
    }

    pub fn profile(&self) -> &AgentProfile {
        &self.profile
    }

    /// Create a project (a working target scope) in the store.
    pub fn create_project(&self, name: &str, target: &str) -> Result<Project, ArkError> {
        Ok(self.engine.create_project(name, target)?)
    }

    /// The project with this name (or id prefix), created if it does not exist yet.
    pub fn find_or_create_project(&self, name: &str, target: &str) -> Result<Project, ArkError> {
        match self
            .engine
            .store
            .resolve_project(name)
            .map_err(ark_runtime::AgentError::from)?
        {
            Some(p) => Ok(p),
            None => self.create_project(name, target),
        }
    }

    pub fn router(&self) -> &ModelRouter {
        &self.router
    }

    /// Swap the model(s) used from the next turn on.
    pub fn set_router(&mut self, router: ModelRouter) {
        self.router = router;
    }

    pub fn settings(&self) -> Settings {
        Settings {
            store_root: self.engine.store.root().to_path_buf(),
            context_budget: self.budget,
            max_steps: self.max_steps,
            planner: self.planner,
            critic: self.critic,
            allow_mutate: self.allow_mutate,
        }
    }

    /// A harness configured from this agent's settings. Front-ends that need more control
    /// than [`ask_session`](Ark::ask_session) start from this.
    pub fn harness(&self) -> Harness {
        self.harness_with(self.router.clone_view())
    }

    fn harness_with(&self, router: ModelRouter) -> Harness {
        Harness::new(router)
            .max_steps(self.max_steps)
            .budget(self.budget)
            .planner(self.planner)
            .critic(self.critic)
            .allow_mutate(self.allow_mutate)
    }

    /// Start a new conversation session in `project`.
    pub fn start_session(&self, project: &ProjectId) -> Result<Session, ArkError> {
        Ok(Session::start(&self.engine, project)?)
    }

    /// Reopen a session by id or unique prefix.
    pub fn open_session(&self, id: &str) -> Result<Option<Session>, ArkError> {
        Ok(Session::open(&self.engine, id)?)
    }

    /// The most recently active session in `project`.
    pub fn latest_session(&self, project: &ProjectId) -> Result<Option<Session>, ArkError> {
        Ok(Session::latest(&self.engine, project)?)
    }

    /// Run one conversational turn: the harness gets the session's bounded carry-over (never
    /// the transcript), the turn is recorded, and the carry-over window slides.
    pub async fn ask_session(
        &self,
        session: &mut Session,
        input: &str,
        opts: TurnOptions,
        on_event: impl FnMut(HarnessEvent),
    ) -> Result<HarnessOutcome, ArkError> {
        let project = session.project().clone();
        let (engine, rules) = self.with_rules(&project, input)?;
        let (engine, profile) = self.with_tools(engine, &opts.tools);
        let router = self.session_router(session).await;
        let harness = self
            .harness_with(router)
            .conversation(session.conversation_digest())
            .approvals(opts.approvals)
            .cancel(opts.cancel)
            .steering(opts.steer)
            .presentation(opts.present)
            .focus(opts.focus)
            .rules(rules);
        session.begin_turn(&self.engine, input)?;
        match harness
            .ask(&engine, &profile, &project, input, on_event)
            .await
        {
            Ok(outcome) => {
                session.record_turn(&self.engine, input, &outcome)?;
                Ok(outcome)
            }
            Err(e) => {
                // Kept in the transcript, marked, so the person sees what failed.
                session.fail_turn(&self.engine, input, &e.to_string())?;
                Err(e.into())
            }
        }
    }

    /// The managed vendor sessions of subscription models (see [`ark_model::session`]). An
    /// orchestrator built on this `Ark` shares it, so every caller respects one capacity per
    /// account.
    pub fn subscriptions(&self) -> &SubscriptionRuntime {
        &self.subscriptions
    }

    /// The router for one conversation. When the reasoning model is a subscription and
    /// sessions are on (`ARK_SESSION_REUSE`), reasoning goes through the conversation's own
    /// managed vendor session, which continues with only what is new; a planner or critic on
    /// the same model makes stateless calls on the same account. Otherwise the app's router.
    pub async fn session_router(&self, session: &Session) -> ModelRouter {
        let strong = self.router.provider(Role::Reason);
        let spec = strong.model_id();
        if !self.subscriptions.policy().reuse || !self.subscriptions.manages(&spec) {
            return self.router.clone_view();
        }
        let lane = format!("session/{}", session.id());
        let Some(managed) = self.subscriptions.session(&lane, &spec).await else {
            return self.router.clone_view();
        };
        let fast = self.router.provider(Role::Plan);
        let fast = if Arc::ptr_eq(fast, strong) {
            self.subscriptions
                .stateless(&spec)
                .unwrap_or_else(|| fast.clone())
        } else {
            fast.clone()
        };
        ModelRouter::new(fast, managed)
    }

    /// The store's rule sets: put a project's standards here and bind them to the project
    /// (`RuleScope::Project`) so its turns follow them.
    pub fn rule_book(&self) -> RuleBook<'_> {
        RuleBook::new(&self.engine.store)
    }

    /// The engine a turn starts with: the app's tools and the MCP plugins connected now. A
    /// plugin added or removed later changes the next turn, not this one.
    fn turn_engine(&self) -> Arc<Engine> {
        match self.engine.plugins() {
            Some(p) if !p.adapters().is_empty() => Arc::new(self.engine.for_turn()),
            _ => self.engine.clone(),
        }
    }

    /// The rules bound to `project`, selected for `input`, and the engine to run with: the
    /// turn's, or a view of it that also offers the `rules` tool when rules are bound.
    fn with_rules(
        &self,
        project: &ProjectId,
        input: &str,
    ) -> Result<(Arc<Engine>, Option<RuleSelection>), ArkError> {
        let engine = self.turn_engine();
        let sets = self
            .rule_book()
            .resolve(&[RuleScope::Project(project.to_string())])?;
        if sets.is_empty() {
            return Ok((engine, None));
        }
        let selection = ark_runtime::rules::select(&sets, input, ark_runtime::rules::RULES_BUDGET);
        let registry = engine.registry.clone().with(Arc::new(RulesTool::new(sets)));
        Ok((Arc::new(engine.scoped(registry)), Some(selection)))
    }

    /// The engine and profile of a turn with `tools` added: their adapters registered, their
    /// actions offered directly, their distillers first.
    fn with_tools(
        &self,
        engine: Arc<Engine>,
        tools: &[TurnTools],
    ) -> (Arc<Engine>, std::borrow::Cow<'_, AgentProfile>) {
        if tools.is_empty() {
            return (engine, std::borrow::Cow::Borrowed(&self.profile));
        }
        let mut registry = engine.registry.clone();
        let mut profile = self.profile.clone();
        for t in tools {
            registry = registry.with(t.adapter.clone());
            for a in t.adapter.actions() {
                if !profile.core_actions.contains(&a.name) {
                    profile.core_actions.push(a.name);
                }
            }
            if let Some(d) = &t.distiller {
                profile.distillers.push(d.clone());
            }
        }
        (
            Arc::new(engine.scoped(registry)),
            std::borrow::Cow::Owned(profile),
        )
    }

    /// What the next turn's first request would contain, by source, without calling a model.
    pub async fn context_preview(&self, session: &Session) -> Result<ContextReport, ArkError> {
        self.context_preview_for(session, false).await
    }

    /// [`context_preview`](Ark::context_preview) for a front-end that can present (see
    /// [`TurnOptions::present`]).
    pub async fn context_preview_for(
        &self,
        session: &Session,
        present: bool,
    ) -> Result<ContextReport, ArkError> {
        self.context_preview_with(session, present, &[]).await
    }

    /// [`context_preview_for`](Ark::context_preview_for) with the turn's [`TurnTools`].
    pub async fn context_preview_with(
        &self,
        session: &Session,
        present: bool,
        tools: &[TurnTools],
    ) -> Result<ContextReport, ArkError> {
        let (engine, rules) = self.with_rules(session.project(), "")?;
        let (engine, profile) = self.with_tools(engine, tools);
        Ok(self
            .harness()
            .presentation(present)
            .conversation(session.conversation_digest())
            .rules(rules)
            .preview_context(&engine, &profile, session.project(), "")
            .await?)
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
        Ok(self
            .harness()
            .ask(&self.turn_engine(), &self.profile, project, task, on_event)
            .await?)
    }

    /// Run the simpler [`Agent`] loop (no context budget/router/state). Handy for small tasks.
    pub async fn ask_simple(
        &self,
        provider: Box<dyn Provider>,
        project: &ProjectId,
        task: &str,
    ) -> Result<String, ArkError> {
        let agent = Agent::new(provider)
            .max_steps(self.max_steps)
            .allow_mutate(self.allow_mutate);
        let outcome = agent
            .ask(&self.turn_engine(), &self.profile, project, task, |_| {})
            .await?;
        Ok(outcome.answer)
    }
}

/// Builder for [`Ark`]. Sane defaults everywhere; override only what you need.
pub struct ArkBuilder {
    engine: Option<Engine>,
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
    subscriptions: Option<SubscriptionRuntime>,
}

impl Default for ArkBuilder {
    fn default() -> Self {
        ArkBuilder {
            engine: None,
            store_root: PathBuf::from("./ark-workspace"),
            registry: Registry::new(),
            profile: AgentProfile::default(),
            router: None,
            provider: None,
            max_steps: 16,
            budget: std::env::var("ARK_CTX_BUDGET")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(6000),
            planner: false,
            critic: true,
            allow_mutate: false,
            subscriptions: None,
        }
    }
}

impl ArkBuilder {
    /// Where the store (blobs, metadata, index) lives. Also loads `<root>/plugins.json`.
    pub fn store_root(mut self, path: impl Into<PathBuf>) -> Self {
        self.store_root = path.into();
        self
    }
    /// Use an engine the consumer already opened (its own registry, sniffers, store). Takes
    /// precedence over `store_root`/`tool`/`registry`.
    pub fn engine(mut self, engine: Engine) -> Self {
        self.engine = Some(engine);
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

    /// Use this subscription runtime (capacities, session policy, ledger) instead of the
    /// default one, which keeps vendor session state in the store.
    pub fn subscriptions(mut self, s: SubscriptionRuntime) -> Self {
        self.subscriptions = Some(s);
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
        let engine = match self.engine {
            Some(e) => e,
            None => Engine::open_with(&self.store_root, self.registry)?,
        };
        let engine = Arc::new(engine);
        let subscriptions = self.subscriptions.unwrap_or_else(|| {
            SubscriptionRuntime::builder()
                .ledger(Arc::new(StoreLedger::new(engine.store.clone())))
                .build()
        });
        Ok(Ark {
            engine,
            subscriptions,
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
        async fn run(
            &self,
            _a: &str,
            input: serde_json::Value,
            _c: &RunCtx<'_>,
        ) -> ark_tools::Result<ActionOutput> {
            let t = input.get("text").and_then(|v| v.as_str()).unwrap_or("");
            Ok(ActionOutput::json(
                serde_json::json!({ "reversed": t.chars().rev().collect::<String>() }),
            ))
        }
    }

    #[tokio::test]
    async fn builds_and_runs_a_tiny_agent() {
        let dir = tempfile::tempdir().unwrap();
        let call = ToolCall {
            id: "c1".into(),
            name: "str.reverse".into(),
            arguments: serde_json::json!({ "text": "abc" }),
        };
        let script = vec![
            Completion {
                content: String::new(),
                tool_calls: vec![call],
                usage: Usage::measured(50, 5),
            },
            Completion {
                content: "Reversed to 'cba' [inv:1].".into(),
                tool_calls: vec![],
                usage: Usage::measured(60, 6),
            },
        ];
        let provider: Arc<dyn ark_model::Provider> = Arc::new(MockProvider::new("mock", script));

        let ark = Ark::builder()
            .store_root(dir.path())
            .tool(Arc::new(Reverse))
            .profile(
                AgentProfile::builder()
                    .instructions("You reverse strings.")
                    .core_actions(["str.reverse"])
                    .build(),
            )
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

    #[tokio::test]
    async fn project_rules_reach_a_single_agent_turn() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockProvider::new(
            "mock",
            vec![Completion {
                content: "ok".into(),
                tool_calls: vec![],
                usage: Usage::measured(10, 2),
            }],
        ));
        let provider: Arc<dyn ark_model::Provider> = mock.clone();
        let ark = Ark::builder()
            .store_root(dir.path())
            .provider(provider)
            .planner(false)
            .critic(false)
            .build()
            .unwrap();
        let project = ark.create_project("p", "").unwrap();
        let book = ark.rule_book();
        book.put(&RuleSet::parse(
            "style",
            "- [S1] (always) Use plain words.\n- [S2] Tables need a header. #tables",
        ))
        .unwrap();
        book.bind(&RuleBinding::new(
            "style",
            RuleScope::Project(project.id.to_string()),
        ))
        .unwrap();
        let mut session = ark.start_session(&project.id).unwrap();
        let mut applied = Vec::new();
        ark.ask_session(&mut session, "write a note", TurnOptions::default(), |e| {
            if let HarnessEvent::Rules { applied: a, .. } = e {
                applied = a;
            }
        })
        .await
        .unwrap();
        assert_eq!(applied.len(), 1, "only the always-rule fits this input");
        let req = &mock.recorded()[0];
        let first = &req.messages[0].content;
        assert!(first.contains("== RULES ==") && first.contains("[S1] Use plain words."));
        assert!(!first.contains("Tables need a header"));
        assert!(
            req.tools.iter().all(|t| !t.name.starts_with("rules.")),
            "the rules tool is loadable, not preloaded"
        );
        let preview = ark.context_preview(&session).await.unwrap();
        assert!(preview.rules > 0);
    }

    #[tokio::test]
    async fn a_subscription_conversation_keeps_one_vendor_session() {
        use ark_model::session::mock::{Call, MockSessions};
        use ark_model::session::{SessionBackend, SessionPolicy, SubscriptionRuntime};
        let backend = Arc::new(MockSessions::new("fake:m", "fake", vec![]));
        for t in ["one", "two", "three"] {
            backend.push(Completion {
                content: format!("answer {t}"),
                tool_calls: vec![],
                usage: Usage::measured(100, 5),
            });
        }
        let b = backend.clone();
        let subs = |reuse: bool| {
            let b = b.clone();
            SubscriptionRuntime::builder()
                .policy(SessionPolicy {
                    reuse,
                    ..Default::default()
                })
                .backends(Arc::new(move |p: &str, _m: &str| {
                    (p == "fake").then(|| b.clone() as Arc<dyn SessionBackend>)
                }))
                .build()
        };
        let dir = tempfile::tempdir().unwrap();
        // The app's provider only names the model; the managed session answers.
        let named: Arc<dyn ark_model::Provider> = Arc::new(MockProvider::new("fake:m", vec![]));
        let ark = Ark::builder()
            .store_root(dir.path())
            .provider(named.clone())
            .subscriptions(subs(true))
            .planner(false)
            .critic(false)
            .build()
            .unwrap();
        let project = ark.create_project("p", "").unwrap();
        let mut first = ark.start_session(&project.id).unwrap();
        for q in ["first question", "second question"] {
            let out = ark
                .ask_session(&mut first, q, TurnOptions::default(), |_| {})
                .await
                .unwrap();
            assert!(out.answer.starts_with("answer"));
        }
        let mut other = ark.start_session(&project.id).unwrap();
        ark.ask_session(
            &mut other,
            "another conversation",
            TurnOptions::default(),
            |_| {},
        )
        .await
        .unwrap();
        let calls = backend.calls();
        assert!(matches!(calls[0], Call::Open { .. }));
        match &calls[1] {
            Call::Resume { delta, .. } => {
                let sent: String = delta.messages.iter().map(|m| m.content.clone()).collect();
                assert!(sent.contains("second question"));
                assert!(!sent.contains("== TASK ==\nfirst question"), "{sent}");
            }
            c => panic!("expected a resume: {c:?}"),
        }
        assert!(
            matches!(&calls[2], Call::Open { vendor, .. } if vendor != "fake:m-v0"),
            "a second conversation opens its own vendor session"
        );
        assert_eq!(ark.subscriptions().sessions().len(), 2);

        // Sessions off: the app's provider is called as before.
        let dir = tempfile::tempdir().unwrap();
        let plain = Arc::new(MockProvider::new(
            "fake:m",
            vec![Completion {
                content: "plain".into(),
                tool_calls: vec![],
                usage: Usage::measured(10, 1),
            }],
        ));
        let p: Arc<dyn ark_model::Provider> = plain.clone();
        let ark = Ark::builder()
            .store_root(dir.path())
            .provider(p)
            .subscriptions(subs(false))
            .planner(false)
            .critic(false)
            .build()
            .unwrap();
        let project = ark.create_project("p", "").unwrap();
        let mut s = ark.start_session(&project.id).unwrap();
        ark.ask_session(&mut s, "hi", TurnOptions::default(), |_| {})
            .await
            .unwrap();
        assert_eq!(plain.call_count(), 1);
    }
}
