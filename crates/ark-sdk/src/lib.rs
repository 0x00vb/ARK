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

use ark_connect::Connections;
use ark_core::{
    ActionDescriptor, ArtifactKind, Focus, Project, ProjectId, RiskTier, RuleScope, SessionId,
};
use ark_model::session::SubscriptionRuntime;
use ark_model::Provider;
use ark_runtime::per_agent::{Owner, PerAgent};
use ark_runtime::router::CurrentModel;
use ark_runtime::skills;
use ark_runtime::{
    AgentProfile, ApprovalGate, CancelToken, ContextReport, Distiller, Engine, Harness,
    HarnessEvent, HarnessOutcome, ModelRouter, Role, RuleBook, RuleSelection, RulesTool, Session,
    Steering, StoreLedger,
};
use ark_tools::web::WebAdapter;
use ark_tools::{ActionOutput, Adapter, AdapterError, ProducedArtifact, Registry, RunCtx};

const IMAGE_GENERATE: &str = "image.generate";
const MAX_IMAGE_BYTES: usize = 25 * 1024 * 1024;
const MAX_IMAGE_EDGE: u32 = 8192;
const MAX_IMAGE_PIXELS: u64 = 32_000_000;

struct ImageGenerator {
    provider: Arc<dyn Provider>,
}

#[async_trait::async_trait]
impl Adapter for ImageGenerator {
    fn name(&self) -> &str {
        "image"
    }

    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            IMAGE_GENERATE,
            "generate one PNG image with OpenAI and save it in the project",
            RiskTier::Mutate,
        )
        .with_input_schema(serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": { "prompt": { "type": "string", "minLength": 1 } },
            "required": ["prompt"]
        }))
        .ask_each_time()]
    }

    async fn run(
        &self,
        action: &str,
        input: serde_json::Value,
        ctx: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        if action != IMAGE_GENERATE {
            return Err(AdapterError::UnknownAction(action.into()));
        }
        let prompt = input["prompt"].as_str().map(str::trim).unwrap_or("");
        if prompt.is_empty() {
            return Err(AdapterError::bad_input(action, "prompt is required"));
        }
        let image = self
            .provider
            .generate_image(prompt)
            .await
            .ok_or_else(|| AdapterError::ToolUnavailable {
                tool: IMAGE_GENERATE.into(),
                detail: "the selected model does not generate images".into(),
            })?
            .map_err(|e| AdapterError::Tool(e.to_string()))?;
        let (width, height) = validate_generated_png(&image.media_type, &image.bytes)
            .map_err(|detail| AdapterError::Tool(format!("invalid generated image: {detail}")))?;
        let root = ctx
            .workspace
            .as_ref()
            .ok_or_else(|| AdapterError::ToolUnavailable {
                tool: IMAGE_GENERATE.into(),
                detail: "this project has no writable folder".into(),
            })?;
        std::fs::create_dir_all(root)?;
        let mut temporary = tempfile::NamedTempFile::new_in(root)?;
        std::io::Write::write_all(&mut temporary, &image.bytes)?;
        temporary.as_file().sync_all()?;
        let mut path = None;
        for n in 1..=10_000u32 {
            let name = if n == 1 {
                "generated-image.png".to_string()
            } else {
                format!("generated-image-{n}.png")
            };
            match temporary.persist_noclobber(root.join(&name)) {
                Ok(_) => {
                    path = Some(name);
                    break;
                }
                Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {
                    temporary = e.file;
                }
                Err(e) => return Err(AdapterError::Io(e.error)),
            }
        }
        let path = path.ok_or_else(|| {
            AdapterError::Tool("could not choose an unused generated image filename".into())
        })?;
        Ok(ActionOutput::json(serde_json::json!({
            "path": path,
            "width": width,
            "height": height
        }))
        .with_artifact(
            ProducedArtifact::bytes(ArtifactKind::Image, path.clone(), image.bytes).at(path),
        ))
    }

    fn summary(&self) -> Option<&str> {
        Some("generate an image in the project")
    }
}

fn validate_generated_png(media_type: &str, bytes: &[u8]) -> Result<(u32, u32), String> {
    if media_type != "image/png" {
        return Err("only PNG output is accepted".into());
    }
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err("the PNG is larger than 25 MB".into());
    }
    if bytes.windows(4).any(|chunk| chunk == b"acTL") {
        return Err("animated PNG output is not accepted".into());
    }
    if !bytes.ends_with(b"\0\0\0\0IEND\xaeB`\x82") {
        return Err("the PNG has trailing or incomplete data".into());
    }
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder
        .read_info()
        .map_err(|_| "the PNG container is invalid".to_string())?;
    let info = reader.info();
    let width = info.width;
    let height = info.height;
    if width == 0 || height == 0 || width > MAX_IMAGE_EDGE || height > MAX_IMAGE_EDGE {
        return Err("the PNG dimensions are outside 1 through 8192 pixels".into());
    }
    if u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
        return Err("the PNG contains more than 32 megapixels".into());
    }
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| "the PNG output size is too large".to_string())?;
    let mut decoded = vec![0; size];
    reader
        .next_frame(&mut decoded)
        .map_err(|_| "the PNG image data is invalid".to_string())?;
    Ok((width, height))
}

/// A convenient, curated prelude for consumers.
pub mod prelude {
    pub use crate::{Ark, ArkBuilder, ArkError};
    pub use crate::{Settings, TurnOptions, TurnTools};
    pub use ark_core::{
        ActionDescriptor, Artifact, ArtifactId, ArtifactKind, Capability, CapabilityCatalog,
        CapabilitySpec, Project, ProjectId, Provenance, RiskTier, Rule, RuleBinding, RuleScope,
        RuleSet, Sniffer,
    };
    pub use ark_runtime::router::CurrentModel;
    pub use ark_runtime::{
        AcquisitionProvider, AcquisitionResult, AgentProfile, ApprovalDecision, ApprovalGate,
        ApprovalRequest, Approver, CancelToken, ContextReport, Distiller, DistillerSet, Engine,
        FileBefore, FixedApprover, Harness, HarnessEvent, HarnessOutcome, KnowledgeHit,
        KnowledgeProvider, ModelRouter, Prompts, Role, RuleBook, RuleSelection, Session, Steering,
    };
    pub use ark_tools::diagram::{Diagram, DiagramKind};
    pub use ark_tools::{
        ActionOutput, Adapter, AdapterError, AdapterHealth, ProducedArtifact, Registry, RunCtx,
    };
    // The underlying crates, so tool authors can name `ark_tools::Result`, build a provider
    // with `ark_model::from_env_arc()`, etc., from the prelude alone.
    pub use {
        ark_browser, ark_computer, ark_connect, ark_core, ark_exec, ark_files, ark_model, ark_os,
        ark_proc, ark_runtime, ark_tools,
    };
}

#[derive(Debug, thiserror::Error)]
pub enum ArkError {
    #[error("no model configured: set .provider(), .router(), or the ARK_PROVIDER env var")]
    NoModel,
    #[error(transparent)]
    Provider(#[from] ark_model::ProviderError),
    #[error(transparent)]
    Runtime(#[from] ark_runtime::AgentError),
    #[error("the answer could not be saved: {reason}")]
    ResultNotStored { answer: String, reason: String },
    #[error("{reason}")]
    AdmissionRejected { reason: String },
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
    /// Authority frozen for this turn.
    pub execution_mode: ark_core::ExecutionMode,
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
    /// The request budget set by the app or `ARK_CTX_BUDGET`. `None`: each agent's is sized
    /// from its model's window (ADR 0050).
    pub context_budget: Option<u32>,
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
    /// The same router, shared with what follows it (agents without a model of their own).
    current: CurrentModel,
    subscriptions: SubscriptionRuntime,
    profile: AgentProfile,
    max_steps: usize,
    budget: Option<u32>,
    planner: bool,
    critic: bool,
    allow_mutate: bool,
    /// The skills the conversation may read (`*`, names; empty: none).
    skills: Vec<String>,
    /// File names of the project folder's own instructions ([`ArkBuilder::project_instructions`]).
    instructions: Vec<String>,
    /// Tools each agent gets its own of: the web, the browser ([`ArkBuilder::web`],
    /// [`ArkBuilder::browser`]).
    per_agent: Vec<Arc<dyn PerAgent>>,
    /// Capabilities that need an account, when the app turned them on
    /// ([`ArkBuilder::connections`]).
    connections: Option<Connections>,
    /// The Computer, when the app turned it on ([`ArkBuilder::computer`]).
    computer: Option<Arc<ark_computer::ComputerService>>,
    /// Project processes, when the app turned them on ([`ArkBuilder::processes`]).
    processes: Option<ark_proc::Processes>,
    /// The shared Chromium service, when the app turned it on. Front-ends use the same service
    /// for a person's runtime-bound Browser without gaining access to an agent's page.
    browser: Option<Arc<ark_browser::BrowserService>>,
    /// Other agent programs an agent can run on, when the app offers them
    /// ([`ArkBuilder::workers`]).
    workers: Option<Arc<dyn ark_runtime::worker::Workers>>,
}

/// Another handle on the same agent: the store, tools, model sessions and capabilities are
/// shared; the clone's model can be changed on its own ([`Ark::set_router`] also moves what
/// follows [`Ark::current_model`]). One app runs several live conversations this way.
impl Clone for Ark {
    fn clone(&self) -> Self {
        Ark {
            engine: self.engine.clone(),
            router: self.router.clone_view(),
            current: self.current.clone(),
            subscriptions: self.subscriptions.clone(),
            profile: self.profile.clone(),
            max_steps: self.max_steps,
            budget: self.budget,
            planner: self.planner,
            critic: self.critic,
            allow_mutate: self.allow_mutate,
            skills: self.skills.clone(),
            instructions: self.instructions.clone(),
            per_agent: self.per_agent.clone(),
            connections: self.connections.clone(),
            computer: self.computer.clone(),
            processes: self.processes.clone(),
            browser: self.browser.clone(),
            workers: self.workers.clone(),
        }
    }
}

/// Every action a turn would be offered now: the engine's tools (the app's, the plugins and
/// the connected capabilities) and the per-agent ones (the web, the browser). For lists such as
/// `/tools`; a turn builds its own.
pub fn offered_tools(engine: &Engine, per_agent: &[Arc<dyn PerAgent>]) -> Registry {
    let mut tools = engine.tools();
    for p in per_agent {
        tools.register(p.listing());
    }
    tools
}

/// Every action offered for a particular project.
pub fn offered_tools_for(
    engine: &Engine,
    project: &ProjectId,
    per_agent: &[Arc<dyn PerAgent>],
) -> Registry {
    let mut tools = engine.tools_for(project);
    for provider in per_agent {
        tools.register(provider.listing());
    }
    tools
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

    /// Swap the model(s) used from the next turn on. Agents that have no model of their own
    /// follow it ([`current_model`](Ark::current_model)).
    pub fn set_router(&mut self, router: ModelRouter) {
        self.current.set(router.clone_view());
        self.router = router;
    }

    /// Run on the shared model again ([`current_model`](Ark::current_model)), after another
    /// handle on this agent changed it.
    pub fn follow_current_model(&mut self) {
        self.router = self.current.get();
    }

    /// The Computer, when the app offers it: who uses the screen, and taking it back.
    pub fn computer(&self) -> Option<&Arc<ark_computer::ComputerService>> {
        self.computer.as_ref()
    }

    /// Project processes, when the app offers them: what runs, its output, stopping it.
    pub fn processes(&self) -> Option<&ark_proc::Processes> {
        self.processes.as_ref()
    }

    /// Chromium on this runtime, when offered by the app.
    pub fn browser_service(&self) -> Option<&Arc<ark_browser::BrowserService>> {
        self.browser.as_ref()
    }

    /// The other agent programs an agent can run on (`agent:<name>` as its model), when the
    /// app offers them (ADR 0043).
    pub fn workers(&self) -> Option<&Arc<dyn ark_runtime::worker::Workers>> {
        self.workers.as_ref()
    }

    /// The model this `Ark` runs on now, as a handle that sees later
    /// [`set_router`](Ark::set_router) calls. An orchestrator resolves agents that have no
    /// model of their own from it.
    pub fn current_model(&self) -> CurrentModel {
        self.current.clone()
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

    /// A lower-level harness bound to an explicit, stable conversation identifier.
    pub fn harness_for_conversation(&self, conversation: &str) -> Result<Harness, ArkError> {
        Ok(self.harness_with(self.router.for_conversation(conversation)?))
    }

    fn harness_with(&self, router: ModelRouter) -> Harness {
        Harness::new(router)
            .max_steps(self.max_steps)
            .budget_or_auto(self.budget)
            .planner(self.planner)
            .critic(self.critic)
            .allow_mutate(self.allow_mutate)
            .stopping(Some(self.engine.stopping()))
    }

    /// Start a new conversation session in `project`.
    pub fn start_session(&self, project: &ProjectId) -> Result<Session, ArkError> {
        Ok(Session::start(&self.engine, project)?)
    }

    /// Start a new conversation session with an identity chosen by a trusted caller.
    pub fn start_named_session(
        &self,
        id: SessionId,
        project: &ProjectId,
    ) -> Result<Session, ArkError> {
        Ok(Session::start_named(&self.engine, id, project)?)
    }

    /// Start a named conversation with an exact model choice stored in its first record.
    pub fn start_named_session_model(
        &self,
        id: SessionId,
        project: &ProjectId,
        model: String,
    ) -> Result<Session, ArkError> {
        Ok(Session::start_named_model(
            &self.engine,
            id,
            project,
            model,
        )?)
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
        session.refresh_context(&self.engine)?;
        let project = session.project().clone();
        let execution_project = session.execution_project(&self.engine)?;
        session.prepare_artifacts(&self.engine, input)?;
        let router = self.session_router(session).await?;
        // The turn before did not finish (the app stopped, it failed, the person stopped it):
        // this one continues it (ADR 0048). Its goal is that turn's goal with this message
        // added, so "continue" never becomes the task (ADR 0050), and what is chosen from the
        // task (rules, skills, tools) is chosen from the goal.
        let unfinished = session.unfinished(&self.engine)?;
        let goal = session.goal_for(unfinished.as_ref(), input);
        let goal_text = goal
            .as_ref()
            .map(|g| g.text())
            .unwrap_or_else(|| input.to_string());
        let (engine, rules) = self.with_rules(&project, &execution_project, &goal_text)?;
        let (engine, catalog) = self.with_skills(engine, &project);
        let owner = format!("session:{}", session.id());
        let tools = self.turn_tools(&opts.tools, &router, &owner);
        let (engine, mut profile) = self.with_tools(engine, &tools);
        self.owned(&mut profile, &owner, &router);
        if catalog.is_some() {
            // The catalog says to call it, so it is loaded, not left to discovery.
            let p = profile.to_mut();
            p.distillers.push(skills::distiller());
            p.core_actions.push(skills::READ.into());
        }
        // What work this chat set up has posted since its last answer.
        session.read_posts(&self.engine)?;
        let harness = self.harness_with(router);
        // Its changes are not made again, one cut short is asked about again, and the model is
        // told what it did, sized to the request budget.
        let carry = unfinished
            .as_ref()
            .map(|u| u.carry(&engine.continued(&u.works), harness.request_budget()));
        let digest = match (carry, session.conversation_digest()) {
            (Some(c), Some(d)) => Some(format!("{d}\n{c}")),
            (c, d) => c.or(d),
        };
        let (done_before, cut_short, works) = unfinished
            .map(|u| (u.calls.clone(), u.cut_short(), u.works))
            .unwrap_or_default();
        // Its calls name the turn, so the next one finds them if this one stops too; the tools
        // know which stopped turns it continues.
        let engine = Arc::new(
            engine
                .for_work(session.work_of(session.turn_count()))
                .continuing(works),
        );
        let harness = harness
            .goal(goal.clone())
            .conversation(digest)
            .done_before(done_before)
            .cut_short(cut_short)
            .approvals(opts.approvals)
            .cancel(opts.cancel)
            .steering(opts.steer)
            .presentation(opts.present)
            .focus(opts.focus)
            .execution_mode(opts.execution_mode)
            .rules(rules)
            .skills(catalog);
        session.begin_turn_with_goal(&self.engine, input, goal)?;
        let mut on_event = on_event;
        let result = {
            let stored = &mut *session;
            let store = &self.engine;
            harness
                .ask(&engine, &profile, &execution_project, input, move |ev| {
                    // What the person said while it ran is part of the goal: kept now, so a
                    // crash keeps it.
                    if let HarnessEvent::Steered { text, context } = &ev {
                        let retained = ark_runtime::steer::SteeringInput {
                            text: text.clone(),
                            context: context.clone(),
                        }
                        .model_text();
                        if let Err(e) = stored.steered_now(store, &retained) {
                            tracing::warn!("could not keep a message sent while the turn ran: {e}");
                        }
                    }
                    on_event(ev)
                })
                .await
        };
        match result {
            Ok(outcome) => {
                if let Err(error) = session.record_turn(&self.engine, input, &outcome) {
                    return Err(ArkError::ResultNotStored {
                        answer: outcome.answer,
                        reason: error.to_string(),
                    });
                }
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
    pub async fn session_router(&self, session: &Session) -> Result<ModelRouter, ArkError> {
        let router = self
            .router
            .for_conversation(&format!("chat/{}", session.id()))?;
        let strong = router.provider(Role::Reason);
        let spec = strong.model_id();
        if !self.subscriptions.policy().reuse || !self.subscriptions.manages(&spec) {
            return Ok(router);
        }
        let lane = format!("session/{}", session.id());
        let Some(managed) = self.subscriptions.session(&lane, &spec).await else {
            return Ok(router);
        };
        let fast = router.provider(Role::Plan);
        let fast = if Arc::ptr_eq(fast, strong) {
            self.subscriptions
                .stateless(&spec)
                .unwrap_or_else(|| fast.clone())
        } else {
            fast.clone()
        };
        Ok(ModelRouter::new(fast, managed))
    }

    /// The store's rule sets: put a project's standards here and bind them to the project
    /// (`RuleScope::Project`) so its turns follow them.
    pub fn rule_book(&self) -> RuleBook<'_> {
        RuleBook::new(&self.engine.store)
    }

    /// The engine a turn starts with: the app's tools and the MCP plugins connected now. A
    /// plugin added or removed later changes the next turn, not this one.
    fn turn_engine(&self, project: &ProjectId) -> Arc<Engine> {
        if self.engine.has_sources() {
            Arc::new(self.engine.for_project_turn(project))
        } else {
            self.engine.clone()
        }
    }

    /// The rules bound to `project`, selected for `input`, and the engine to run with: the
    /// turn's, or a view of it that also offers the `rules` tool when rules are bound.
    fn with_rules(
        &self,
        project: &ProjectId,
        execution_project: &ProjectId,
        input: &str,
    ) -> Result<(Arc<Engine>, Option<RuleSelection>), ArkError> {
        let engine = self.turn_engine(execution_project);
        let words: Vec<_> = input
            .split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .map(str::to_lowercase)
            .collect();
        let requested = words
            .iter()
            .any(|w| matches!(w.as_str(), "remember" | "memorize"))
            || words.windows(2).any(|w| w[0] == "learn" && w[1] == "from")
            || (words.iter().any(|w| w == "save") && words.iter().any(|w| w == "correction"));
        let enabled = requested
            || !engine
                .store
                .project_memories(project)
                .map_err(ark_runtime::AgentError::from)?
                .is_empty();
        let engine = if enabled {
            Arc::new(engine.scoped(engine.registry.clone().with(Arc::new(
                ark_runtime::memory::MemoryTool::new(engine.store.clone(), project.clone()),
            ))))
        } else {
            engine
        };
        let sets = self
            .rule_book()
            .resolve(&[RuleScope::Project(project.to_string())])?;
        let instructions = engine.workspace_dir(project).ok().and_then(|dir| {
            ark_runtime::rules::project_instructions(
                &dir,
                &self.instructions,
                input,
                ark_runtime::rules::INSTRUCTIONS_BUDGET,
            )
        });
        if sets.is_empty() {
            return Ok((
                engine,
                instructions.map(|p| RuleSelection::default().with_project(p)),
            ));
        }
        let mut selection =
            ark_runtime::rules::select(&sets, input, ark_runtime::rules::RULES_BUDGET);
        if let Some(p) = instructions {
            selection = selection.with_project(p);
        }
        let registry = engine.registry.clone().with(Arc::new(RulesTool::new(sets)));
        Ok((Arc::new(engine.scoped(registry)), Some(selection)))
    }

    /// The skills a conversation turn may read, and the engine that offers `skills.read` for
    /// them. None found (or none allowed): the engine as it was, no catalog.
    fn with_skills(
        &self,
        engine: Arc<Engine>,
        project: &ProjectId,
    ) -> (Arc<Engine>, Option<String>) {
        if self.skills.is_empty() {
            return (engine, None);
        }
        let found = skills::allowed(
            &skills::discover(&skills::dirs(&engine, project)),
            &self.skills,
        );
        let Some(catalog) = skills::catalog(&found, skills::CATALOG_BUDGET) else {
            return (engine, None);
        };
        let registry = engine
            .registry
            .clone()
            .with(Arc::new(skills::SkillsTool::new(found)));
        (Arc::new(engine.scoped(registry)), Some(catalog.text))
    }

    /// The names the conversation's skills are chosen by (`*`, names).
    pub fn skill_names(&self) -> &[String] {
        &self.skills
    }

    /// The skills this project's conversation may read, as found now.
    pub fn skills(&self, project: &ProjectId) -> Vec<skills::Skill> {
        skills::allowed(
            &skills::discover(&skills::dirs(&self.engine, project)),
            &self.skills,
        )
    }

    /// Every action a turn would be offered now ([`offered_tools`]).
    pub fn tools(&self) -> Registry {
        offered_tools(&self.engine, &self.per_agent)
    }

    pub fn tools_for(&self, project: &ProjectId) -> Registry {
        offered_tools_for(&self.engine, project, &self.per_agent)
    }

    /// The accounts and capabilities this app can connect; `None` unless it turned them on.
    pub fn connections(&self) -> Option<&Connections> {
        self.connections.as_ref()
    }

    /// The sources of per-agent tools the app turned on (the web, the browser).
    pub fn per_agent(&self) -> &[Arc<dyn PerAgent>] {
        &self.per_agent
    }

    /// The per-agent tools of one owner: its web search on its model, its own browser. An
    /// orchestrator asks for each agent's at each task.
    pub fn agent_tools(&self, owner: &Owner) -> Vec<TurnTools> {
        self.per_agent
            .iter()
            .map(|p| TurnTools {
                adapter: p.for_owner(owner),
                distiller: p.distiller(),
            })
            .collect()
    }

    /// A turn's tools: the front-end's, and the owner's per-agent ones on the turn's planning
    /// model (a search needs no reasoning).
    fn turn_tools(&self, tools: &[TurnTools], router: &ModelRouter, owner: &str) -> Vec<TurnTools> {
        let mut all = tools.to_vec();
        let reason = router.provider(Role::Reason);
        if reason.generates_images() {
            all.push(TurnTools {
                adapter: Arc::new(ImageGenerator {
                    provider: reason.clone(),
                }),
                distiller: None,
            });
        }
        all.extend(self.agent_tools(&Owner {
            name: owner,
            model: router.provider(Role::Plan),
        }));
        all
    }

    /// What one owner can ask to turn on mid-turn ([`PerAgent::acquisition`]), after the app's
    /// own sources (accounts, installs): a need goes to the first that knows it.
    pub fn acquisition_for(
        &self,
        base: Option<Arc<dyn ark_runtime::AcquisitionProvider>>,
        owner: &Owner,
    ) -> Option<Arc<dyn ark_runtime::AcquisitionProvider>> {
        let mut sources: Vec<Arc<dyn ark_runtime::AcquisitionProvider>> =
            base.into_iter().collect();
        sources.extend(self.per_agent.iter().filter_map(|p| p.acquisition(owner)));
        match sources.len() {
            0 => None,
            1 => sources.pop(),
            _ => Some(Arc::new(ark_runtime::Sources(sources))),
        }
    }

    /// A turn's profile with what its owner can ask to turn on.
    fn owned(
        &self,
        profile: &mut std::borrow::Cow<'_, AgentProfile>,
        owner: &str,
        router: &ModelRouter,
    ) {
        let owner = Owner {
            name: owner,
            model: router.provider(Role::Plan),
        };
        if !self
            .per_agent
            .iter()
            .any(|p| p.acquisition(&owner).is_some())
        {
            return;
        }
        let p = profile.to_mut();
        p.acquisition = self.acquisition_for(p.acquisition.take(), &owner);
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
        self.context_preview_task(session, present, tools, "").await
    }

    /// Preview selection for a concrete next message, without calling a model.
    pub async fn context_preview_task(
        &self,
        session: &Session,
        present: bool,
        tools: &[TurnTools],
        task: &str,
    ) -> Result<ContextReport, ArkError> {
        let execution_project = session.execution_project(&self.engine)?;
        let unfinished = session.unfinished(&self.engine)?;
        let goal = session.goal_for(unfinished.as_ref(), task);
        let query = goal
            .as_ref()
            .map(|g| g.text())
            .unwrap_or_else(|| task.to_string());
        let router = self.session_router(session).await?;
        let (engine, rules) = self.with_rules(session.project(), &execution_project, &query)?;
        let (engine, catalog) = self.with_skills(engine, session.project());
        let owner = format!("session:{}", session.id());
        let tools = self.turn_tools(tools, &router, &owner);
        let (engine, mut profile) = self.with_tools(engine, &tools);
        self.owned(&mut profile, &owner, &router);
        if catalog.is_some() {
            let p = profile.to_mut();
            p.distillers.push(skills::distiller());
            p.core_actions.push(skills::READ.into());
        }
        Ok(self
            .harness_with(router)
            .goal(goal)
            .presentation(present)
            .conversation(session.conversation_digest())
            .rules(rules)
            .skills(catalog)
            .preview_context(&engine, &profile, &execution_project, task)
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
        let router = self
            .router
            .for_conversation(&format!("run/{}", uuid::Uuid::new_v4()))?;
        let tools = self.turn_tools(&[], &router, "ark");
        let (engine, mut profile) = self.with_tools(self.turn_engine(project), &tools);
        self.owned(&mut profile, "ark", &router);
        Ok(self
            .harness_with(router)
            .ask(&engine, &profile, project, task, on_event)
            .await?)
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
    budget: Option<u32>,
    planner: bool,
    critic: bool,
    allow_mutate: bool,
    subscriptions: Option<SubscriptionRuntime>,
    skills: Vec<String>,
    instructions: Vec<String>,
    web: Option<WebAdapter>,
    files: bool,
    code: Option<CodeChoice>,
    browser: Option<BrowserChoice>,
    computer: Option<ComputerChoice>,
    processes: Option<ProcessesChoice>,
    workers: Option<WorkersChoice>,
    connections: Option<ConnectionsChoice>,
    secrets: Option<Arc<dyn ark_connect::Secrets>>,
    sources: Vec<Arc<dyn ark_tools::ToolSource>>,
}
enum CodeChoice {
    Default,
    Given(Box<ark_exec::Exec>),
}

enum ProcessesChoice {
    /// The person's shell, records in the store root's `processes` folder.
    Default,
    Given(ark_proc::Processes),
}

enum ComputerChoice {
    /// This computer, where ARK can use apps (macOS).
    Native,
    Given(Arc<ark_computer::ComputerService>),
}

enum BrowserChoice {
    /// The installed Chrome, Edge or Brave, profiles in the store root's `browser` folder.
    Default,
    Given(Arc<ark_browser::BrowserService>),
}

enum ConnectionsChoice {
    /// The built-in catalog, tokens in the OS keychain (or the app's
    /// [`secrets`](ArkBuilder::secrets)), records in the store root.
    Default,
    Given(Connections),
}

enum WorkersChoice {
    Default,
    Given(Arc<dyn ark_runtime::worker::Workers>),
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
            budget: ark_runtime::budget::budget_from_env(),
            planner: false,
            critic: true,
            allow_mutate: false,
            subscriptions: None,
            skills: vec!["*".into()],
            instructions: vec!["AGENTS.md".into()],
            web: None,
            files: false,
            code: None,
            browser: None,
            computer: None,
            processes: None,
            workers: None,
            connections: None,
            secrets: None,
            sources: Vec::new(),
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
    /// Add a consumer-owned source whose adapters can change between turns.
    /// The engine snapshots it at the start of each turn.
    pub fn source(mut self, source: Arc<dyn ark_tools::ToolSource>) -> Self {
        self.sources.push(source);
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
    /// Fix the request budget of every agent. Without it each agent's is sized from its
    /// model's window (ADR 0050).
    pub fn context_budget(mut self, tokens: u32) -> Self {
        self.budget = Some(tokens);
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

    /// The skills a conversation turn may read: names, `*` (the default: every skill of the
    /// project folder's `.agents/skills` and the store's `skills`), or none. Agents of an
    /// orchestrator choose their own (`AgentSpec::skills`).
    pub fn skills<S: Into<String>>(mut self, names: impl IntoIterator<Item = S>) -> Self {
        self.skills = names.into_iter().map(Into::into).collect();
        self
    }

    /// The project folder's own instructions for agents, by file name, the first found at the
    /// folder's root: its sections go into each conversation turn as the project's rules, within
    /// a budget, with the subfolders that have their own named (ADR 0051). Default
    /// `["AGENTS.md"]`; none turns it off. A name is a file name, not a path.
    pub fn project_instructions<S: Into<String>>(
        mut self,
        names: impl IntoIterator<Item = S>,
    ) -> Self {
        self.instructions = names.into_iter().map(Into::into).collect();
        self
    }

    /// Offer the `web` tool (`web.search`, `web.fetch`; [`ark_tools::web`]) in every turn and to
    /// agents whose tools allow it. Off by default.
    pub fn web(mut self, yes: bool) -> Self {
        self.web = yes.then(WebAdapter::new);
        self
    }

    /// Offer the Files tools (`files.list`, `read`, `search`, `write`, `edit`; [`ark_files`]):
    /// the project folder's text files, spreadsheets (`.xlsx`, `.csv`) and documents (`.docx`),
    /// read and changed in place. Writing asks first. Off by default.
    pub fn files(mut self, yes: bool) -> Self {
        self.files = yes;
        self
    }

    /// Offer Run code (`code.run`; [`ark_exec`]): agents run programs (Python first) in the
    /// project folder, contained: no network, writes only there. Every file a run writes comes
    /// back as an artifact the window shows by its kind. Asks before each run unless the person
    /// allows it in the project. Runs are contained on macOS, on Linux with bubblewrap, and on
    /// Windows in an AppContainer when the program calls [`ark_exec::entry`] first in `main`;
    /// where ARK cannot contain a run it is not offered. Python is set up with `uv` under the
    /// store on first use. Off by default.
    pub fn code(mut self, yes: bool) -> Self {
        self.code = yes.then_some(CodeChoice::Default);
        self
    }

    /// [`code`](ArkBuilder::code) with tools the app set up (its own `uv`, other runtimes).
    pub fn code_with(mut self, exec: ark_exec::Exec) -> Self {
        self.code = Some(CodeChoice::Given(Box::new(exec)));
        self
    }

    /// Offer capabilities that need an account (Mail, Calendar; [`ark_connect`]). Their records
    /// go in the store root's `connections.json`, their tokens in the OS keychain. A connected
    /// capability's tools join every turn, and agents whose `tools` allow them. Off by default.
    pub fn connections(mut self, yes: bool) -> Self {
        self.connections = yes.then_some(ConnectionsChoice::Default);
        self
    }

    /// Where account tokens and saved API keys are kept, instead of the OS keychain. A server
    /// uses [`ark_connect::FileSecrets`], since a Linux kernel keyring is gone after a restart
    /// (ADR 0028): `.secrets(Arc::new(FileSecrets::in_store(&root)?))`.
    pub fn secrets(mut self, secrets: Arc<dyn ark_connect::Secrets>) -> Self {
        self.secrets = Some(secrets);
        self
    }

    /// [`connections`](ArkBuilder::connections) with a set the app opened: its own catalog or
    /// secret store.
    pub fn connections_with(mut self, c: Connections) -> Self {
        self.connections = Some(ConnectionsChoice::Given(c));
        self
    }

    /// Offer the Browser (`browser.open`, `read`, `click`, `type`, `back`; [`ark_browser`]): the
    /// installed Chrome, Edge or Brave, a context of its own per agent, and a signed-in browser
    /// one agent uses at a time. Off by default.
    pub fn browser(mut self, yes: bool) -> Self {
        self.browser = yes.then_some(BrowserChoice::Default);
        self
    }

    /// Offer the Computer (`computer.apps`, `look`, `open`, `click`, `type`, `keys`, ...;
    /// [`ark_computer`]): agents use the apps on this computer, one agent at a time, every
    /// action asked. It stays off until the person turns it on: an agent that needs it asks
    /// mid-task (`acquire_capability`), or the person turns it on in the capability list. The
    /// answer is kept in the store's `computer.json` (ADR 0045). Where the platform has no
    /// support (only macOS for now) it is not offered. Not offered by default.
    pub fn computer(mut self, yes: bool) -> Self {
        self.computer = yes.then_some(ComputerChoice::Native);
        self
    }

    /// Offer project processes (`process.start`, `list`, `stop`; [`ark_proc`]): agents start
    /// long-running commands in the project folder (a dev server, a watcher) with the person's
    /// shell and tools. Not contained: every start asks, per command and folder, and "Always in
    /// this project" covers only that command there. The window shows each process and its
    /// output and stops it; a missing program (Node.js, Rust, ...) is installed through
    /// `acquire_capability` after the person allows it. macOS and Linux. Off by default.
    pub fn processes(mut self, yes: bool) -> Self {
        self.processes = yes.then_some(ProcessesChoice::Default);
        self
    }

    /// Let an agent run on another agent program installed here, with its own tools (Claude
    /// Code, Codex; ADR 0043): its model spec names it (`agent:claude-code`). The task stays
    /// ARK's: what the program wants to change asks the person through ARK, and its results
    /// come from the project folder. ARK's own agent stays the default.
    pub fn workers(mut self, yes: bool) -> Self {
        self.workers = yes.then_some(WorkersChoice::Default);
        self
    }

    /// [`workers`](ArkBuilder::workers) with the app's own catalog.
    pub fn workers_with(mut self, w: Arc<dyn ark_runtime::worker::Workers>) -> Self {
        self.workers = Some(WorkersChoice::Given(w));
        self
    }

    /// [`processes`](ArkBuilder::processes) with a service the app set up (its own shell).
    pub fn processes_with(mut self, p: ark_proc::Processes) -> Self {
        self.processes = Some(ProcessesChoice::Given(p));
        self
    }

    /// [`computer`](ArkBuilder::computer) with a service the app set up (tests, another screen).
    pub fn computer_with(mut self, service: Arc<ark_computer::ComputerService>) -> Self {
        self.computer = Some(ComputerChoice::Given(service));
        self
    }

    /// [`browser`](ArkBuilder::browser) with a service the app set up (tests, its own profiles).
    pub fn browser_with(mut self, service: Arc<ark_browser::BrowserService>) -> Self {
        self.browser = Some(BrowserChoice::Given(service));
        self
    }

    /// [`web`](ArkBuilder::web) with an adapter set up by the app: its own search backends,
    /// or private addresses allowed for an intranet.
    pub fn web_with(mut self, web: WebAdapter) -> Self {
        self.web = Some(web);
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
        let mut engine = match self.engine {
            Some(e) => e,
            None => Engine::open_with(&self.store_root, self.registry)?,
        };
        let connections = self.connections.map(|c| match c {
            ConnectionsChoice::Given(c) => c,
            ConnectionsChoice::Default => Connections::open(
                engine.store.root(),
                ark_connect::Catalog::builtin(),
                self.secrets
                    .clone()
                    .unwrap_or_else(|| Arc::new(ark_connect::Keychain)),
            ),
        });
        if let Some(c) = &connections {
            ark_model::keys::set_source(Arc::new(c.api_keys().clone()));
        }
        let router = match (self.router, self.provider) {
            (Some(r), _) => r,
            (None, Some(p)) => ModelRouter::single(p),
            (None, None) => ModelRouter::from_env()?,
        };
        let mut profile = self.profile;
        // A stored copy of a project file has the kind ark-files gives the file, whichever
        // tool wrote it: one decision for the agent, the store and the window.
        engine.register_sniffer(Arc::new(ark_files::Kinds));
        let exec = match self.code {
            Some(CodeChoice::Given(e)) => Some(*e),
            Some(CodeChoice::Default) => {
                let root = engine.store.root().join("exec");
                ark_exec::Exec::new(
                    root.join("runs"),
                    vec![Arc::new(ark_exec::Python::new(root.join("python")))],
                )
            }
            None => None,
        };
        if let Some(exec) = exec {
            let dir = engine.store.root().join("exec").join("skills");
            match exec.write_skills(&dir) {
                Ok(()) => engine.add_skills(dir),
                Err(e) => tracing::warn!("Run code's skills not written to {}: {e}", dir.display()),
            }
            engine.register(Arc::new(exec.shows(ark_files::shown())));
            profile.distillers.push(ark_exec::distill());
        }
        if self.files {
            engine.register(Arc::new(ark_files::Files));
            profile.distillers.push(ark_files::distill());
        }
        let processes = match self.processes {
            Some(ProcessesChoice::Default) => {
                ark_proc::Processes::open(engine.store.root().join("processes"))
            }
            Some(ProcessesChoice::Given(p)) => Some(p),
            None => None,
        };
        // What an agent may ask for mid-task: accounts to connect, programs to install.
        let mut sources: Vec<Arc<dyn ark_runtime::AcquisitionProvider>> = Vec::new();
        if let Some(c) = &connections {
            // Saved API keys reach the model APIs; the environment still comes first.
            ark_model::keys::set_source(Arc::new(c.api_keys().clone()));
            engine.add_source(Arc::new(c.clone()));
            profile
                .distillers
                .push(Arc::new(ark_connect::services::distill));
            sources.push(Arc::new(c.clone()));
        }
        for source in self.sources {
            engine.try_add_source(source)?;
        }
        if let Some(p) = &processes {
            engine.try_register(Arc::new(ark_proc::ProcessTool(p.clone())))?;
            profile.distillers.push(ark_proc::distill());
            sources.push(Arc::new(ark_proc::Installs::new(p.clone())));
        }
        if profile.acquisition.is_none() {
            profile.acquisition = match sources.len() {
                0 => None,
                1 => sources.pop(),
                _ => Some(Arc::new(ark_runtime::Sources(sources))),
            };
        }
        let mut per_agent: Vec<Arc<dyn PerAgent>> = Vec::new();
        if let Some(w) = self.web {
            per_agent.push(Arc::new(ark_runtime::web::Web(w)));
        }
        let browser = match self.browser {
            Some(BrowserChoice::Default) => Some(ark_browser::BrowserService::new(
                engine.store.root().join("browser"),
            )),
            Some(BrowserChoice::Given(svc)) => Some(svc),
            None => None,
        };
        if let Some(svc) = &browser {
            // Agents open the pages their project's own processes serve (ADR 0046). A weak
            // handle: what runs still stops with the runtime.
            if let Some(p) = &processes {
                let p = p.downgrade();
                svc.serve_local_pages(Arc::new(move |ws| {
                    p.upgrade().map(|p| p.serving(ws)).unwrap_or_default()
                }));
            }
            per_agent.push(Arc::new(ark_browser::Browsers(svc.clone())));
        }
        let computer = match self.computer {
            // Off until the person turns it on; their answer kept in the store (ADR 0045).
            Some(ComputerChoice::Native) => ark_computer::ComputerService::native()
                .map(|s| s.switched(engine.store.root().join("computer.json"))),
            Some(ComputerChoice::Given(svc)) => Some(svc),
            None => None,
        };
        if let Some(svc) = &computer {
            per_agent.push(Arc::new(ark_computer::Computers(svc.clone())));
        }
        let workers = self.workers.map(|workers| match workers {
            WorkersChoice::Default => {
                Arc::new(ark_acp::Catalog::at(engine.store.root().join("workers")))
                    as Arc<dyn ark_runtime::worker::Workers>
            }
            WorkersChoice::Given(workers) => workers,
        });
        let engine = Arc::new(engine);
        let subscriptions = self.subscriptions.unwrap_or_else(|| {
            SubscriptionRuntime::builder()
                .ledger(Arc::new(StoreLedger::new(engine.store.clone())))
                .build()
        });
        Ok(Ark {
            engine,
            subscriptions,
            current: CurrentModel::new(router.clone_view()),
            router,
            profile,
            max_steps: self.max_steps,
            budget: self.budget,
            planner: self.planner,
            critic: self.critic,
            allow_mutate: self.allow_mutate,
            skills: self.skills,
            instructions: self.instructions,
            per_agent,
            connections,
            computer,
            processes,
            browser,
            workers,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::prelude::*;
    use super::{validate_generated_png, ImageGenerator, IMAGE_GENERATE};
    use ark_model::{Completion, GeneratedImage, MockProvider, Provider, ToolCall, Usage};
    use ark_tools::{Adapter, AdapterError, BlobResolver, RunCtx};
    use std::path::PathBuf;
    use std::sync::Arc;

    #[tokio::test]
    async fn project_memory_suggestions_have_no_empty_project_overhead() {
        use ark_core::{Invocation, MemorySource, RiskTier, SessionId};
        use ark_runtime::{
            catalog::capability_index,
            memory::{MemoryReview, SUGGEST},
        };
        let dir = tempfile::tempdir().unwrap();
        let model = Arc::new(MockProvider::new("memory-test", vec![]));
        let ark = Ark::builder()
            .store_root(dir.path())
            .provider(model.clone())
            .planner(false)
            .critic(false)
            .build()
            .unwrap();
        let project = ark.create_project("memory", "").unwrap();
        let base = ark.turn_engine(&project.id);
        for input in ["", "write a report", "a remembered detail", "fix the code"] {
            let (engine, _) = ark.with_rules(&project.id, &project.id, input).unwrap();
            assert_eq!(
                capability_index(&engine.registry),
                capability_index(&base.registry)
            );
            assert!(engine.registry.descriptor(SUGGEST).is_none());
        }
        for input in [
            "REMEMBER this correction",
            "memorize the convention",
            "learn from this correction",
            "learn  \n from this correction",
            "save this correction",
        ] {
            let (engine, _) = ark.with_rules(&project.id, &project.id, input).unwrap();
            assert!(engine.registry.descriptor(SUGGEST).is_some());
        }
        let session = SessionId::new();
        base.store
            .create_session(session.as_str(), &project.id, &serde_json::json!({}))
            .unwrap();
        let inv = Invocation::new(SUGGEST, RiskTier::Mutate, serde_json::json!({}));
        base.store.put_invocation(&project.id, &inv).unwrap();
        let pending = base
            .store
            .record_memory(
                &project.id,
                "report",
                "Use a bullet list.",
                MemorySource::Agent { invocation: inv.id },
                None,
            )
            .unwrap();
        assert!(ark
            .with_rules(&project.id, &project.id, "write a report")
            .unwrap()
            .0
            .registry
            .descriptor(SUGGEST)
            .is_some());
        base.store
            .review_memory(
                &project.id,
                pending.id.as_str(),
                MemoryReview::Accept,
                &session,
            )
            .unwrap();
        assert!(ark
            .with_rules(&project.id, &project.id, "write a report")
            .unwrap()
            .0
            .registry
            .descriptor(SUGGEST)
            .is_some());
        base.store
            .review_memory(
                &project.id,
                pending.id.as_str(),
                MemoryReview::Forget,
                &session,
            )
            .unwrap();
        assert!(ark
            .with_rules(&project.id, &project.id, "write a report")
            .unwrap()
            .0
            .registry
            .descriptor(SUGGEST)
            .is_none());
        let mut chat = ark.start_session(&project.id).unwrap();
        let preview = ark
            .context_preview_task(&chat, false, &[], "write a report")
            .await
            .unwrap();
        let outcome = ark
            .ask_session(&mut chat, "write a report", TurnOptions::default(), |_| {})
            .await
            .unwrap();
        assert_eq!(preview.memory, 0);
        assert_eq!(outcome.metrics.steps, 1);
        assert!(!model.recorded()[0].system.contains(SUGGEST));
        ark.ask_session(
            &mut chat,
            "remember this correction",
            TurnOptions::default(),
            |_| {},
        )
        .await
        .unwrap();
        assert!(model.recorded()[1].system.contains(SUGGEST));
    }

    struct NoBlobs;
    impl BlobResolver for NoBlobs {
        fn path(&self, _id: &ArtifactId) -> Option<PathBuf> {
            None
        }
        fn read(&self, id: &ArtifactId) -> ark_tools::Result<Vec<u8>> {
            Err(AdapterError::ArtifactNotFound(id.to_string()))
        }
    }

    struct ImageProvider(Vec<u8>);
    #[async_trait::async_trait]
    impl Provider for ImageProvider {
        async fn complete(
            &self,
            _system: &str,
            _messages: &[ark_model::Message],
            _tools: &[ark_model::ToolDef],
        ) -> ark_model::Result<Completion> {
            unreachable!()
        }
        fn generates_images(&self) -> bool {
            true
        }
        async fn generate_image(&self, _prompt: &str) -> Option<ark_model::Result<GeneratedImage>> {
            Some(Ok(GeneratedImage {
                media_type: "image/png".into(),
                bytes: self.0.clone(),
            }))
        }
        fn describe(&self) -> String {
            "image-test".into()
        }
    }

    fn small_png() -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 2, 2);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[0; 12]).unwrap();
        }
        bytes
    }

    #[test]
    fn generated_pngs_are_bounded_before_the_project_is_written() {
        let png = small_png();
        assert_eq!(validate_generated_png("image/png", &png), Ok((2, 2)));
        assert!(validate_generated_png("image/svg+xml", b"<svg/>").is_err());
        let mut truncated = png.clone();
        truncated.truncate(24);
        assert!(validate_generated_png("image/png", &truncated).is_err());
        let mut too_wide = Vec::new();
        {
            let encoder = png::Encoder::new(&mut too_wide, 8193, 1);
            let _ = encoder.write_header().unwrap();
        }
        assert!(validate_generated_png("image/png", &too_wide).is_err());
    }

    #[tokio::test]
    async fn image_generation_asks_each_time_and_writes_a_durable_artifact() {
        let png = small_png();
        let adapter = ImageGenerator {
            provider: Arc::new(ImageProvider(png.clone())),
        };
        let action = adapter.actions().remove(0);
        assert_eq!(action.name, IMAGE_GENERATE);
        assert_eq!(action.risk, RiskTier::Mutate);
        assert!(action.ask_each_time);

        let project = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join("generated-image.png"), b"keep").unwrap();
        let ctx = RunCtx::new(&NoBlobs).with_workspace(project.path().to_path_buf());
        let output = adapter
            .run(
                IMAGE_GENERATE,
                serde_json::json!({"prompt": "a blue square"}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(output.output["path"], "generated-image-2.png");
        assert_eq!(
            std::fs::read(project.path().join("generated-image.png")).unwrap(),
            b"keep"
        );
        assert_eq!(
            std::fs::read(project.path().join("generated-image-2.png")).unwrap(),
            png
        );
        assert_eq!(output.produced.len(), 1);
        assert_eq!(output.produced[0].kind, ArtifactKind::Image);
        assert_eq!(
            output.produced[0].path.as_deref(),
            Some("generated-image-2.png")
        );
        assert_eq!(output.produced[0].bytes, png);
    }

    #[tokio::test]
    async fn invalid_generated_image_writes_nothing() {
        let adapter = ImageGenerator {
            provider: Arc::new(ImageProvider(b"not a png".to_vec())),
        };
        let project = tempfile::tempdir().unwrap();
        let ctx = RunCtx::new(&NoBlobs).with_workspace(project.path().to_path_buf());
        assert!(adapter
            .run(IMAGE_GENERATE, serde_json::json!({"prompt": "bad"}), &ctx)
            .await
            .is_err());
        assert_eq!(std::fs::read_dir(project.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dangling_symlink_is_a_collision_not_an_infinite_retry() {
        let project = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("missing.png", project.path().join("generated-image.png"))
            .unwrap();
        let adapter = ImageGenerator {
            provider: Arc::new(ImageProvider(small_png())),
        };
        let ctx = RunCtx::new(&NoBlobs).with_workspace(project.path().to_path_buf());
        let output = adapter
            .run(IMAGE_GENERATE, serde_json::json!({"prompt": "safe"}), &ctx)
            .await
            .unwrap();
        assert_eq!(output.output["path"], "generated-image-2.png");
    }

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

    /// ADR 0034: a turn that saves over a file says which version before it kept; putting it
    /// back keeps the file as it was, so that can be put back too.
    #[tokio::test]
    async fn a_file_put_back_can_be_put_back_again() {
        let dir = tempfile::tempdir().unwrap();
        let write = |id: &str, text: &str| Completion {
            content: String::new(),
            tool_calls: vec![ToolCall {
                id: id.into(),
                name: "files.write".into(),
                arguments: serde_json::json!({ "path": "notes/plan.md", "text": text }),
            }],
            usage: Usage::measured(50, 5),
        };
        let done = Completion {
            content: "Saved.".into(),
            tool_calls: vec![],
            usage: Usage::measured(60, 6),
        };
        let script = vec![write("c1", "first"), write("c2", "second"), done];
        let provider: Arc<dyn ark_model::Provider> = Arc::new(MockProvider::new("mock", script));
        let ark = Ark::builder()
            .store_root(dir.path())
            .files(true)
            .allow_mutate(true)
            .provider(provider)
            .planner(false)
            .critic(false)
            .build()
            .unwrap();
        let project = ark.create_project("t", "").unwrap();
        let mut kept = Vec::new();
        ark.ask_with(&project.id, "write the plan twice", |e| {
            if let HarnessEvent::ToolResult { before, .. } = e {
                kept.push(before);
            }
        })
        .await
        .unwrap();
        // The first save made the file: nothing before it. The second kept "first".
        assert_eq!(kept.len(), 2);
        assert!(kept[0].is_empty());
        assert_eq!(kept[1].len(), 1);
        assert_eq!(kept[1][0].path, "notes/plan.md");

        let engine = ark.engine();
        let file = engine
            .workspace_dir(&project.id)
            .unwrap()
            .join("notes/plan.md");
        let text = || std::fs::read_to_string(&file).unwrap();
        assert_eq!(text().trim(), "second");
        let back = engine
            .restore_file(&project.id, &ArtifactId(kept[1][0].artifact.clone()))
            .unwrap();
        assert_eq!(back.path, "notes/plan.md");
        assert_eq!(text().trim(), "first");
        engine
            .restore_file(&project.id, &back.undo.unwrap())
            .unwrap();
        assert_eq!(text().trim(), "second");

        // A version that names no file of the project is refused, and so is a path that leads
        // out of the folder.
        let note = engine
            .ingest_bytes(
                &project.id,
                "n",
                ArtifactKind::Text,
                b"x",
                Provenance::Ingested { path: "n".into() },
            )
            .unwrap();
        assert!(engine.restore_file(&project.id, &note.id).is_err());
        let mut out = note.clone();
        out.meta = serde_json::json!({ "path": "../outside.md", "before": true });
        engine.store.put_artifact(&project.id, &out).unwrap();
        assert!(engine.restore_file(&project.id, &out.id).is_err());
        assert!(!dir.path().join("workspaces/outside.md").exists());
    }

    struct OneHit;
    #[async_trait::async_trait]
    impl ark_tools::web::SearchBackend for OneHit {
        fn name(&self) -> String {
            "fixed".into()
        }
        async fn search(
            &self,
            query: &str,
            _limit: usize,
        ) -> ark_tools::Result<Vec<ark_tools::web::Hit>> {
            Ok(vec![ark_tools::web::Hit {
                title: format!("All about {query}"),
                url: "https://tea.example/guide".into(),
                snippet: "Green tea at 80 C.".into(),
            }])
        }
    }

    #[tokio::test]
    async fn a_conversation_turn_offers_the_web_and_reads_its_card() {
        let dir = tempfile::tempdir().unwrap();
        let search = ToolCall {
            id: "c1".into(),
            name: "web.search".into(),
            arguments: serde_json::json!({ "query": "tea" }),
        };
        let mock = Arc::new(MockProvider::new(
            "mock",
            vec![
                Completion {
                    content: String::new(),
                    tool_calls: vec![search],
                    usage: Usage::measured(50, 5),
                },
                Completion {
                    content: "80 C, says tea.example [inv:1].".into(),
                    tool_calls: vec![],
                    usage: Usage::measured(60, 6),
                },
            ],
        ));
        let provider: Arc<dyn ark_model::Provider> = mock.clone();
        let web = ark_tools::web::WebAdapter::new().backends(vec![Arc::new(OneHit)]);
        let ark = Ark::builder()
            .store_root(dir.path())
            .provider(provider)
            .web_with(web)
            .planner(false)
            .critic(false)
            .build()
            .unwrap();
        let project = ark.create_project("t", "").unwrap();
        let mut session = ark.start_session(&project.id).unwrap();
        ark.ask_session(
            &mut session,
            "how hot for green tea?",
            TurnOptions::default(),
            |_| {},
        )
        .await
        .unwrap();
        let seen = mock.recorded();
        let offered: Vec<&str> = seen[0].tools.iter().map(|t| t.name.as_str()).collect();
        assert!(
            offered.contains(&"web.search") && offered.contains(&"web.fetch"),
            "{offered:?}"
        );
        let card = seen[1]
            .messages
            .iter()
            .find(|m| m.content.contains("result(s) for"))
            .map(|m| m.content.clone())
            .unwrap_or_default();
        assert!(
            card.contains("1. All about tea https://tea.example/guide\n   Green tea at 80 C."),
            "{card}"
        );
    }

    #[tokio::test]
    async fn without_the_web_no_turn_offers_it() {
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
        let project = ark.create_project("t", "").unwrap();
        let mut session = ark.start_session(&project.id).unwrap();
        ark.ask_session(&mut session, "hi", TurnOptions::default(), |_| {})
            .await
            .unwrap();
        assert!(!mock.recorded()[0]
            .tools
            .iter()
            .any(|t| t.name.starts_with("web.")));
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

    /// A chat turn carries the project folder's AGENTS.md as the project's rules (ADR 0051), and
    /// an app that turns it off sends none.
    #[tokio::test]
    async fn a_turn_follows_the_project_folders_agents_file() {
        for on in [true, false] {
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
            let mut b = Ark::builder()
                .store_root(dir.path())
                .provider(provider)
                .planner(false)
                .critic(false);
            if !on {
                b = b.project_instructions(Vec::<String>::new());
            }
            let ark = b.build().unwrap();
            let project = ark.create_project("t", "").unwrap();
            let folder = ark.engine.workspace_dir(&project.id).unwrap();
            std::fs::write(
                folder.join("AGENTS.md"),
                "# Book\n\nChapters are `chNN.md`.\n",
            )
            .unwrap();
            let mut session = ark.start_session(&project.id).unwrap();
            let mut rules = Vec::new();
            ark.ask_session(&mut session, "hi", TurnOptions::default(), |e| {
                if let HarnessEvent::Rules { applied, .. } = e {
                    rules = applied;
                }
            })
            .await
            .unwrap();
            let sent: String = mock.recorded()[0]
                .messages
                .iter()
                .map(|m| m.content.clone())
                .collect();
            assert_eq!(
                sent.contains("From the project's own file AGENTS.md"),
                on,
                "on: {on}"
            );
            assert_eq!(sent.contains("Chapters are `chNN.md`."), on, "on: {on}");
            assert_eq!(
                rules.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
                if on { vec!["Book"] } else { vec![] }
            );
            let report = ark.context_preview(&session).await.unwrap();
            assert_eq!(report.rules > 0, on, "the context report counts it");
        }
    }

    /// Run code's Python comes with the data-analysis skill; a skill of the same name in the
    /// store replaces it, and a runtime without skills leaves none behind.
    #[test]
    fn run_code_comes_with_its_skill() {
        if ark_exec::Sandbox::native().is_none() {
            eprintln!("skipped: no sandbox here, so no Run code");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let provider: Arc<dyn ark_model::Provider> = Arc::new(MockProvider::new("mock", vec![]));
        let build = |code: Option<ark_exec::Exec>| {
            let b = Ark::builder()
                .store_root(dir.path())
                .provider(provider.clone())
                .planner(false)
                .critic(false);
            match code {
                Some(e) => b.code_with(e),
                None => b.code(true),
            }
            .build()
            .unwrap()
        };
        let ark = build(None);
        let project = ark.create_project("p", "").unwrap();
        let found = ark.skills(&project.id);
        let s = found.iter().find(|s| s.name == "data-analysis").unwrap();
        assert_eq!(s.scope, ark_runtime::SkillScope::Builtin);
        assert!(std::fs::read_to_string(s.dir.join("SKILL.md"))
            .unwrap()
            .contains("Join with care"));

        let mine = dir.path().join("skills").join("data-analysis");
        std::fs::create_dir_all(&mine).unwrap();
        std::fs::write(mine.join("SKILL.md"), "---\ndescription: mine\n---\nx").unwrap();
        let found = ark.skills(&project.id);
        let s = found.iter().find(|s| s.name == "data-analysis").unwrap();
        assert_eq!(s.description, "mine");
        std::fs::remove_dir_all(&mine).unwrap();
        drop(ark);

        // Another app's Run code, with no Python: the skill goes.
        let exec = ark_exec::Exec::new(dir.path().join("runs"), vec![]).unwrap();
        let ark = build(Some(exec));
        assert!(!ark
            .skills(&project.id)
            .iter()
            .any(|s| s.name == "data-analysis"));
    }
}
