//! The research harness: an evidence-driven, budget-managed replacement for the plain ReAct
//! loop. It classifies the question, optionally drafts a short plan, then runs a step loop
//! that discovers tools on demand, distills every result into an evidence card (never dumping
//! raw output), keeps a bounded working context, and reasons over a persistent working state
//! instead of replaying chat history.
//!
//! It preserves the invariant: every tool call goes through [`Engine::run_action`] and becomes
//! an invocation; meta-tools (discovery, retrieval, state) are handled here and never touch the
//! approval gate. `Mutate` actions still stop for a human.
//!
//! Everything domain-specific — the persona, the capability taxonomy, the result distillers,
//! the external-knowledge source, the "is this an investigation?" markers — comes from the
//! [`AgentProfile`], so the same loop drives a security agent, a writing agent, or a data
//! agent unchanged.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::Arc;

use ark_core::{
    ArtifactId, Author, Claim, ClaimType, EvidenceCard, ExecutionMode, Focus, Interesting,
    Invocation, InvocationId, InvocationStatus, Note, NoteKind, ProjectId, ResearchState, Reuse,
    RiskTier, WorkspaceContext,
};
use ark_model::{estimate_tokens, Completion, Message, ToolDef, Usage};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::approval::{preview_call, ApprovalDecision, ApprovalGate, ApprovalRequest};
use crate::budget::WorkingSet;
use crate::cancel::{self, CancelToken};
use crate::catalog::{self, MetaToolConfig};
use crate::context_report::ContextReport;
use crate::distill::{call_digest, distill};
use crate::engine::Engine;
use crate::error::AgentError;
use crate::metrics::{role_name, Metrics};
use crate::profile::AgentProfile;
use crate::router::{ModelRouter, Role};
use crate::rules::RuleSelection;
use crate::steer::{self, Steering};

/// A file a call changed or removed, and the artifact that holds it as it was before the call
/// (ADR 0034). `Engine::restore_file` puts it back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileBefore {
    pub path: String,
    pub artifact: String,
}

/// Events emitted as the harness works. They describe *what happened*, never how to show it:
/// a terminal, a GUI, a JSON log and a test all consume the same stream. Every event is owned
/// and serializable so it can cross a channel or a process boundary.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HarnessEvent {
    Classified {
        trivial: bool,
    },
    Plan {
        steps: Vec<String>,
    },
    /// A reasoning step began (1-based).
    Step {
        index: usize,
    },
    /// The request about to be sent, broken down by source.
    Context {
        report: ContextReport,
    },
    /// Incremental model text. In a step that ends with tool calls this is narration; in the
    /// final step it is the answer arriving.
    TextDelta {
        text: String,
    },
    /// The last `text` streamed as [`TextDelta`](HarnessEvent::TextDelta)s belongs to a model
    /// call that then failed and is tried again ([`Retry`](HarnessEvent::Retry) follows): it is
    /// not part of the conversation. A front-end takes it back where it can; the retry streams
    /// its own text (ADR 0049).
    TextWithdrawn {
        text: String,
    },
    /// A model call finished: which role, which model, what it cost.
    ModelCall {
        role: String,
        model: String,
        usage: Usage,
        elapsed_ms: u64,
        /// Milliseconds from sending the request to the first streamed text, when any text
        /// arrived (a reply that is only tool calls has none).
        #[serde(default)]
        first_token_ms: Option<u64>,
        /// Provider-level retries this call needed before it succeeded.
        #[serde(default)]
        retries: u32,
    },
    ToolCall {
        id: String,
        action: String,
        input: serde_json::Value,
    },
    ToolResult {
        id: String,
        action: String,
        ok: bool,
        summary: String,
        invocation: String,
        duration_ms: Option<i64>,
        error: Option<String>,
        /// Estimated tokens this result adds to the model's context: the distilled card, not
        /// the raw output (which stays in the store).
        #[serde(default)]
        context_tokens: u32,
        /// Estimated tokens of the raw output kept in the store. `raw_tokens` against
        /// `context_tokens` is what distillation saved.
        #[serde(default)]
        raw_tokens: u32,
        /// Files of the project folder the call wrote (relative paths), for a front-end to
        /// open. Each is also stored as an artifact, a snapshot of what was written.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<String>,
        /// Files the call changed or removed whose version before it is kept, for a front-end
        /// to offer to put back (ADR 0034).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        before: Vec<FileBefore>,
    },
    Duplicate {
        action: String,
        reused: String,
    },
    Retrieval {
        what: String,
    },
    /// A state-changing action was refused because no approver is configured.
    NeedsApproval {
        action: String,
    },
    /// The approval gate is asking a human.
    ApprovalRequested {
        request: ApprovalRequest,
    },
    ApprovalResolved {
        action: String,
        class: String,
        decision: ApprovalDecision,
    },
    /// A capability-acquisition meta-tool ran (find/propose/acquire). `action` is which one;
    /// `detail` is the agent-facing result.
    Capability {
        action: String,
        detail: String,
    },
    StateUpdate {
        kind: String,
        text: String,
    },
    Note {
        kind: String,
        body: String,
    },
    Present {
        directive: ark_core::PresentDirective,
    },
    SelfCheck {
        passed: bool,
        detail: String,
    },
    /// A message the user sent while the turn was running reached the model.
    Steered {
        text: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        context: String,
    },
    /// A model call failed with a temporary error and is tried again after `delay_ms`. Text
    /// the failed attempt had streamed was withdrawn just before
    /// ([`TextWithdrawn`](HarnessEvent::TextWithdrawn)). No tool runs twice: tools run only
    /// after a model call succeeds.
    ///
    /// `repair`: the model's reply could not be used, and the model was told why and asked
    /// for it again (ADR 0050). Nothing waits and nothing failed on the way to it.
    Retry {
        role: String,
        attempt: u32,
        delay_ms: u64,
        error: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        repair: bool,
    },
    /// Several steps in a row produced nothing new. `stopped` is false when the model was
    /// nudged to change course, true when the loop ended and a final answer was forced.
    Stall {
        idle_steps: u32,
        stopped: bool,
    },
    /// Bound rules were selected for this turn: `applied` reached the model, `held_back`
    /// more are reachable with `rules.lookup`.
    Rules {
        applied: Vec<ark_core::RuleRef>,
        held_back: usize,
    },
    /// The turn was cancelled; no answer follows.
    Cancelled,
    /// The process is stopping ([`Engine::stop_soon`]) and the turn stopped at a safe point; no
    /// answer follows. Unlike [`Cancelled`](HarnessEvent::Cancelled), nobody asked it to stop:
    /// the work continues later.
    Interrupted,
    /// The final answer. `streamed` is true when its text already arrived as `TextDelta`s.
    Answer {
        text: String,
        streamed: bool,
    },
}

/// Items per working-state section the model sees on each request. Older items stay stored
/// and are counted as `(+n more)`.
pub const STATE_DIGEST_ITEMS: usize = 6;

pub struct HarnessOutcome {
    pub answer: String,
    pub cited: Vec<InvocationId>,
    pub steps: usize,
    pub trivial: bool,
    pub metrics: Metrics,
    pub state: ResearchState,
    /// True when the run stopped on a [`CancelToken`]; `answer` is then empty.
    pub cancelled: bool,
    /// It stopped because the process is stopping, not because it was cancelled
    /// ([`HarnessEvent::Interrupted`]): the work continues later. Implies `cancelled`.
    pub interrupted: bool,
    /// Messages the user sent while the turn ran that reached the model, in order.
    pub steered: Vec<String>,
    /// The step limit or the stall guard ended the work before the model answered, and the
    /// answer was asked for (`force_answer`): the work may not be done. The next turn
    /// continues its goal (ADR 0050).
    pub ran_out: bool,
}

impl HarnessOutcome {
    /// Whether the answer is backed by at least one cited piece of evidence. A convenience
    /// for consumers that gate on grounding (a CLI badge, an eval scorer).
    pub fn grounded(&self) -> bool {
        !self.cited.is_empty()
    }
}

pub struct Harness {
    router: ModelRouter,
    allow_mutate: bool,
    max_steps: usize,
    /// What this turn works on when it continues an unfinished one (ADR 0050). `None`: the
    /// question is the goal.
    goal: Option<crate::session::Goal>,
    /// The request budget the app or `ARK_CTX_BUDGET` set. `None`: sized from the reasoning
    /// model's window ([`crate::budget::budget_for`], ADR 0050).
    budget: Option<u32>,
    /// Actions to load before the first step because they match the task's words (see
    /// [`catalog::matching_actions`]); 0 leaves discovery to the model. Default 2.
    preload: usize,
    /// Name the loaded actions in the first message (default on; `ARK_READY_LIST=0` turns it
    /// off). Measured live on qwen3:4b (bench/RESULTS.md, P2).
    ready_list: bool,
    planner: bool,
    critic: bool,
    focus: Option<Focus>,
    /// Images the person attached to this turn's message, shown to a model that reads them.
    pictures: Vec<ark_model::Picture>,
    conversation: Option<String>,
    approvals: Option<Arc<ApprovalGate>>,
    guard: Option<Arc<dyn crate::guard::CallGuard>>,
    cancel: Option<CancelToken>,
    /// The process is stopping ([`Engine::stop_soon`]): like a cancel, except that a batch with
    /// a state-changing call runs to its end.
    stopping: Option<CancelToken>,
    steer: Option<Steering>,
    retry: ark_model::RetryPolicy,
    presentation: bool,
    rules: Option<RuleSelection>,
    skills: Option<String>,
    /// State-changing calls that already succeeded in an earlier run of the same work, which
    /// stopped before it finished (see [`Harness::done_before`]).
    done_before: Vec<Invocation>,
    /// State-changing calls an earlier run of the same work had under way when it stopped
    /// (see [`Harness::cut_short`]).
    cut_short: Vec<Invocation>,
    execution_mode: ExecutionMode,
}

/// The pieces of the first user message, kept separate so the context report can attribute
/// tokens to each.
struct FirstTurn {
    /// The project's name, target and newest artifacts. Here, not in the system prompt, so the
    /// system prompt stays identical across projects and a vendor's prompt cache serves it.
    project: String,
    memory: Option<String>,
    workspace: Option<String>,
    conversation: Option<String>,
    rules: Option<String>,
    /// The skills catalog ([`crate::skills::catalog`]).
    skills: Option<String>,
    /// What the agent can ask to be added ([`crate::AcquisitionProvider::offer`]).
    can_add: Option<String>,
    /// The actions already loaded, one line each, so a model calls them instead of looking
    /// for others in the index.
    ready: Option<String>,
    task: String,
}

impl FirstTurn {
    fn render(&self) -> String {
        let mut s = String::new();
        if !self.project.is_empty() {
            let _ = write!(s, "== PROJECT ==\n{}\n\n", self.project);
        }
        if let Some(w) = &self.workspace {
            let _ = write!(s, "== WORKSPACE CONTEXT ==\n{w}\n\n");
        }
        if let Some(c) = &self.conversation {
            let _ = write!(s, "== EARLIER IN THIS CONVERSATION ==\n{c}\n\n");
        }
        if let Some(r) = &self.rules {
            let _ = write!(s, "== RULES ==\n{r}\n\n");
        }
        if let Some(memory) = &self.memory {
            s.push_str(memory);
        }
        if let Some(k) = &self.skills {
            let _ = write!(s, "== SKILLS ==\n{k}\n\n");
        }
        if let Some(c) = &self.can_add {
            let _ = write!(s, "== CAN BE ADDED ==\n{c}\n\n");
        }
        if let Some(r) = &self.ready {
            let _ = write!(s, "== READY TO CALL ==\n{r}\n\n");
        }
        if s.is_empty() {
            return self.task.clone();
        }
        let _ = write!(s, "== TASK ==\n{}", self.task);
        s
    }
}

impl Harness {
    pub fn new(router: ModelRouter) -> Self {
        Harness {
            router,
            allow_mutate: false,
            max_steps: 16,
            goal: None,
            budget: crate::budget::budget_from_env(),
            // 2: measured live on qwen3:4b and Claude Haiku (bench/RESULTS.md, P1).
            preload: std::env::var("ARK_PRELOAD_ACTIONS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(2),
            ready_list: std::env::var("ARK_READY_LIST").map_or(true, |v| v != "0"),
            // Off: on qwen3:4b the plan cost calls and success on nothing it helped
            // (bench/RESULTS.md, P2 overhead).
            planner: false,
            critic: true,
            focus: None,
            pictures: Vec::new(),
            conversation: None,
            approvals: None,
            guard: None,
            cancel: None,
            stopping: None,
            steer: None,
            retry: ark_model::RetryPolicy::default(),
            presentation: false,
            rules: None,
            skills: None,
            done_before: Vec::new(),
            cut_short: Vec::new(),
            execution_mode: ExecutionMode::Manual,
        }
    }

    /// The bound rules chosen for this turn (see [`crate::rules::select`]). Their block rides
    /// in the first message under the budget and is reported as `rules` in each
    /// [`ContextReport`].
    pub fn rules(mut self, selection: Option<RuleSelection>) -> Self {
        self.rules = selection.filter(|s| !s.is_empty());
        self
    }

    /// The skills this turn may read: the catalog [`crate::skills::catalog`] wrote, one line
    /// per skill. It rides in the first message after the rules and is reported as `skills`;
    /// the runtime reads a skill's instructions only through `skills.read`.
    pub fn skills(mut self, catalog: Option<String>) -> Self {
        self.skills = catalog.filter(|t| !t.trim().is_empty());
        self
    }

    /// Calls an earlier run of this work made before it stopped (a restart, a suspend). A
    /// state-changing call identical to one of them that succeeded is not run, or asked about,
    /// again: the model gets that earlier result. A change must not happen twice because the
    /// work was interrupted.
    pub fn done_before(mut self, invocations: Vec<Invocation>) -> Self {
        self.done_before = invocations
            .into_iter()
            .filter(|i| i.risk == RiskTier::Mutate && i.status == InvocationStatus::Ok)
            .collect();
        self
    }

    /// Changes an earlier run of this work had under way when it stopped: whether they happened
    /// is unknown. An identical call is asked of the person, whatever was granted, saying so;
    /// with no one to ask it is not run, and the model is told why.
    pub fn cut_short(mut self, invocations: Vec<Invocation>) -> Self {
        self.cut_short = invocations
            .into_iter()
            .filter(|i| i.risk == RiskTier::Mutate)
            .collect();
        self
    }

    /// Offer the `present` tool: the front-end can show things beside the conversation (a
    /// workspace window). Off by default; a terminal or a headless run cannot.
    pub fn presentation(mut self, yes: bool) -> Self {
        self.presentation = yes;
        self
    }

    /// How failed model calls are retried (default: `ARK_MODEL_RETRIES`, 2).
    pub fn retry_policy(mut self, policy: ark_model::RetryPolicy) -> Self {
        self.retry = policy;
        self
    }

    /// Tell the harness what the user is currently looking at, so on-screen references
    /// resolve against it.
    /// The task with the file the person has open, if any: what they look at is part of what
    /// they ask about, so it counts toward which actions are loaded first.
    fn with_focus(&self, task: &str) -> String {
        match self
            .focus
            .as_ref()
            .and_then(|f| f.surface.as_ref())
            .and_then(|s| s.path.as_deref())
        {
            Some(path) => format!("{task} {path}"),
            None => task.to_string(),
        }
    }

    /// Images the person attached to the message. They ride on the task frame; a model that
    /// does not read images is told so in words.
    pub fn pictures(mut self, pictures: Vec<ark_model::Picture>) -> Self {
        self.pictures = pictures;
        self
    }

    /// The task frame as a message: with the attached images when the model reads them.
    fn task_message(&self, mut text: String) -> Message {
        if self.pictures.is_empty() {
            return Message::user(text);
        }
        if self.router.provider(Role::Reason).reads_pictures() {
            return Message::user(text).with_pictures(self.pictures.clone());
        }
        text.push_str("\n(an image was attached, but this model is not sent images)");
        Message::user(text)
    }

    fn picture_tokens(&self) -> u32 {
        if self.router.provider(Role::Reason).reads_pictures() {
            self.pictures.len() as u32 * ark_model::PICTURE_TOKENS
        } else {
            0
        }
    }

    pub fn focus(mut self, focus: Option<Focus>) -> Self {
        self.focus = focus.filter(|f| !f.is_empty());
        self
    }
    pub fn allow_mutate(mut self, yes: bool) -> Self {
        self.allow_mutate = yes;
        self
    }
    pub fn execution_mode(mut self, mode: ExecutionMode) -> Self {
        self.execution_mode = mode;
        self
    }
    pub fn max_steps(mut self, n: usize) -> Self {
        self.max_steps = n;
        self
    }
    /// The goal this turn works on when it continues an unfinished one: the first request of
    /// the chain and what the person said since, the question last ([`Session::goal_for`]).
    /// It is the task the model gets and stays in every request; the research state's goal,
    /// rule and tool selection and the trivial class read it. `ask`'s question stays the
    /// person's newest message.
    ///
    /// [`Session::goal_for`]: crate::session::Session::goal_for
    pub fn goal(mut self, goal: Option<crate::session::Goal>) -> Self {
        self.goal = goal;
        self
    }

    /// Set the request budget. Without it the budget is sized from the reasoning model's
    /// window ([`crate::budget::budget_for`]).
    pub fn budget(mut self, tokens: u32) -> Self {
        self.budget = Some(tokens);
        self
    }

    /// Like [`budget`](Harness::budget), keeping the default when `None`.
    pub fn budget_or_auto(mut self, tokens: Option<u32>) -> Self {
        if tokens.is_some() {
            self.budget = tokens;
        }
        self
    }

    /// The request budget this harness uses: the one set, else sized from the window of the
    /// model in its reasoning slot.
    pub fn request_budget(&self) -> u32 {
        self.budget.unwrap_or_else(|| {
            crate::budget::budget_for(self.router.provider(Role::Reason).context_window())
        })
    }
    /// Load up to `n` actions that match the task's words before the first step.
    pub fn preload(mut self, n: usize) -> Self {
        self.preload = n;
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
    /// Bounded carry-over from earlier turns (see [`crate::Session`]). It rides in the first
    /// message under the budget; it is never the raw transcript.
    pub fn conversation(mut self, digest: Option<String>) -> Self {
        self.conversation = digest.filter(|d| !d.trim().is_empty());
        self
    }
    /// Ask a human before `Mutate` actions instead of refusing them.
    pub fn approvals(mut self, gate: Option<Arc<ApprovalGate>>) -> Self {
        self.approvals = gate;
        self
    }
    pub fn guard(mut self, guard: Arc<dyn crate::guard::CallGuard>) -> Self {
        self.guard = Some(guard);
        self
    }

    /// Stop cooperatively when this token is cancelled.
    pub fn cancel(mut self, token: Option<CancelToken>) -> Self {
        self.cancel = token;
        self
    }
    /// Stop at the next safe point when the process is stopping: give it
    /// [`Engine::stopping`]. The turn ends cancelled; a batch of calls that changes something
    /// finishes first, so no change is cut in half.
    pub fn stopping(mut self, token: Option<CancelToken>) -> Self {
        self.stopping = token;
        self
    }
    /// Take messages the user sends while the turn runs (see [`Steering`]).
    pub fn steering(mut self, steer: Option<Steering>) -> Self {
        self.steer = steer;
        self
    }
    pub fn describe(&self) -> String {
        self.router.describe()
    }

    fn goal_query(&self, question: &str) -> String {
        match &self.goal {
            Some(g) => format!("{}\n{question}", g.text()),
            None => question.to_string(),
        }
    }

    fn rules_text(&self) -> Option<String> {
        self.rules
            .as_ref()
            .map(|r| r.text.clone())
            .filter(|t| !t.is_empty())
    }
    /// Run a task grounded in a project, shaped by `profile`.
    pub async fn ask(
        &self,
        engine: &Engine,
        profile: &AgentProfile,
        project: &ProjectId,
        question: &str,
        on_event: impl FnMut(HarnessEvent),
    ) -> Result<HarnessOutcome, AgentError> {
        let outcome = self
            .ask_inner(engine, profile, project, question, on_event)
            .await;
        if let Some(cancel) = &self.cancel {
            cancel
                .wait_for_commit()
                .await
                .map_err(std::io::Error::other)?;
        }
        outcome
    }

    async fn ask_inner(
        &self,
        engine: &Engine,
        profile: &AgentProfile,
        project: &ProjectId,
        question: &str,
        mut on_event: impl FnMut(HarnessEvent),
    ) -> Result<HarnessOutcome, AgentError> {
        let bound = self
            .guard
            .as_ref()
            .map(|guard| engine.guarded(guard.clone()));
        let engine = bound.as_ref().unwrap_or(engine);
        let guard = engine.call_guard();
        // Counted, so a stopping process waits for this run to reach a safe point.
        let _run = engine.run_started();
        let mut metrics = Metrics::start();
        let turn_started = time::OffsetDateTime::now_utc();

        // Resume: load accumulated state; set the current objective without discarding prior
        // findings/interesting/hypotheses.
        let mut state = engine
            .store
            .get_research_state(project)?
            .unwrap_or_default();
        // A turn that continues an unfinished one works on that one's goal, not on its own
        // words ("continue"), for everything chosen from the task (ADR 0050).
        let goal_text = self.goal_query(question);
        state.goal = self
            .goal
            .as_ref()
            .map(|g| g.request.clone())
            .filter(|r| !r.trim().is_empty())
            .unwrap_or_else(|| question.to_string());
        state.touch();
        engine.store.put_research_state(project, &state)?;
        let budget = self.request_budget();

        let trivial = classify_trivial(&goal_text, &profile.investigation_markers);
        on_event(HarnessEvent::Classified { trivial });
        if let Some(r) = &self.rules {
            on_event(HarnessEvent::Rules {
                applied: r.applied.clone(),
                held_back: r.held_back,
            });
        }

        let mut cancelled = false;

        // Plan (non-trivial only, when enabled).
        if !trivial && self.planner {
            match self
                .cancellable(self.draft_plan(profile, question, &mut metrics, &mut on_event))
                .await
            {
                None => cancelled = true,
                Some(Some(steps)) if !steps.is_empty() => {
                    on_event(HarnessEvent::Plan {
                        steps: steps.clone(),
                    });
                    state.next_actions = steps;
                    engine.store.put_research_state(project, &state)?;
                }
                Some(_) => {}
            }
        }

        let sys = self.system_prompt(engine, profile).await;
        let system = sys.render();
        let meta_cfg = MetaToolConfig {
            knowledge: profile.knowledge.is_some(),
            acquisition: self.execution_mode != ExecutionMode::Plan && acquisition_on(profile),
            present: self.presentation,
        };

        let wsctx = self.build_workspace_context(engine, project)?;

        // Loaded action schemas: the profile's core set (present in the registry), then each
        // on-demand load appended. The order never changes within a turn, so loading one more
        // action keeps every earlier tool definition in the cacheable prefix.
        let mut loaded: Vec<String> = core_loaded(engine, profile)
            .into_iter()
            .filter(|name| self.action_available(engine, name))
            .collect();
        let core = loaded.len();
        for name in
            catalog::matching_actions(&engine.registry, &self.with_focus(&goal_text), self.preload)
        {
            if self.action_available(engine, &name) && !loaded.contains(&name) {
                loaded.push(name);
            }
        }
        // A vendor that caches its system prompt only whole, with the tools in it, gets the
        // task's preloaded actions after its first call. The vendor session then opens with the
        // same instructions every turn. They stay named in READY TO CALL, and a direct call
        // works on the first step since calls are looked up in the registry.
        let mut held: Vec<String> = if self
            .router
            .provider(Role::Reason)
            .caches_system_prompt_whole()
        {
            loaded.split_off(core)
        } else {
            Vec::new()
        };

        let mut first = FirstTurn {
            project: self.project_block(engine, project)?,
            memory: crate::memory::recall(&engine.store, project, &goal_text, budget)?,
            workspace: wsctx.digest(6),
            conversation: self.conversation.clone(),
            rules: self.rules_text(),
            skills: self.skills.clone(),
            can_add: (self.execution_mode != ExecutionMode::Plan)
                .then(|| can_add(profile))
                .flatten(),
            ready: self.ready_block(engine, &[loaded.as_slice(), held.as_slice()].concat()),
            task: self.task_text(question, budget),
        };
        let first_text = first.render();
        // The first message is the task frame: pinned, so no number of steps drops the task
        // (ADR 0050).
        let mut ws = WorkingSet::new(budget);
        ws.pin(self.task_message(first_text));
        // Tokens of retrieval results (evidence lookups, knowledge search) per round, aligned
        // with the working set's rounds, so the context report can show them apart.
        let mut retrieved: Vec<u32> = Vec::new();

        let mut cited: Vec<InvocationId> = Vec::new();
        let mut answer = String::new();
        let mut streamed = false;
        let mut steps_taken = 0usize;

        // Stall guard: a weak model can churn on meta-tools or errored calls for many steps
        // without gathering evidence. Count consecutive no-progress steps; steer once, then
        // force a final answer instead of burning the whole budget.
        const STALL_STEER: u32 = 3;
        const STALL_MAX: u32 = 5;
        let mut stall: u32 = 0;
        // Identical calls that failed again this turn: cards, but not progress.
        let mut repeated_failures: u32 = 0;
        // Runtime-handled calls (retrievals, state edits, notes) seen this turn. Only a new one
        // counts as progress: repeating the same search or the same hypothesis is not work.
        let mut meta_seen: HashSet<String> = HashSet::new();
        // State-changing calls run this task, by digest (see `MAX_IDENTICAL_CHANGES`).
        let mut changes_run: HashMap<String, u32> = HashMap::new();
        // Failed read and observation calls run this turn, by digest. One retry may recover a
        // transient failure. More identical attempts are kept from spending time on the same
        // input until a successful change may have repaired what they read.
        let mut failures_run: HashMap<String, u32> = HashMap::new();
        // Changes an earlier run of this work already made, by digest (`done_before`).
        let done_before: HashMap<String, &Invocation> = self
            .done_before
            .iter()
            .map(|i| {
                let canonical = engine.canonical_input(project, i.input.clone());
                (call_digest(&i.action, &canonical), i)
            })
            .collect();
        // Changes an earlier run had under way when it stopped, by digest (`cut_short`). Each is
        // checked once: after the person or the model decided, the call runs as any other.
        let mut cut_short: HashSet<String> = self
            .cut_short
            .iter()
            .map(|i| call_digest(&i.action, &engine.canonical_input(project, i.input.clone())))
            .collect();
        // When an action that changes state last ran this turn. Reads before it are not
        // reused after it.
        let mut changed_at = turn_started;
        let mut fresh_meta: u32 = 0;
        let mut corrected_text_call = false;
        // Replies the model was asked to send again because they could not be used.
        let mut repairs: u32 = 0;
        // The model gave its own answer (possibly empty), not the step limit or a stall.
        let mut answered = false;
        let mut repairs_in_row: u32 = 0;
        // Files of the project folder this turn's calls wrote, and whether the model was told
        // once that its answer names a saved file none of them wrote.
        let mut written: Vec<String> = Vec::new();
        let mut corrected_claim = false;
        // Whether the model was told once that its answer announced work it had not done.
        let mut corrected_announce = false;
        // The answer the model last wrote beside a check of its own changes (see `rechecks`).
        let mut checked: Option<String> = None;
        let mut steered: Vec<String> = Vec::new();
        // Tools an acquisition added this turn, and the turn's engine once it has them.
        let outer = engine;
        let mut added: Vec<Arc<dyn ark_tools::Adapter>> = Vec::new();
        let mut grown: Option<Engine> = None;

        for _ in 0..self.max_steps {
            if !added.is_empty() {
                let cur = grown.as_ref().unwrap_or(outer);
                let mut registry = cur.registry.clone();
                for a in added.drain(..) {
                    for d in a.actions() {
                        if !loaded.contains(&d.name) {
                            loaded.push(d.name);
                        }
                    }
                    registry.register(a);
                }
                let next = cur.scoped(registry);
                grown = Some(next);
            }
            let engine: &Engine = grown.as_ref().unwrap_or(outer);
            if cancelled || self.is_cancelled() {
                cancelled = true;
                break;
            }
            if steps_taken == 1 {
                for name in held.drain(..) {
                    if !loaded.contains(&name) {
                        loaded.push(name);
                    }
                }
            }
            steps_taken += 1;
            metrics.record_step();
            on_event(HarnessEvent::Step { index: steps_taken });
            let progress_before =
                metrics.cards_created - repeated_failures + fresh_meta + loaded.len() as u32;

            first.memory = crate::memory::recall(&engine.store, project, &goal_text, budget)?;
            let refreshed = first.render();
            let first_tokens = estimate_tokens(&refreshed) + 4 + self.picture_tokens();
            ws.pin(self.task_message(refreshed));
            let state_digest = state.digest(STATE_DIGEST_ITEMS);
            let tail = state_message_with(&state_digest, &steered);
            let (mut messages, est) = ws.assemble_with(&system, WorkingSet::message_tokens(&tail));
            let tail_tokens = WorkingSet::message_tokens(&tail);
            messages.push(tail);
            metrics.peak_prompt_tokens = metrics.peak_prompt_tokens.max(est);

            let tools = self.tool_set(engine, &loaded, meta_cfg);
            let mut report = self.report(
                profile,
                &sys,
                &state_digest,
                &first,
                first_tokens,
                &ws,
                &retrieved,
                est,
                &tools,
                loaded.len(),
                steps_taken,
            );
            // What the person said while it ran rides in the working state, not in tool results.
            let said = tail_tokens.saturating_sub(report.working_state);
            report.working_state += said;
            report.tool_rounds = report.tool_rounds.saturating_sub(said);
            on_event(HarnessEvent::Context { report });

            let mut unusable = String::new();
            let called = self
                .call_model(
                    Role::Reason,
                    &system,
                    &messages,
                    &tools,
                    true,
                    &mut metrics,
                    &mut on_event,
                    Some(&mut unusable),
                    Some(&crate::memory::Snapshot {
                        store: &engine.store,
                        project,
                        task: &goal_text,
                        budget,
                        selected: &first.memory,
                    }),
                )
                .await;
            let Some(mut completion) = (match called {
                Ok(c) => c,
                // The model's reply could not be used: tell it why and let it send it again,
                // a bounded number of times (ADR 0050). Nothing ran.
                Err(AgentError::UnusableReply { detail, .. })
                    if repairs_in_row < MAX_REPAIRS_IN_ROW && repairs < MAX_REPAIRS =>
                {
                    repairs_in_row += 1;
                    repairs += 1;
                    metrics.record_retry();
                    on_event(HarnessEvent::Retry {
                        role: role_name(Role::Reason).to_string(),
                        attempt: repairs,
                        delay_ms: 0,
                        error: detail.clone(),
                        repair: true,
                    });
                    let mut round = Vec::new();
                    if !unusable.trim().is_empty() {
                        round.push(Message::assistant(unusable));
                    }
                    round.push(Message::user(format!("{REPAIR_NOTE} ({detail})")));
                    ws.push_round(round);
                    retrieved.push(0);
                    continue;
                }
                Err(AgentError::MemoryChanged) => continue,
                Err(e) => return Err(e),
            }) else {
                cancelled = true;
                break;
            };
            repairs_in_row = 0;

            // A full answer beside reads of what this turn changed is a check of its own work.
            // A second one that repeats the first is the model restating its answer, not
            // working: its reads do not run and the reply is taken as the answer, through the
            // same checks as any other (ADR 0051). A change in between starts over.
            if rechecks(&completion, engine, &written) {
                if checked
                    .as_deref()
                    .is_some_and(|before| restates(before, &completion.content))
                {
                    tracing::info!(
                        calls = completion.tool_calls.len(),
                        "the answer was restated beside another check of the same changes; ending the turn"
                    );
                    completion.tool_calls.clear();
                } else {
                    checked = Some(completion.content.clone());
                }
            } else if completion.tool_calls.iter().any(|c| {
                engine
                    .registry
                    .descriptor(&c.name)
                    .is_some_and(|d| d.risk == RiskTier::Mutate)
            }) {
                checked = None;
            }

            if completion.tool_calls.is_empty() {
                // The user said more while this answer was being written: treat the text as
                // narration and keep going with their message.
                let late = steer::drain(&self.steer);
                if !late.is_empty() {
                    let mut round = vec![Message::assistant(completion.content.clone())];
                    self.deliver_steering(late, &mut round, &mut steered, &mut on_event);
                    ws.push_round(round);
                    retrieved.push(0);
                    stall = 0;
                    continue;
                }
                // A reply that writes a tool call as text ran nothing; taking it as the answer
                // loses the step (a small model does this with the working state last). Say so
                // once and let it try again.
                if !corrected_text_call && writes_tool_call_as_text(&completion.content, &tools) {
                    corrected_text_call = true;
                    tracing::info!("the model wrote a tool call as text; asking again");
                    ws.push_round(vec![
                        Message::assistant(completion.content.clone()),
                        Message::user(
                            "Your last reply wrote a tool call as text, so nothing ran. To use a tool, call it as a tool. If you are done, reply with the final answer only.",
                        ),
                    ]);
                    retrieved.push(0);
                    continue;
                }
                // An answer that says a file was saved when no call of this turn wrote it (a run
                // that wrote a chart but not the report it was meant to): say so once, with the
                // files that were written, and let the model save it or correct the answer.
                if !corrected_claim {
                    let missing = unwritten_claims(
                        &completion.content,
                        &written,
                        || engine.workspace_dir(project).ok(),
                        turn_started,
                    );
                    if !missing.is_empty() {
                        corrected_claim = true;
                        tracing::info!(files = ?missing, "the answer names files no call wrote; asking again");
                        ws.push_round(vec![
                            Message::assistant(completion.content.clone()),
                            Message::user(unwritten_note(&missing, &written)),
                        ]);
                        retrieved.push(0);
                        continue;
                    }
                }
                // An answer that ends by saying what it will do next ("I will now call
                // files.edit") while it can still call tools stopped before the work: a small
                // model does this after reading what it was asked to change. Say so once.
                if !corrected_announce && !tools.is_empty() && announces_work(&completion.content) {
                    corrected_announce = true;
                    tracing::info!("the answer announces work it has not done; asking again");
                    ws.push_round(vec![
                        Message::assistant(completion.content.clone()),
                        Message::user(ANNOUNCED_NOTE),
                    ]);
                    retrieved.push(0);
                    continue;
                }
                answer = completion.content.trim().to_string();
                answered = true;
                streamed = !answer.is_empty();
                break;
            }

            // The model's note with its calls stays in the round for the ledger (it is not sent
            // while the round is shown: `budget::sent`) and goes on each call's record, so work
            // that stops continues with it.
            let mut said = Message::assistant_tool_calls(completion.tool_calls.clone());
            said.content = completion.content.clone();
            let mut round = vec![said];
            let mut round_retrieved = 0u32;
            let narrated = engine.narrating(&completion.content);

            // Reads and observations of this step that need no approval and were not reused
            // run together (see `flush`). Anything else first finishes the pending batch, so
            // results stay in call order.
            let mut batch: Vec<Pending> = Vec::new();
            let mut tally = Tally {
                metrics: &mut metrics,
                cited: &mut cited,
                repeated_failures: &mut repeated_failures,
                failed_runs: &mut failures_run,
                changed_at: &mut changed_at,
                written: &mut written,
            };
            macro_rules! flush {
                () => {
                    if !batch.is_empty()
                        && !self
                            .flush(
                                &narrated,
                                profile,
                                project,
                                &mut batch,
                                &mut tally,
                                &mut round,
                                &mut on_event,
                            )
                            .await?
                    {
                        cancelled = true;
                        break;
                    }
                };
            }

            for call in &completion.tool_calls {
                if self.is_cancelled() {
                    cancelled = true;
                    break;
                }
                let guarded = match &guard {
                    Some(guard) => {
                        let meta = catalog::is_meta_tool(&call.name);
                        if meta && !crate::guard::safe_meta(&call.name) {
                            flush!();
                            round.push(Message::tool_result(
                                call,
                                "refused: this operation has no guarded effect contract",
                            ));
                            continue;
                        }
                        let descriptor = if meta {
                            None
                        } else {
                            engine.registry.descriptor(&call.name)
                        };
                        let input = if meta {
                            call.arguments.clone()
                        } else {
                            engine.canonical_input(project, call.arguments.clone())
                        };
                        match guard.check(&call.name, &input, descriptor.as_ref()) {
                            crate::guard::Verdict::Refuse(reason) => {
                                flush!();
                                round.push(Message::tool_result(
                                    call,
                                    format!(
                                        "refused: {reason}. This run cannot broaden its authority."
                                    ),
                                ));
                                continue;
                            }
                            crate::guard::Verdict::Ask
                                if meta
                                    || descriptor.as_ref().is_none_or(|descriptor| {
                                        descriptor.risk != RiskTier::Mutate
                                    }) =>
                            {
                                flush!();
                                round.push(Message::tool_result(
                                    call,
                                    "refused: this operation has no guarded approval route",
                                ));
                                continue;
                            }
                            verdict => Some(verdict),
                        }
                    }
                    None => None,
                };
                if catalog::is_meta_tool(&call.name) {
                    flush!();
                    let repeat = !meta_seen.insert(call_digest(&call.name, &call.arguments));
                    if !repeat && counts_as_progress(&call.name) {
                        fresh_meta += 1;
                    }
                    let result = self
                        .handle_meta(
                            engine,
                            profile,
                            project,
                            call,
                            &mut loaded,
                            &mut added,
                            &mut state,
                            tally.metrics,
                            &mut on_event,
                        )
                        .await?;
                    let result = if repeat && counts_as_progress(&call.name) {
                        format!("(you already made this exact call in this task; the result has not changed. Try something different or answer.)\n{result}")
                    } else {
                        result
                    };
                    if catalog::is_retrieval(&call.name) {
                        round_retrieved += estimate_tokens(&result) + 4;
                    }
                    round.push(Message::tool_result(call, result));
                    continue;
                }

                let Some(desc) = engine.registry.descriptor(&call.name) else {
                    flush!();
                    round.push(Message::tool_result(
                        call,
                        format!("error: unknown tool `{}`. Call load_action(name) first, or check the capability index.", call.name),
                    ));
                    continue;
                };

                if !self.descriptor_available(&desc) {
                    flush!();
                    round.push(Message::tool_result(
                        call,
                        "unavailable in Plan: answer with a plan and do not change state.",
                    ));
                    continue;
                }

                // The digest uses the canonical artifact id, so a short id and a full id name
                // the same call.
                let canonical = engine.canonical_input(project, call.arguments.clone());
                let digest = call_digest(&call.name, &canonical);
                // An identical call in this reply must finish before its failure count is read.
                // Otherwise four copies in one reply could all pass the limit before the first
                // result arrives.
                if batch.iter().any(|p| p.digest == digest) {
                    flush!();
                }
                // An identical change that already ran twice in this task is not run, or asked
                // about, again: the same program fails the same way, the same refused write is
                // refused again. Twice allows one retry of a transient failure.
                if desc.risk == RiskTier::Mutate
                    && changes_run.get(&digest).copied().unwrap_or(0) >= MAX_IDENTICAL_CHANGES
                {
                    flush!();
                    round.push(Message::tool_result(call, REPEATED_CHANGE_NOTE));
                    continue;
                }
                if desc.risk == RiskTier::Mutate {
                    if let Some(inv) = done_before.get(&digest) {
                        flush!();
                        let reused = format!("{} ({})", inv.id, inv.action);
                        on_event(HarnessEvent::Duplicate {
                            action: call.name.clone(),
                            reused: reused.clone(),
                        });
                        tally.cited.push(inv.id.clone());
                        round.push(Message::tool_result(
                            call,
                            format!(
                                "{DONE_BEFORE_NOTE} [{}]: {}",
                                inv.id,
                                inv.output.to_string().chars().take(600).collect::<String>()
                            ),
                        ));
                        continue;
                    }
                    // A change waits for the reads before it, and runs alone.
                    flush!();
                }
                // A failed read or observation gets one retry. A third identical execution has
                // the same input and no intervening successful change, so return feedback at
                // once. Different input still runs, and a successful change clears this guard.
                if desc.risk != RiskTier::Mutate
                    && tally.failed_runs.get(&digest).copied().unwrap_or(0)
                        >= MAX_IDENTICAL_FAILURES
                {
                    flush!();
                    tally.metrics.record_duplicate();
                    round.push(Message::tool_result(call, REPEATED_FAILURE_NOTE));
                    continue;
                }
                // Asked of the person until they allow it once; with no one to ask, it is never
                // run again on its own: only the person can tell whether it happened.
                let again = desc.risk == RiskTier::Mutate && cut_short.contains(&digest);
                if again && self.approvals.is_none() {
                    round.push(Message::tool_result(call, CUT_SHORT_NOTE));
                    continue;
                }
                on_event(HarnessEvent::ToolCall {
                    id: call.id.clone(),
                    action: call.name.clone(),
                    input: call.arguments.clone(),
                });

                let auto = self.execution_mode == ExecutionMode::Auto
                    && desc.auto_eligible
                    && !desc.ask_each_time;
                let mut pending_invocation = None;
                let mut approved = match &guarded {
                    Some(verdict) => {
                        *verdict == crate::guard::Verdict::Allow && !again && !desc.person_only
                    }
                    None => (self.allow_mutate || auto) && !again && !desc.person_only,
                };
                if desc.risk == RiskTier::Mutate && !approved {
                    match self
                        .request_approval(
                            ApprovalContext {
                                engine: &narrated,
                                project,
                                profile,
                            },
                            &desc,
                            call,
                            again,
                            guarded.is_some(),
                            &mut on_event,
                        )
                        .await?
                    {
                        ApprovalReply::Cancelled => {
                            cancelled = true;
                            break;
                        }
                        ApprovalReply::Decided {
                            decision: ApprovalDecision::Deny,
                            ..
                        } => {
                            round.push(Message::tool_result(
                                call,
                                "denied: the user declined this action. Do not retry it; continue from read-only evidence or ask the user.",
                            ));
                            continue;
                        }
                        ApprovalReply::Decided { invocation, .. } => {
                            approved = true;
                            pending_invocation = Some(*invocation);
                            cut_short.remove(&digest);
                        }
                        ApprovalReply::Unavailable => {
                            on_event(HarnessEvent::NeedsApproval {
                                action: call.name.clone(),
                            });
                            round.push(Message::tool_result(
                                call,
                                "denied: this action changes state and needs human approval. Do not retry; reason from read-only evidence instead.",
                            ));
                            continue;
                        }
                    }
                }

                // Dedup: reuse identical prior evidence instead of re-running the tool (see
                // `Reuse`). A state-changing call is never deduplicated: running it again is
                // the point. Only a successful result is evidence. A failure (an error, a
                // timeout, a missing tool) may be transient, so the same call runs again; a
                // repeat that fails again counts as no progress, which keeps the stall guard
                // working.
                let failed_before = desc.risk != RiskTier::Mutate
                    && tally.failed_runs.get(&digest).copied().unwrap_or(0) > 0;
                if desc.risk != RiskTier::Mutate && desc.reuse != Reuse::Volatile {
                    let prior = engine.store.find_card_by_digest(project, &digest)?;
                    let reuse = prior.filter(|card| {
                        card.ok
                            && match desc.reuse {
                                Reuse::Stable => true,
                                // Within this turn, and after the last state change.
                                _ if card.created_at >= turn_started => {
                                    card.created_at >= *tally.changed_at
                                }
                                _ => reads_stored_artifact(engine, project, &desc, &canonical),
                            }
                    });
                    if let Some(existing) = reuse {
                        flush!();
                        tally.metrics.record_duplicate();
                        let reused = existing.reference_line();
                        on_event(HarnessEvent::Duplicate {
                            action: call.name.clone(),
                            reused: reused.clone(),
                        });
                        tally.cited.push(existing.invocation.clone());
                        round.push(Message::tool_result(
                            call,
                            format!("(reused prior evidence) {reused}"),
                        ));
                        continue;
                    }
                }

                if desc.risk == RiskTier::Mutate {
                    *changes_run.entry(digest.clone()).or_default() += 1;
                }
                let mut executed = call.clone();
                if let Some(invocation) = &pending_invocation {
                    executed.arguments = invocation.input.clone();
                }
                batch.push(Pending {
                    call: executed,
                    risk: desc.risk,
                    approved,
                    invocation: pending_invocation,
                    failed_before,
                    digest,
                });
                if desc.risk == RiskTier::Mutate {
                    flush!();
                }
            }
            if !cancelled
                && !batch.is_empty()
                && !self
                    .flush(
                        &narrated,
                        profile,
                        project,
                        &mut batch,
                        &mut tally,
                        &mut round,
                        &mut on_event,
                    )
                    .await?
            {
                cancelled = true;
            }

            if cancelled {
                break;
            }

            let progress_after =
                metrics.cards_created - repeated_failures + fresh_meta + loaded.len() as u32;
            let late = steer::drain(&self.steer);
            if !late.is_empty() {
                self.deliver_steering(late, &mut round, &mut steered, &mut on_event);
                stall = 0;
            } else if progress_after > progress_before {
                stall = 0;
            } else {
                stall += 1;
                if stall == STALL_STEER {
                    tracing::info!(idle_steps = stall, "no progress; nudging the model");
                    on_event(HarnessEvent::Stall {
                        idle_steps: stall,
                        stopped: false,
                    });
                    round.push(Message::user(
                        "You have gathered no new evidence for several steps. Either call load_action(name) \
                         and then a concrete action to produce evidence, or stop now and give your best \
                         grounded answer. Do not repeat discovery calls you have already made.",
                    ));
                }
            }
            ws.push_round(round);
            retrieved.push(round_retrieved);
            if stall >= STALL_MAX {
                tracing::info!(idle_steps = stall, "no progress; forcing a final answer");
                on_event(HarnessEvent::Stall {
                    idle_steps: stall,
                    stopped: true,
                });
                break;
            }
        }

        if cancelled {
            state.touch();
            engine.store.put_research_state(project, &state)?;
            metrics.finish();
            let stopped = self.stopped_event();
            let interrupted = matches!(stopped, HarnessEvent::Interrupted);
            on_event(stopped);
            return Ok(HarnessOutcome {
                answer: String::new(),
                cited,
                steps: steps_taken,
                trivial,
                metrics,
                state,
                cancelled: true,
                interrupted,
                steered,
                ran_out: false,
            });
        }

        // The loop ended without the model's own answer: the work may not be done.
        let ran_out = !answered;
        for refresh in 0..3 {
            if !answer.is_empty() {
                break;
            }
            first.memory = crate::memory::recall(&engine.store, project, &goal_text, budget)?;
            let refreshed = first.render();
            let first_tokens = estimate_tokens(&refreshed) + 4 + self.picture_tokens();
            ws.pin(self.task_message(refreshed));
            match self
                .force_answer(
                    profile,
                    &mut ws,
                    &sys,
                    (&first, first_tokens, &retrieved, loaded.len(), steps_taken),
                    &state,
                    &mut metrics,
                    &mut on_event,
                    &crate::memory::Snapshot {
                        store: &engine.store,
                        project,
                        task: &goal_text,
                        budget,
                        selected: &first.memory,
                    },
                )
                .await
            {
                Err(AgentError::MemoryChanged) if refresh < 2 => continue,
                Err(e) => return Err(e),
                Ok(Some(a)) => {
                    answer = a;
                    streamed = !answer.is_empty();
                    break;
                }
                Ok(None) => {
                    metrics.finish();
                    let stopped = self.stopped_event();
                    let interrupted = matches!(stopped, HarnessEvent::Interrupted);
                    on_event(stopped);
                    return Ok(HarnessOutcome {
                        answer: String::new(),
                        cited,
                        steps: steps_taken,
                        trivial,
                        metrics,
                        state,
                        cancelled: true,
                        interrupted,
                        steered,
                        ran_out: false,
                    });
                }
            }
        }

        let guarded = engine.finalize_answer(project, &cited, answer.clone())?;
        if guarded != answer {
            if streamed && !answer.is_empty() {
                on_event(HarnessEvent::TextWithdrawn {
                    text: answer.clone(),
                });
            }
            answer = guarded;
            streamed = false;
        }
        let (passed, detail) = self.self_check(&answer, &cited, trivial, steps_taken);
        on_event(HarnessEvent::SelfCheck {
            passed,
            detail: detail.clone(),
        });
        if !passed && self.critic && !trivial {
            if let Some(revised) = self
                .run_critic(
                    profile,
                    &system,
                    &state,
                    &answer,
                    &detail,
                    &mut metrics,
                    &mut on_event,
                )
                .await
            {
                answer = revised;
                streamed = false;
            }
        }
        let guarded = engine.finalize_answer(project, &cited, answer.clone())?;
        if guarded != answer {
            answer = guarded;
            streamed = false;
        }

        let mut note = Note::new(NoteKind::Finding, &answer, Author::Agent);
        note.cites_invocations = cited.clone();
        engine.store.put_note(project, &note)?;
        state.touch();
        engine.store.put_research_state(project, &state)?;

        metrics.finish();
        let checkpoint = json!({
            "goal": state.goal,
            "steps": steps_taken,
            "metrics": metrics,
            "cited": cited.iter().map(|i| i.as_str()).collect::<Vec<_>>(),
        });
        engine.store.put_checkpoint(project, &checkpoint)?;

        tracing::info!(
            steps = steps_taken,
            model_calls = metrics.total_model_calls(),
            prompt_tokens = metrics.prompt_tokens,
            cached_tokens = metrics.cached_tokens,
            completion_tokens = metrics.completion_tokens,
            tool_calls = metrics.tool_calls,
            duplicates = metrics.duplicate_tool_calls,
            retries = metrics.provider_retries,
            ms = metrics.elapsed_ms,
            "turn finished"
        );
        on_event(HarnessEvent::Answer {
            text: answer.clone(),
            streamed,
        });
        Ok(HarnessOutcome {
            answer,
            cited,
            steps: steps_taken,
            trivial,
            metrics,
            state,
            cancelled: false,
            interrupted: false,
            steered,
            ran_out,
        })
    }

    /// What the first request of a turn would contain right now, without calling a model.
    /// Lets a front-end answer "what is in my context?" between turns with real numbers.
    pub async fn preview_context(
        &self,
        engine: &Engine,
        profile: &AgentProfile,
        project: &ProjectId,
        task: &str,
    ) -> Result<ContextReport, AgentError> {
        let query = self.goal_query(task);
        let state = engine
            .store
            .get_research_state(project)?
            .unwrap_or_default();
        let sys = self.system_prompt(engine, profile).await;
        let system = sys.render();
        let wsctx = self.build_workspace_context(engine, project)?;
        let mut loaded = core_loaded(engine, profile);
        let core = loaded.len();
        for name in
            catalog::matching_actions(&engine.registry, &self.with_focus(&query), self.preload)
        {
            if !loaded.contains(&name) {
                loaded.push(name);
            }
        }
        // As in a turn: a whole-cache provider gets the preloads from the second request.
        let held: Vec<String> = if self
            .router
            .provider(Role::Reason)
            .caches_system_prompt_whole()
        {
            loaded.split_off(core)
        } else {
            Vec::new()
        };
        let first = FirstTurn {
            project: self.project_block(engine, project)?,
            memory: crate::memory::recall(&engine.store, project, &query, self.request_budget())?,
            workspace: wsctx.digest(6),
            conversation: self.conversation.clone(),
            rules: self.rules_text(),
            skills: self.skills.clone(),
            can_add: can_add(profile),
            ready: self.ready_block(engine, &[loaded.as_slice(), held.as_slice()].concat()),
            task: self.task_text(task, self.request_budget()),
        };
        let first_text = first.render();
        let first_tokens = estimate_tokens(&first_text) + 4 + self.picture_tokens();
        let mut ws = WorkingSet::new(self.request_budget());
        ws.pin(self.task_message(first_text));
        let state_digest = state.digest(STATE_DIGEST_ITEMS);
        let tail = state_message(&state_digest);
        let (_, est) = ws.assemble_with(&system, WorkingSet::message_tokens(&tail));
        let meta_cfg = MetaToolConfig {
            knowledge: profile.knowledge.is_some(),
            acquisition: self.execution_mode != ExecutionMode::Plan && acquisition_on(profile),
            present: self.presentation,
        };
        let tools = self.tool_set(engine, &loaded, meta_cfg);
        Ok(self.report(
            profile,
            &sys,
            &state_digest,
            &first,
            first_tokens,
            &ws,
            &[0],
            est,
            &tools,
            loaded.len(),
            0,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn report(
        &self,
        profile: &AgentProfile,
        sys: &SystemPrompt,
        state_digest: &str,
        first: &FirstTurn,
        first_tokens: u32,
        ws: &WorkingSet,
        retrieved: &[u32],
        total: u32,
        tools: &[ToolDef],
        loaded: usize,
        step: usize,
    ) -> ContextReport {
        let instructions = estimate_tokens(&profile.prompts.instructions);
        // The runtime's own lines: the rules after the instructions, and the project block
        // (in the first message).
        let project_block = estimate_tokens(&first.project);
        let project = estimate_tokens(&sys.rules).saturating_sub(instructions) + project_block;
        let ready = first.ready.as_deref().map(estimate_tokens).unwrap_or(0)
            + first.can_add.as_deref().map(estimate_tokens).unwrap_or(0);
        // The first message is the pinned head: always in the request.
        let capability_index = estimate_tokens(&sys.index) + ready;
        let working_state = WorkingSet::message_tokens(&state_message(state_digest));
        let system_total = estimate_tokens(&sys.render());
        // The head holds workspace + conversation + rules + skills + task; attribute its
        // framing to the task.
        let w = first.workspace.as_deref().map(estimate_tokens).unwrap_or(0);
        let c = first
            .conversation
            .as_deref()
            .map(estimate_tokens)
            .unwrap_or(0);
        let r = first.rules.as_deref().map(estimate_tokens).unwrap_or(0);
        let k = first.skills.as_deref().map(estimate_tokens).unwrap_or(0);
        let memory = first.memory.as_deref().map(estimate_tokens).unwrap_or(0);
        let (workspace, conversation, rules, skills, task) = (
            w,
            c,
            r,
            k,
            first_tokens.saturating_sub(w + c + r + k + ready + memory + project_block),
        );
        let first_kept = first_tokens;
        let ledger = ws.ledger_tokens();
        // Rounds before `last_compressed` were dropped from this request.
        let retrieved: u32 = retrieved.iter().skip(ws.last_compressed).sum();
        let model_output = ws.kept_model_output();
        let tool_rounds = total
            .saturating_sub(system_total)
            .saturating_sub(working_state)
            .saturating_sub(first_kept)
            .saturating_sub(ledger)
            .saturating_sub(retrieved)
            .saturating_sub(model_output);
        let schema = |t: &ToolDef| {
            estimate_tokens(&t.name)
                + estimate_tokens(&t.description)
                + estimate_tokens(&t.parameters.to_string())
        };
        let tool_schemas = tools.iter().map(schema).sum();
        let action_schemas = tools
            .iter()
            .filter(|t| !catalog::is_meta_tool(&t.name))
            .map(schema)
            .sum();
        ContextReport {
            step,
            role: "reason".into(),
            model_output,
            action_schemas,
            instructions,
            project,
            capability_index,
            working_state,
            conversation,
            workspace,
            rules,
            skills,
            memory,
            task,
            tool_rounds,
            retrieved,
            ledger,
            compressed_rounds: ws.last_compressed,
            tool_schemas,
            loaded_actions: loaded,
            total,
            budget: self.request_budget(),
        }
    }

    /// Run a batch of pending adapter calls, at most [`MAX_PARALLEL_TOOLS`] at a time, then
    /// record each result in call order: invocation, evidence card, event, message. Returns
    /// false when the turn was cancelled while they ran (nothing of the batch is recorded;
    /// dropping the futures stops their processes).
    #[allow(clippy::too_many_arguments)]
    async fn flush(
        &self,
        engine: &Engine,
        profile: &AgentProfile,
        project: &ProjectId,
        batch: &mut Vec<Pending>,
        tally: &mut Tally<'_>,
        round: &mut Vec<Message>,
        on_event: &mut impl FnMut(HarnessEvent),
    ) -> Result<bool, AgentError> {
        use futures::StreamExt;
        let pending = std::mem::take(batch);
        let jobs: Vec<_> = pending
            .iter()
            .map(|p| {
                (
                    p.call.name.clone(),
                    p.call.arguments.clone(),
                    p.approved,
                    p.invocation.clone(),
                )
            })
            .collect();
        let runs = futures::stream::iter(jobs.into_iter().map(
            |(name, input, approved, invocation)| async move {
                let fence = self
                    .cancel
                    .clone()
                    .map(|token| Arc::new(token) as Arc<dyn ark_tools::CommitFence>);
                engine
                    .run_approved_action(project, &name, input, approved, invocation, fence)
                    .await
            },
        ))
        .buffered(MAX_PARALLEL_TOOLS)
        .collect::<Vec<_>>();
        // A stopping process lets a batch that changes something finish: dropped halfway, the
        // change may or may not have happened. Only a cancel drops it.
        let changes = pending.iter().any(|p| p.risk == RiskTier::Mutate);
        let results = if changes {
            tokio::select! {
                biased;
                _ = cancel::wait(&self.cancel) => None,
                out = runs => Some(out),
            }
        } else {
            self.cancellable(runs).await
        };
        let Some(results) = results else {
            if let Some(cancel) = &self.cancel {
                cancel
                    .wait_for_commit()
                    .await
                    .map_err(std::io::Error::other)?;
            }
            return Ok(false);
        };
        for (p, inv) in pending.iter().zip(results) {
            let inv = inv?;
            if p.risk == RiskTier::Mutate && inv.status != InvocationStatus::PendingApproval {
                // Something changed: earlier reads this turn may no longer hold.
                *tally.changed_at = time::OffsetDateTime::now_utc();
            }
            tally.metrics.record_tool_call();
            let ok = matches!(inv.status, InvocationStatus::Ok);
            if ok {
                tally.failed_runs.remove(&p.digest);
                if p.risk == RiskTier::Mutate {
                    // A successful change may have repaired any failed read or observation.
                    tally.failed_runs.clear();
                }
            } else if p.risk != RiskTier::Mutate {
                *tally.failed_runs.entry(p.digest.clone()).or_default() += 1;
            }
            let card = distill(&inv, &profile.distillers);
            engine.store.put_evidence_card(project, &card)?;
            tally.metrics.record_card_created();
            if !ok && p.failed_before {
                *tally.repeated_failures += 1;
            }
            tally.cited.push(inv.id.clone());

            if !ok {
                let error: String = inv
                    .error
                    .clone()
                    .unwrap_or_default()
                    .chars()
                    .take(160)
                    .collect();
                tracing::info!(action = %p.call.name, %error, "tool call failed");
            }
            let mut model_text = card_for_model(&card);
            // A picture the call took goes with its result, to a model that reads pictures.
            let mut pictures: Vec<ark_model::Picture> = engine
                .take_pictures(inv.id.as_str())
                .into_iter()
                .map(|p| ark_model::Picture {
                    media_type: p.media_type,
                    bytes: p.bytes,
                })
                .collect();
            if !pictures.is_empty() && !self.router.provider(Role::Reason).reads_pictures() {
                pictures.clear();
                model_text.push_str("\n(a picture was taken, but this model is not sent pictures)");
            }
            let raw_tokens = estimate_tokens(&inv.output.to_string());
            let (mut files, mut before) = (Vec::new(), Vec::new());
            for a in inv
                .produced
                .iter()
                .filter_map(|id| engine.store.get_artifact(project, id).ok().flatten())
            {
                let Some(path) = a.meta.get("path").and_then(|p| p.as_str()) else {
                    continue;
                };
                if a.meta["before"] == true {
                    before.push(FileBefore {
                        path: path.to_string(),
                        artifact: a.id.as_str().to_string(),
                    });
                } else {
                    files.push(path.to_string());
                }
            }
            tally.written.extend(files.iter().cloned());
            on_event(HarnessEvent::ToolResult {
                id: p.call.id.clone(),
                action: p.call.name.clone(),
                ok,
                summary: card.summary.clone(),
                invocation: inv.id.as_str().to_string(),
                duration_ms: inv.duration_ms(),
                error: inv.error.clone(),
                // The same per-message estimate the working set budgets with.
                context_tokens: estimate_tokens(&model_text)
                    + 4
                    + pictures.len() as u32 * ark_model::PICTURE_TOKENS,
                raw_tokens,
                files,
                before,
            });
            round.push(Message::tool_result(&p.call, model_text).with_pictures(pictures));
        }
        Ok(true)
    }

    /// Hand the user's mid-turn messages to the model at the end of `round`.
    fn deliver_steering(
        &self,
        texts: Vec<steer::SteeringInput>,
        round: &mut Vec<Message>,
        steered: &mut Vec<String>,
        on_event: &mut impl FnMut(HarnessEvent),
    ) {
        for input in texts {
            let text = input.model_text();
            round.push(Message::user(format!(
                "The user added this while you were working. Take it into account and continue:\n{text}"
            )));
            on_event(HarnessEvent::Steered {
                text: input.text,
                context: input.context,
            });
            steered.push(text);
        }
    }

    /// The task the model gets: the question, or for a turn that continues an unfinished one,
    /// its goal with what the person said since (the question last). The goal's request is
    /// clipped to fit the budget: `budget / 2` characters, 1,000 to 4,000.
    fn task_text(&self, question: &str, budget: u32) -> String {
        let task = match &self.goal {
            Some(g) => g.render((budget as usize / 2).clamp(1_000, 4_000)),
            None => question.to_string(),
        };
        match self.execution_mode {
            ExecutionMode::Plan => format!(
                "Plan only. Inspect what you need, but do not change project or external state. Return an actionable plan.\n\n{task}"
            ),
            _ => task,
        }
    }

    /// The event a run that stopped ends with: the person's cancel, or the process stopping.
    fn stopped_event(&self) -> HarnessEvent {
        let asked = self.cancel.as_ref().is_some_and(|c| c.is_cancelled());
        let stopping = self.stopping.as_ref().is_some_and(|c| c.is_cancelled());
        if stopping && !asked {
            HarnessEvent::Interrupted
        } else {
            HarnessEvent::Cancelled
        }
    }

    fn is_cancelled(&self) -> bool {
        [&self.cancel, &self.stopping]
            .into_iter()
            .flatten()
            .any(|c| c.is_cancelled())
    }

    /// Run a future unless the turn is cancelled, or the process stops, first. `None` means
    /// cancelled.
    async fn cancellable<F: std::future::Future>(&self, fut: F) -> Option<F::Output> {
        tokio::select! {
            biased;
            _ = cancel::wait_either(&self.cancel, &self.stopping) => None,
            out = fut => Some(out),
        }
    }

    /// One model call for `role`, streaming text to `on_event` when `stream` is set, recording
    /// usage in `metrics`. `Ok(None)` means the turn was cancelled mid-call.
    #[allow(clippy::too_many_arguments)]
    async fn call_model(
        &self,
        role: Role,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
        stream: bool,
        metrics: &mut Metrics,
        on_event: &mut impl FnMut(HarnessEvent),
        repair: Option<&mut String>,
        memory: Option<&crate::memory::Snapshot<'_>>,
    ) -> Result<Option<Completion>, AgentError> {
        let provider = self.router.provider(role).clone();
        if self.guard.is_some() && !provider.host_controls_tools() {
            return Err(AgentError::NotAllowed(
                "the selected model cannot enforce this execution's permissions".into(),
            ));
        }
        let mut repair = repair;
        let started = std::time::Instant::now();
        let mut retries = 0u32;
        // When this call first failed: an unreachable model is waited for that long.
        let mut failing_since: Option<std::time::Instant> = None;
        loop {
            if let Some(memory) = memory {
                memory.check()?;
            }
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let sink = move |t: &str| {
                let _ = tx.send(t.to_string());
            };
            let attempt_started = std::time::Instant::now();
            let mut first_token_ms: Option<u64> = None;
            // What this attempt showed: withdrawn if the attempt fails and is retried.
            let mut shown = String::new();
            let fut = provider.complete_streaming(system, messages, tools, &sink);
            tokio::pin!(fut);
            let result = loop {
                tokio::select! {
                    biased;
                    _ = cancel::wait_either(&self.cancel, &self.stopping) => return Ok(None),
                    Some(text) = rx.recv() => {
                        first_token_ms.get_or_insert(attempt_started.elapsed().as_millis() as u64);
                        if stream {
                            shown.push_str(&text);
                            on_event(HarnessEvent::TextDelta { text });
                        }
                    }
                    r = &mut fut => break r,
                }
            };
            while let Ok(text) = rx.try_recv() {
                first_token_ms.get_or_insert(attempt_started.elapsed().as_millis() as u64);
                if stream {
                    shown.push_str(&text);
                    on_event(HarnessEvent::TextDelta { text });
                }
            }
            let completion = match result {
                Ok(c) => c,
                // The model's own reply cannot be used. Sending the same request again asks
                // for the same mistake: with `repair`, the caller tells the model what was
                // wrong instead (ADR 0050). What it showed is withdrawn and handed back.
                Err(e @ ark_model::ProviderError::Malformed { .. }) if repair.is_some() => {
                    if !shown.is_empty() {
                        on_event(HarnessEvent::TextWithdrawn {
                            text: shown.clone(),
                        });
                    }
                    if let Some(out) = repair.as_deref_mut() {
                        *out = shown;
                    }
                    tracing::warn!(role = role_name(role), error = %e, "the model's reply could not be used");
                    return Err(AgentError::model(&e));
                }
                // What was shown does not decide whether to retry: the shown text is withdrawn
                // and the retry streams its own (ADR 0049). Tools run only after a call
                // succeeds, so none runs twice.
                Err(e)
                    if self.retry.should_retry_for(
                        &e,
                        retries,
                        failing_since
                            .get_or_insert_with(std::time::Instant::now)
                            .elapsed(),
                    ) =>
                {
                    if !shown.is_empty() {
                        on_event(HarnessEvent::TextWithdrawn { text: shown });
                    }
                    let delay = self.retry.delay(retries, &e);
                    retries += 1;
                    metrics.record_retry();
                    tracing::warn!(
                        role = role_name(role),
                        attempt = retries,
                        delay_ms = delay.as_millis() as u64,
                        error = %e,
                        "model call failed; retrying"
                    );
                    on_event(HarnessEvent::Retry {
                        role: role_name(role).to_string(),
                        attempt: retries,
                        delay_ms: delay.as_millis() as u64,
                        error: e.to_string(),
                        repair: false,
                    });
                    if self.cancellable(tokio::time::sleep(delay)).await.is_none() {
                        return Ok(None);
                    }
                    continue;
                }
                Err(e) => {
                    tracing::warn!(role = role_name(role), retries, error = %e, "model call failed");
                    return Err(AgentError::model(&e));
                }
            };
            let model = provider.model_id();
            metrics.record_call(role, &model, completion.usage);
            tracing::debug!(
                role = role_name(role),
                model = %model,
                prompt = completion.usage.prompt_tokens,
                cached = completion.usage.cached_tokens,
                completion = completion.usage.completion_tokens,
                ms = started.elapsed().as_millis() as u64,
                ttft_ms = first_token_ms,
                retries,
                "model call"
            );
            on_event(HarnessEvent::ModelCall {
                role: role_name(role).to_string(),
                model,
                usage: completion.usage,
                elapsed_ms: started.elapsed().as_millis() as u64,
                first_token_ms,
                retries,
            });
            return Ok(Some(completion));
        }
    }

    /// Persist the exact call before asking for its approval.
    async fn request_approval(
        &self,
        context: ApprovalContext<'_>,
        desc: &ark_core::ActionDescriptor,
        call: &ark_model::ToolCall,
        cut_short: bool,
        every_time: bool,
        on_event: &mut impl FnMut(HarnessEvent),
    ) -> Result<ApprovalReply, AgentError> {
        let gate = match &self.approvals {
            None => return Ok(ApprovalReply::Unavailable),
            Some(g) => g.clone(),
        };
        let invocation =
            context
                .engine
                .pending_approval(context.project, &call.name, call.arguments.clone())?;
        let request = ApprovalRequest {
            invocation: Some(invocation.id.clone()),
            project: Some(context.project.clone()),
            action: call.name.clone(),
            summary: desc.summary.clone(),
            risk: desc.risk,
            input: invocation.input.clone(),
            class: desc
                .grant
                .as_ref()
                .and_then(|g| g.class(&invocation.input))
                .unwrap_or_else(|| {
                    context
                        .profile
                        .approval_class(&call.name, &invocation.input)
                }),
            preview: preview_call(&call.name, &invocation.input),
            agent: None,
            task: None,
            ask_each_time: desc.ask_each_time || cut_short || every_time,
            cut_short,
        };
        if request.ask_each_time || !gate.is_granted(&request.class) {
            on_event(HarnessEvent::ApprovalRequested {
                request: request.clone(),
            });
        }
        let Some(decision) = self.cancellable(gate.check(&request)).await else {
            return Ok(ApprovalReply::Cancelled);
        };
        if decision == ApprovalDecision::Deny {
            context.engine.deny_pending(context.project, &invocation)?;
        }
        on_event(HarnessEvent::ApprovalResolved {
            action: request.action.clone(),
            class: request.class.clone(),
            decision,
        });
        Ok(ApprovalReply::Decided {
            decision,
            invocation: Box::new(invocation),
        })
    }

    fn tool_set(
        &self,
        engine: &Engine,
        loaded: &[String],
        cfg: MetaToolConfig,
    ) -> Vec<ark_model::ToolDef> {
        let mut tools = catalog::meta_tool_defs(cfg);
        for name in loaded {
            if let Some(d) = engine
                .registry
                .descriptor(name)
                .filter(|d| self.descriptor_available(d))
            {
                tools.push(catalog::action_tooldef(&d));
            }
        }
        tools
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_meta(
        &self,
        engine: &Engine,
        profile: &AgentProfile,
        project: &ProjectId,
        call: &ark_model::ToolCall,
        loaded: &mut Vec<String>,
        added: &mut Vec<Arc<dyn ark_tools::Adapter>>,
        state: &mut ResearchState,
        metrics: &mut Metrics,
        on_event: &mut impl FnMut(HarnessEvent),
    ) -> Result<String, AgentError> {
        let a = &call.arguments;
        let sarg = |k: &str| a.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        match call.name.as_str() {
            catalog::LIST_ACTIONS => {
                let adapter = normalize_action_name(&sarg("adapter"));
                let adapter = adapter.split('.').next().unwrap_or(&adapter).to_string();
                let acts: Vec<_> = catalog::list_actions(&engine.registry, &adapter)
                    .into_iter()
                    .filter(|d| self.descriptor_available(d))
                    .collect();
                if acts.is_empty() {
                    return Ok(format!(
                        "no adapter named `{adapter}`. See the capability index."
                    ));
                }
                let mut s = format!("actions in {adapter}:\n");
                for d in acts {
                    let _ = writeln!(s, "  {} — {}", d.name, d.summary);
                }
                Ok(s)
            }
            catalog::LOAD_ACTION => {
                let name = normalize_action_name(&sarg("name"));
                match engine
                    .registry
                    .descriptor(&name)
                    .filter(|d| self.descriptor_available(d))
                {
                    Some(d) => {
                        if !loaded.contains(&name) {
                            loaded.push(name.clone());
                        }
                        Ok(format!("loaded `{name}` ({}). Its input schema is now available; call it directly.", d.summary))
                    }
                    None => Ok(format!(
                        "no action named `{name}`. Use list_actions to see an adapter's actions."
                    )),
                }
            }
            catalog::GET_EVIDENCE => {
                metrics.record_retrieval();
                let inv_ref = sarg("invocation");
                let q = crate::evidence::Query {
                    path: a.get("path").and_then(|v| v.as_str()),
                    matching: a.get("match").and_then(|v| v.as_str()),
                    offset: a.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
                    limit: a.get("limit").and_then(|v| v.as_u64()).unwrap_or(40) as usize,
                };
                on_event(HarnessEvent::Retrieval {
                    what: format!(
                        "get_evidence {inv_ref}{}{}",
                        q.path.map(|p| format!(".{p}")).unwrap_or_default(),
                        q.matching
                            .map(|m| format!(" match \"{m}\""))
                            .unwrap_or_default()
                    ),
                });
                match engine.store.resolve_invocation(project, &inv_ref)? {
                    None => Ok(format!("no invocation matching `{inv_ref}`.")),
                    Some(inv) => Ok(crate::evidence::extract(&inv.output, &q)),
                }
            }
            catalog::SEARCH_EVIDENCE => {
                metrics.record_retrieval();
                let query = sarg("query");
                let limit = a.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
                on_event(HarnessEvent::Retrieval {
                    what: format!("search_evidence \"{query}\""),
                });
                let hits = engine.store.search(project, &query, limit)?;
                if hits.is_empty() {
                    return Ok(format!("no stored file or document matches \"{query}\". This searches stored artifacts only: to get new data, load and call an action from the capability index; to search a result you already have, use get_evidence with match."));
                }
                let mut s = format!("{} match(es) for \"{query}\":\n", hits.len());
                for (i, h) in hits.iter().enumerate() {
                    let _ = writeln!(
                        s,
                        "  art:{} {} ({}) score {:.2}",
                        &h.artifact_id[..h.artifact_id.len().min(12)],
                        h.name,
                        h.kind,
                        h.score
                    );
                    // The lines that matched, for the best hits: often the answer itself, and
                    // otherwise what tells the model whether the file is worth opening.
                    if i < SNIPPET_HITS {
                        for line in matching_lines(engine, &h.artifact_id, &query) {
                            let _ = writeln!(s, "      > {line}");
                        }
                    }
                }
                Ok(s)
            }
            catalog::SEARCH_KNOWLEDGE => {
                metrics.record_retrieval();
                let query = sarg("query");
                let limit = a.get("limit").and_then(|v| v.as_u64()).unwrap_or(4) as usize;
                on_event(HarnessEvent::Retrieval {
                    what: format!("search_knowledge \"{query}\""),
                });
                match &profile.knowledge {
                    None => Ok("external knowledge is not configured for this agent.".into()),
                    Some(kb) => {
                        let hits = kb.search(&query, limit);
                        if hits.is_empty() {
                            return Ok(format!("no knowledge matched \"{query}\". This is background only; rely on gathered evidence."));
                        }
                        let mut s = format!("external knowledge for \"{query}\" (cite as shown; background, not observations):\n");
                        for h in hits {
                            let _ = writeln!(s, "{} {}", h.citation, h.snippet);
                        }
                        Ok(s)
                    }
                }
            }
            catalog::RECORD_STATE if sarg("kind") == "note" => {
                // A free-form note for the notebook; the working state is unchanged.
                let body = sarg("text");
                engine
                    .store
                    .put_note(project, &Note::new(NoteKind::Note, &body, Author::Agent))?;
                on_event(HarnessEvent::Note {
                    kind: "note".into(),
                    body,
                });
                Ok("recorded note.".into())
            }
            catalog::RECORD_STATE => {
                metrics.record_state_update();
                let kind = sarg("kind");
                let text = sarg("text");
                apply_state_update(state, &kind, &text, a);
                engine.store.put_research_state(project, state)?;
                on_event(HarnessEvent::StateUpdate {
                    kind: kind.clone(),
                    text: text.clone(),
                });
                Ok(format!("recorded {kind}."))
            }
            catalog::RECORD_NOTE => {
                let kind = sarg("kind");
                let body = sarg("body");
                let nk = note_kind_from_str(&kind);
                engine
                    .store
                    .put_note(project, &Note::new(nk, &body, Author::Agent))?;
                on_event(HarnessEvent::Note {
                    kind: kind.clone(),
                    body: body.clone(),
                });
                Ok(format!("recorded {kind} note."))
            }
            catalog::PRESENT => {
                let view = sarg("view");
                if view.is_empty() {
                    return Ok(
                        "error: `present` needs a `view` (file, artifact, evidence, …).".into(),
                    );
                }
                let opt = |k: &str| {
                    a.get(k)
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                };
                let directive = ark_core::PresentDirective {
                    view: view.clone(),
                    artifact: opt("artifact").map(ArtifactId),
                    path: opt("path"),
                    locator: opt("locator"),
                    pane: opt("pane"),
                    note: opt("note"),
                };
                let headline = directive.headline();
                on_event(HarnessEvent::Present { directive });
                Ok(format!("presented to the user: {headline}"))
            }
            // Capability acquisition — enabled only when the profile supplies a provider.
            catalog::FIND_CAPABILITY
            | catalog::PROPOSE_CAPABILITY
            | catalog::ACQUIRE_CAPABILITY => {
                if self.execution_mode == ExecutionMode::Plan
                    && call.name == catalog::ACQUIRE_CAPABILITY
                {
                    return Ok("unavailable in Plan: do not connect or install anything.".into());
                }
                match &profile.acquisition {
                    None => Ok("capability acquisition is not enabled for this agent.".into()),
                    Some(acq) => {
                        let need = sarg("need");
                        if need.is_empty() {
                            return Ok("error: this meta-tool needs a `need` (a capability slug or free-text description).".into());
                        }
                        let (which, out) = match call.name.as_str() {
                            catalog::FIND_CAPABILITY => ("find_capability", acq.find(&need).await),
                            catalog::PROPOSE_CAPABILITY => {
                                ("propose_capability", acq.propose(&need).await)
                            }
                            // Acquisition is gated by the same approval as a Mutate action. Its
                            // tools join this turn from the next step.
                            _ => {
                                // A switch the person turns on waits for them, whatever
                                // the agent may do without asking.
                                let approved = (self.allow_mutate && !acq.asks_a_person(&need))
                                    || self.approve_acquisition(profile, &need, on_event).await;
                                let got = acq.acquire(&need, approved).await;
                                added.extend(got.tools);
                                ("acquire_capability", got.summary)
                            }
                        };
                        on_event(HarnessEvent::Capability {
                            action: which.to_string(),
                            detail: out.clone(),
                        });
                        Ok(out)
                    }
                }
            }
            other => Ok(format!("error: unknown meta-tool `{other}`.")),
        }
    }

    /// One cheap-model call to draft a short plan. Returns its steps, or None on any failure.
    async fn draft_plan(
        &self,
        profile: &AgentProfile,
        question: &str,
        metrics: &mut Metrics,
        on_event: &mut impl FnMut(HarnessEvent),
    ) -> Option<Vec<String>> {
        let msgs = [Message::user(format!("Goal: {question}"))];
        let instructions = estimate_tokens(&profile.prompts.plan_system);
        let task = WorkingSet::message_tokens(&msgs[0]);
        on_event(HarnessEvent::Context {
            report: ContextReport {
                role: "plan".into(),
                instructions,
                task,
                total: instructions + task,
                budget: self.request_budget(),
                ..Default::default()
            },
        });
        let c = self
            .call_model(
                Role::Plan,
                &profile.prompts.plan_system,
                &msgs,
                &[],
                false,
                metrics,
                on_event,
                None,
                None,
            )
            .await
            .ok()??;
        let steps: Vec<String> = c
            .content
            .lines()
            .map(|l| l.trim().trim_start_matches(['-', '*', '•']).trim())
            .map(|l| {
                l.trim_start_matches(|ch: char| ch.is_ascii_digit() || ch == '.' || ch == ')')
                    .trim()
            })
            .filter(|l| l.len() > 3)
            .take(6)
            .map(str::to_string)
            .collect();
        Some(steps)
    }

    /// Ask the gate before acquiring a capability (it installs or connects something).
    async fn approve_acquisition(
        &self,
        profile: &AgentProfile,
        need: &str,
        on_event: &mut impl FnMut(HarnessEvent),
    ) -> bool {
        let desc = ark_core::ActionDescriptor::new(
            catalog::ACQUIRE_CAPABILITY,
            "acquire a new capability",
            RiskTier::Mutate,
        )
        // A yes connects, installs or turns on one thing once. No grant covers the next ask:
        // a capability the person turned off is asked for again (ADR 0045).
        .ask_each_time();
        let call = ark_model::ToolCall {
            id: String::new(),
            name: catalog::ACQUIRE_CAPABILITY.into(),
            arguments: json!({ "need": need }),
        };
        // Each need is its own question: a yes for one capability is not a yes for another.
        let mut profile = profile.clone();
        let base = profile.approval_classifier.take();
        profile.approval_classifier =
            Some(Arc::new(move |action: &str, input: &serde_json::Value| {
                let class = match &base {
                    Some(f) => f(action, input),
                    None => action.to_string(),
                };
                match input.get("need").and_then(|v| v.as_str()) {
                    Some(n) => format!("{class}:{}", n.trim().to_lowercase()),
                    None => class,
                }
            }));
        let Some(gate) = &self.approvals else {
            return false;
        };
        let request = ApprovalRequest {
            invocation: None,
            project: None,
            action: call.name.clone(),
            summary: desc.summary,
            risk: desc.risk,
            input: call.arguments.clone(),
            class: profile.approval_class(&call.name, &call.arguments),
            preview: preview_call(&call.name, &call.arguments),
            agent: None,
            task: None,
            ask_each_time: true,
            cut_short: false,
        };
        on_event(HarnessEvent::ApprovalRequested {
            request: request.clone(),
        });
        let Some(decision) = self.cancellable(gate.check(&request)).await else {
            return false;
        };
        on_event(HarnessEvent::ApprovalResolved {
            action: request.action,
            class: request.class,
            decision,
        });
        decision.allows()
    }

    #[allow(clippy::too_many_arguments)]
    async fn force_answer(
        &self,
        profile: &AgentProfile,
        ws: &mut WorkingSet,
        sys: &SystemPrompt,
        (first, first_tokens, retrieved, loaded, step): (&FirstTurn, u32, &[u32], usize, usize),
        state: &ResearchState,
        metrics: &mut Metrics,
        on_event: &mut impl FnMut(HarnessEvent),
        memory: &crate::memory::Snapshot<'_>,
    ) -> Result<Option<String>, AgentError> {
        // Same system prompt and rounds as the reasoning steps, so the cached prefix carries
        // over; only the state and the instruction to stop differ.
        let system = sys.render();
        let digest = state.digest(8);
        let tail = state_message(&digest);
        let stop = Message::user(&profile.prompts.force_answer);
        let reserve = WorkingSet::message_tokens(&tail) + WorkingSet::message_tokens(&stop);
        let (mut messages, est) = ws.assemble_with(&system, reserve);
        metrics.peak_prompt_tokens = metrics.peak_prompt_tokens.max(est);
        let mut report = self.report(
            profile,
            sys,
            &digest,
            first,
            first_tokens,
            ws,
            retrieved,
            est,
            &[],
            loaded,
            step,
        );
        report.role = "synthesize".into();
        report.task += WorkingSet::message_tokens(&stop);
        report.tool_rounds = report
            .tool_rounds
            .saturating_sub(WorkingSet::message_tokens(&stop));
        on_event(HarnessEvent::Context { report });
        messages.push(tail);
        messages.push(stop);
        Ok(self
            .call_model(
                Role::Synthesize,
                &system,
                &messages,
                &[],
                true,
                metrics,
                on_event,
                None,
                Some(memory),
            )
            .await?
            .map(|c| c.content.trim().to_string()))
    }

    /// Deterministic checks, no model call: did we answer, and is a factual answer grounded in
    /// cited evidence? Returns (passed, human detail).
    fn self_check(
        &self,
        answer: &str,
        cited: &[InvocationId],
        trivial: bool,
        steps: usize,
    ) -> (bool, String) {
        if answer.is_empty() {
            return (false, "no answer produced".into());
        }
        let admits_unknown = [
            "unknown",
            "cannot determine",
            "not enough",
            "no evidence",
            "unable to",
        ]
        .iter()
        .any(|p| answer.to_ascii_lowercase().contains(p));
        let cites_something = answer.contains("inv:") || !cited.is_empty();
        if !trivial && !cites_something && !admits_unknown && steps > 1 {
            return (
                false,
                "answer is ungrounded: no evidence cited and no uncertainty acknowledged".into(),
            );
        }
        (
            true,
            format!(
                "answered; {} evidence {} cited",
                cited.len(),
                if cited.len() == 1 { "item" } else { "items" }
            ),
        )
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_critic(
        &self,
        profile: &AgentProfile,
        system: &str,
        state: &ResearchState,
        answer: &str,
        issue: &str,
        metrics: &mut Metrics,
        on_event: &mut impl FnMut(HarnessEvent),
    ) -> Option<String> {
        let sys = format!(
            "{system}\n\n{} (the check that failed: {issue})",
            profile.prompts.critic_intro
        );
        let digest = state.digest(8);
        let msgs = [Message::user(format!(
            "== WORKING STATE ==\n{digest}\n\n== DRAFT ANSWER ==\n{answer}"
        ))];
        let system_tokens = estimate_tokens(system);
        let instructions = estimate_tokens(&profile.prompts.instructions);
        let review = estimate_tokens(&sys).saturating_sub(system_tokens);
        let working_state = estimate_tokens(&digest);
        let draft = WorkingSet::message_tokens(&msgs[0]).saturating_sub(working_state);
        on_event(HarnessEvent::Context {
            report: ContextReport {
                role: "critic".into(),
                instructions,
                project: system_tokens.saturating_sub(instructions),
                working_state,
                model_output: draft,
                task: review,
                total: estimate_tokens(&sys) + WorkingSet::message_tokens(&msgs[0]),
                budget: self.request_budget(),
                ..Default::default()
            },
        });
        let c = self
            .call_model(
                Role::Critic,
                &sys,
                &msgs,
                &[],
                false,
                metrics,
                on_event,
                None,
                None,
            )
            .await
            .ok()??;
        let out = c.content.trim().to_string();
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }

    fn build_workspace_context(
        &self,
        engine: &Engine,
        project: &ProjectId,
    ) -> Result<WorkspaceContext, AgentError> {
        let label = engine
            .store
            .get_project(project)?
            .map(|p| {
                if p.target.is_empty() {
                    p.name.clone()
                } else {
                    format!("{} — {}", p.name, p.target)
                }
            })
            .unwrap_or_default();

        let mut ctx = WorkspaceContext::new(label).with_focus(self.focus.clone());

        if let Some(a) = self
            .focus
            .as_ref()
            .and_then(|f| f.surface.as_ref())
            .and_then(|s| s.artifact.clone())
        {
            ctx.artifact = Some(a);
        }

        if let Some(aid) = ctx.active_artifact().cloned() {
            let terms = selection_terms(self.focus.as_ref());
            ctx.nearby_evidence = gather_nearby_evidence(engine, project, &aid, &terms, 8)?;
        }
        Ok(ctx)
    }

    /// The system prompt for a turn: the profile's instructions and rules, then the capability
    /// index. It names no project and no task, so it stays byte-identical across turns, chats
    /// and projects of one profile, and a provider's prefix cache can serve it.
    async fn system_prompt(&self, engine: &Engine, profile: &AgentProfile) -> SystemPrompt {
        let rules = format!(
            "{}\n\nActions that change state need explicit human approval; observation and analysis are free. Reason from what tools return. For an action you are allowed to propose, call it to request any required approval. The host handles approval before execution. Do not substitute a text-only permission request.",
            profile.prompts.instructions
        );
        let index = crate::capmap::capability_index(
            &engine.registry,
            &profile.capabilities,
            &profile.inferrer,
        )
        .await;
        SystemPrompt { rules, index }
    }

    /// The project's name, target and newest artifacts, for the first message.
    fn project_block(&self, engine: &Engine, project: &ProjectId) -> Result<String, AgentError> {
        let p = engine
            .store
            .get_project(project)?
            .ok_or_else(|| AgentError::ProjectNotFound(project.to_string()))?;
        let artifacts = engine.store.list_artifacts(project)?;
        let mut project_block = String::new();
        let _ = writeln!(project_block, "Project: {} — {}", p.name, p.target);
        if artifacts.is_empty() {
            let _ = write!(
                project_block,
                "Artifacts: none yet (ingest a file or capture one first)."
            );
        } else {
            let _ = writeln!(
                project_block,
                "Artifacts, newest first (pass `art:<id>` as the `artifact` argument):"
            );
            for a in artifacts.iter().take(ARTIFACTS_LISTED) {
                let _ = writeln!(
                    project_block,
                    "  - art:{} {} ({})",
                    a.id.short(),
                    a.name,
                    a.kind.as_tag()
                );
            }
            if artifacts.len() > ARTIFACTS_LISTED {
                let _ = write!(
                    project_block,
                    "  (+{} more; search_evidence finds them by content or name)",
                    artifacts.len() - ARTIFACTS_LISTED
                );
            }
        }
        Ok(project_block.trim_end().to_string())
    }
}

/// Independent read-only calls from one model reply that run at the same time.
pub const MAX_PARALLEL_TOOLS: usize = 4;

/// An adapter call waiting to run in the current batch.
struct ApprovalContext<'a> {
    engine: &'a Engine,
    project: &'a ProjectId,
    profile: &'a AgentProfile,
}

enum ApprovalReply {
    Cancelled,
    Unavailable,
    Decided {
        decision: ApprovalDecision,
        invocation: Box<Invocation>,
    },
}

struct Pending {
    call: ark_model::ToolCall,
    risk: RiskTier,
    approved: bool,
    invocation: Option<Invocation>,
    failed_before: bool,
    digest: String,
}

/// The per-turn tallies a finished call updates.
struct Tally<'a> {
    metrics: &'a mut Metrics,
    cited: &'a mut Vec<InvocationId>,
    repeated_failures: &'a mut u32,
    failed_runs: &'a mut HashMap<String, u32>,
    changed_at: &'a mut time::OffsetDateTime,
    /// Files of the project folder the turn's calls wrote (relative paths).
    written: &'a mut Vec<String>,
}

/// Artifacts named in the first message. Older ones stay reachable through search.
const ARTIFACTS_LISTED: usize = 12;

/// The system prompt, by part. It names no project, so it is the same for every turn of a
/// profile and a provider's prefix cache can serve it across turns, chats and projects.
struct SystemPrompt {
    /// The profile's instructions plus the runtime's fixed rules.
    rules: String,
    index: String,
}

impl SystemPrompt {
    fn render(&self) -> String {
        format!("{}\n\n{}", self.rules, self.index)
    }
}

/// The working state, sent as the last message of every request. It changes as the agent
/// records findings; keeping it at the end leaves everything before it byte-identical between
/// steps, which is what a prefix cache needs.
fn state_message(digest: &str) -> Message {
    state_message_with(digest, &[])
}

/// Messages the person sent while the turn ran that the working state repeats, newest kept, and
/// the characters of each.
const STEERED_CARRIED: usize = 5;
const STEERED_CHARS: usize = 300;

/// The working state with what the person said while the turn ran: a steering message also
/// sits in a round, which can be dropped; here it stays (ADR 0050).
fn state_message_with(digest: &str, steered: &[String]) -> Message {
    use std::fmt::Write as _;
    let mut s = format!("== WORKING STATE ==\n{digest}");
    if !steered.is_empty() {
        s.push_str("\nThe person also said while you worked (newest last):");
        let older = steered.len().saturating_sub(STEERED_CARRIED);
        if older > 0 {
            let _ = write!(s, "\n  (+{older} earlier)");
        }
        for m in &steered[older..] {
            let flat = m.split_whitespace().collect::<Vec<_>>().join(" ");
            let clipped: String = flat.chars().take(STEERED_CHARS).collect();
            let more = if flat.chars().count() > STEERED_CHARS {
                "…"
            } else {
                ""
            };
            let _ = write!(s, "\n  - {clipped}{more}");
        }
    }
    Message::user(s)
}

impl Harness {
    /// The loaded actions as lines for the first message, when `ready_list` is on.
    fn ready_block(&self, engine: &Engine, loaded: &[String]) -> Option<String> {
        if !self.ready_list || loaded.is_empty() {
            return None;
        }
        let mut s = String::from("Already loaded; call these directly, without load_action:");
        for name in loaded {
            if let Some(d) = engine.registry.descriptor(name) {
                let _ = write!(s, "\n- {name}: {}", d.summary);
            }
        }
        Some(s)
    }

    fn descriptor_available(&self, descriptor: &ark_core::ActionDescriptor) -> bool {
        self.execution_mode != ExecutionMode::Plan
            || descriptor.risk != RiskTier::Mutate
            || descriptor.stays_in_plan
    }

    fn action_available(&self, engine: &Engine, name: &str) -> bool {
        engine
            .registry
            .descriptor(name)
            .is_some_and(|descriptor| self.descriptor_available(&descriptor))
    }
}

/// The profile's core actions that exist in the registry, in the profile's order.
/// Whether the turn is offered the acquisition tools: a provider with something to acquire.
fn acquisition_on(profile: &AgentProfile) -> bool {
    profile.acquisition.as_ref().is_some_and(|a| a.active())
}

fn can_add(profile: &AgentProfile) -> Option<String> {
    profile
        .acquisition
        .as_ref()
        .filter(|a| a.active())
        .and_then(|a| a.offer())
        .filter(|t| !t.trim().is_empty())
}

fn core_loaded(engine: &Engine, profile: &AgentProfile) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for a in &profile.core_actions {
        if engine.registry.descriptor(a).is_some() && !v.contains(a) {
            v.push(a.clone());
        }
    }
    v
}

/// Files an answer says were saved that no call of this turn wrote. A claim is a file name
/// (`report.md`, `out/chart.png`) in a sentence that says something was saved, written or
/// created and does not say it was not. A name counts as written when a call wrote a file of
/// that name, or when the file in `workspace` changed since `since` (a run lists only so many
/// of the files it wrote). Deterministic and English only; a miss costs nothing, a false
/// alarm one more reply.
fn unwritten_claims(
    text: &str,
    written: &[String],
    workspace: impl FnOnce() -> Option<std::path::PathBuf>,
    since: time::OffsetDateTime,
) -> Vec<String> {
    let claims = claimed_files(text);
    if claims.is_empty() {
        return claims;
    }
    let workspace = workspace();
    let since = std::time::SystemTime::from(since);
    claims
        .into_iter()
        .filter(|claim| {
            let name = file_name(claim);
            if written.iter().any(|w| file_name(w) == name) {
                return false;
            }
            let changed = workspace
                .as_ref()
                .and_then(|dir| std::fs::metadata(dir.join(claim)).ok())
                .and_then(|m| m.modified().ok())
                .is_some_and(|t| t >= since);
            !changed
        })
        .collect()
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// File names in sentences that say something was saved, written or created, in order, once
/// each. A name that follows "from" or "using" is an input, not a claim.
fn claimed_files(text: &str) -> Vec<String> {
    const SAYS_SAVED: &[&str] = &[
        "saved",
        "wrote",
        "written",
        "created",
        "exported",
        "generated",
        "stored",
        "produced",
    ];
    const SAYS_NOT: &[&str] = &[
        "not", "cannot", "unable", "failed", "fail", "couldn't", "didn't", "wasn't", "won't",
        "isn't", "hasn't", "haven't",
    ];
    const INPUT: &[&str] = &["from", "using"];
    let mut claims: Vec<String> = Vec::new();
    for sentence in sentences(text) {
        let words: Vec<String> = sentence
            .split(|c: char| c.is_whitespace() || "`*_\"'()[]<>,;|".contains(c))
            .map(|w| w.trim_end_matches(['.', ':', '!', '?']).to_string())
            .filter(|w| !w.is_empty())
            .collect();
        let lower: Vec<String> = words.iter().map(|w| w.to_lowercase()).collect();
        let says = |set: &[&str]| lower.iter().any(|w| set.contains(&w.as_str()));
        if !says(SAYS_SAVED) || says(SAYS_NOT) {
            continue;
        }
        for (i, w) in words.iter().enumerate() {
            let input = i > 0 && INPUT.contains(&lower[i - 1].as_str());
            if !input && looks_like_file(w) && !claims.contains(w) {
                claims.push(w.clone());
            }
        }
    }
    claims
}

/// Sentences and lines of `text`: split at a line break, or at `.`, `!` or `?` followed by
/// a space (a file name's dot is not). A list item belongs to the line before the list when
/// that line ends with `:` ("Saved these files:" then "- report.md").
fn sentences(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut lead: Option<String> = None;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let item = line.trim_start();
        let is_item = item.starts_with(['-', '*', '+'])
            || item
                .split_once(". ")
                .is_some_and(|(n, _)| n.chars().all(|c| c.is_ascii_digit()));
        match &lead {
            Some(l) if is_item => {
                out.push(format!("{l} {item}"));
                continue;
            }
            _ => lead = None,
        }
        let mut start = 0;
        let bytes = line.as_bytes();
        for i in 0..bytes.len() {
            if matches!(bytes[i], b'.' | b'!' | b'?') && bytes.get(i + 1) == Some(&b' ') {
                out.push(line[start..=i].to_string());
                start = i + 1;
            }
        }
        let last = line[start..].trim_end();
        if last.ends_with(':') {
            lead = Some(last.to_string());
        }
        out.push(last.to_string());
    }
    out
}

/// A relative or absolute file name with an extension: `report.md`, `out/chart.png`. Not a
/// web address, a version number, an abbreviation (`e.g`) or a domain (`example.com`).
fn looks_like_file(word: &str) -> bool {
    const DOMAINS: &[&str] = &[
        "com", "org", "net", "io", "dev", "ai", "app", "co", "edu", "gov",
    ];
    if word.contains("://") || word.starts_with("www.") || word.contains('@') {
        return false;
    }
    let Some((stem, ext)) = word.rsplit_once('.') else {
        return false;
    };
    let stem_name = file_name(stem);
    (2..=5).contains(&ext.len())
        && ext.chars().all(|c| c.is_ascii_alphanumeric())
        && ext.starts_with(|c: char| c.is_ascii_alphabetic())
        && !DOMAINS.contains(&ext.to_lowercase().as_str())
        && stem_name.chars().any(|c| c.is_alphanumeric())
}

/// What the model is told when its answer names files no call wrote.
fn unwritten_note(missing: &[String], written: &[String]) -> String {
    let (names, it) = if missing.len() == 1 {
        (missing[0].clone(), "it")
    } else {
        (missing.join(", "), "them")
    };
    let wrote = if written.is_empty() {
        "This turn wrote no files.".to_string()
    } else {
        let mut w = written.to_vec();
        w.dedup();
        format!("This turn wrote: {}.", w.join(", "))
    };
    format!(
        "Your answer says {names} was saved, but no tool call in this turn wrote {it}. {wrote} Save {it} now with a tool, or reply with an answer that names only the files that were written."
    )
}

/// Runs of one identical state-changing call in a task: the first and one retry.
pub const MAX_IDENTICAL_CHANGES: u32 = 2;

/// Runs of one identical failed read or observation in a turn: the first and one retry.
const MAX_IDENTICAL_FAILURES: u32 = 2;

/// Replies that could not be used the model is asked to send again: in a row, and in a turn.
const MAX_REPAIRS_IN_ROW: u32 = 2;
const MAX_REPAIRS: u32 = 4;
const REPAIR_NOTE: &str = "Your last reply could not be used, so nothing ran. Send it again with valid arguments for the tool you meant to call. Put long text in several smaller edits instead of one large one.";

const DONE_BEFORE_NOTE: &str = "not run again: this exact change already ran before the work was interrupted, so running it again would make it twice. Its result";
const CUT_SHORT_NOTE: &str = "not run: this exact change was under way when the work was interrupted, so it may already have happened, and only the person can tell. Do not try it again; say in your answer that it was cut short and may need to be checked.";
const REPEATED_CHANGE_NOTE: &str = "not run: this exact call already ran twice in this task, with the results above. Running it again gives the same result. Change the input to fix what went wrong (for a program, fix the error it reported), or answer from what you have.";
const REPEATED_FAILURE_NOTE: &str = "not run: this exact call already failed twice in this turn. ARK already gave it one retry. Use a different tool or input, or answer from what you know.";

const ANNOUNCED_NOTE: &str = "Your reply says what you will do next, but it called no tool, so nothing was done and the turn would end here. Do it now by calling the tool. If the work is already done or cannot be done, reply with the final answer only.";

/// Text at least this long, over at least [`ANSWER_LINES`] lines, reads as a full answer rather
/// than a note on the next step. From the owner's run (ADR 0051): notes beside tool calls were
/// 39 to 205 characters on one line, the answers 1,431 to 1,798 over 13 to 25 lines.
const ANSWER_CHARS: usize = 600;
const ANSWER_LINES: usize = 3;
/// Share of the shorter answer's words the next one repeats when it restates it. The owner's
/// run: 0.61 to 0.67 between consecutive restated answers.
const RESTATED: f32 = 0.5;

/// Whether a reply is a full answer written beside reads of files this turn already changed
/// (or folders holding them), and nothing else: the model checking its own work.
fn rechecks(c: &Completion, engine: &Engine, written: &[String]) -> bool {
    let text = c.content.trim();
    if c.tool_calls.is_empty()
        || written.is_empty()
        || text.chars().count() < ANSWER_CHARS
        || text.lines().filter(|l| !l.trim().is_empty()).count() < ANSWER_LINES
    {
        return false;
    }
    let norm = |p: &str| {
        p.trim()
            .replace('\\', "/")
            .trim_start_matches("./")
            .trim_end_matches('/')
            .to_string()
    };
    let written: Vec<String> = written.iter().map(|w| norm(w)).collect();
    c.tool_calls.iter().all(|call| {
        let read_only = !catalog::is_meta_tool(&call.name)
            && engine
                .registry
                .descriptor(&call.name)
                .is_some_and(|d| d.risk != RiskTier::Mutate);
        let Some(path) = call.arguments.get("path").and_then(|p| p.as_str()) else {
            return false;
        };
        let path = norm(path);
        read_only
            && !path.is_empty()
            && path != "."
            && written
                .iter()
                .any(|w| *w == path || w.starts_with(&format!("{path}/")))
    })
}

/// Whether `now` repeats most of the words of `before`: the same answer said again.
fn restates(before: &str, now: &str) -> bool {
    let words = |t: &str| -> HashSet<String> {
        t.split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.chars().count() >= 3)
            .map(str::to_lowercase)
            .collect()
    };
    let (a, b) = (words(before), words(now));
    let smaller = a.len().min(b.len());
    smaller > 0 && a.intersection(&b).count() as f32 / smaller as f32 >= RESTATED
}

/// Whether the end of an answer announces work the model is about to do ("I will now call
/// `files__edit`", "Let me now update the file"), the way a model ends a reply instead of making
/// the call. Only the last three sentences count, and a sentence that waits on the person
/// ("if you confirm", a question) does not. Deterministic and English only; a miss costs
/// nothing, a false alarm one more reply.
fn announces_work(text: &str) -> bool {
    const WILL: &[&str] = &[
        "i will now",
        "i'll now",
        "now i will",
        "now i'll",
        "let me now",
        "i will proceed",
        "i'll proceed",
        "i will call",
        "i'll call",
        "i will execute",
        "i will perform",
        "i will run",
        "i'll run",
        "i am going to",
        "i'm going to",
    ];
    const WAITS: &[&str] = &[
        "?",
        "if you",
        "once you",
        "when you",
        "you'd like",
        "you want",
        "confirm",
        "wait",
        "approval",
    ];
    let all = sentences(text);
    all.iter().rev().take(3).any(|sentence| {
        let s = sentence.to_lowercase().replace('\u{2019}', "'");
        WILL.iter().any(|w| s.contains(w)) && !WAITS.iter().any(|w| s.contains(w))
    })
}

/// Whether `text` contains a call to one of `tools` written out as text (`record_state({`,
/// `fs__read(`), the way a model sometimes "calls" a tool without calling it.
fn writes_tool_call_as_text(text: &str, tools: &[ToolDef]) -> bool {
    tools.iter().any(|t| {
        [t.name.clone(), ark_model::mangle_tool_name(&t.name)]
            .iter()
            .any(|name| {
                text.match_indices(&format!("{name}(")).any(|(i, m)| {
                    let before = text[..i].chars().last();
                    let after = text[i + m.len()..].trim_start();
                    !before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.')
                        && (after.starts_with('{') || after.starts_with(')'))
                })
            })
    })
}

/// Search hits that show their matching lines, and lines shown per hit.
const SNIPPET_HITS: usize = 5;
const SNIPPET_LINES: usize = 2;

/// Up to [`SNIPPET_LINES`] lines of a stored text artifact that contain a word of `query`
/// (three letters or more, any case), clipped. Empty for binary or very large artifacts.
fn matching_lines(engine: &Engine, artifact: &str, query: &str) -> Vec<String> {
    let terms: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= 3)
        .map(|t| t.to_lowercase())
        .collect();
    if terms.is_empty() {
        return Vec::new();
    }
    let Ok(bytes) = engine.store.read_blob(&ArtifactId(artifact.to_string())) else {
        return Vec::new();
    };
    if bytes.len() > 8 * 1024 * 1024 {
        return Vec::new();
    }
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Vec::new();
    };
    // Lines with the most query words first, then in file order.
    let mut scored: Vec<(usize, usize, &str)> = text
        .lines()
        .enumerate()
        .filter_map(|(n, line)| {
            let l = line.to_lowercase();
            let hits = terms.iter().filter(|t| l.contains(t.as_str())).count();
            (hits > 0).then_some((hits, n, line))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored
        .into_iter()
        .take(SNIPPET_LINES)
        .map(|(_, _, line)| {
            let t = line.trim();
            if t.chars().count() > 200 {
                format!("{}…", t.chars().take(200).collect::<String>())
            } else {
                t.to_string()
            }
        })
        .collect()
}

/// Runtime-handled calls whose first use in a turn is progress: they bring information in or
/// record it. Discovery calls (`list_actions`, `load_action`) count through the loaded set.
fn counts_as_progress(name: &str) -> bool {
    catalog::is_retrieval(name) || matches!(name, catalog::RECORD_STATE | catalog::RECORD_NOTE)
}

/// Whether an `Auto` call's result from an earlier turn still holds: a `Read` of a stored
/// artifact, whose bytes are content-addressed and cannot change. Workspace files and external
/// systems change between turns, and an `Observe` call depends on more than its artifact.
fn reads_stored_artifact(
    engine: &Engine,
    project: &ProjectId,
    desc: &ark_core::ActionDescriptor,
    input: &serde_json::Value,
) -> bool {
    if desc.risk != RiskTier::Read {
        return false;
    }
    match input.get("artifact").and_then(|a| a.as_str()) {
        Some(id) => engine
            .store
            .resolve_artifact(project, id)
            .ok()
            .flatten()
            .is_some(),
        None => false,
    }
}

/// Normalize an action-name argument to its canonical dotted form. Providers that forbid `.`
/// in tool names present actions as `ns__verb`; a model may echo that mangled form back as a
/// string argument. Restoring the dot keeps namespaced actions discoverable.
fn normalize_action_name(name: &str) -> String {
    let n = name.trim();
    if n.contains('.') {
        n.to_string()
    } else {
        ark_model::unmangle_tool_name(n)
    }
}

/// The distinctive terms a selection contributes, used to match nearby evidence cards.
fn selection_terms(focus: Option<&Focus>) -> Vec<String> {
    let mut terms = Vec::new();
    if let Some(sel) = focus.and_then(|f| f.selection.as_ref()) {
        for (_, v) in sel.named() {
            let v = v.trim();
            if v.len() >= 2 {
                terms.push(v.to_ascii_lowercase());
            }
        }
    }
    terms
}

/// Compact reference lines for evidence near a focus. Most recent first, de-duplicated,
/// capped at `max`.
fn gather_nearby_evidence(
    engine: &Engine,
    project: &ProjectId,
    artifact: &ArtifactId,
    terms: &[String],
    max: usize,
) -> Result<Vec<String>, AgentError> {
    let mut touched: HashSet<String> = HashSet::new();
    for inv in engine.store.list_invocations(project)? {
        if inv
            .consumed
            .iter()
            .chain(inv.produced.iter())
            .any(|a| a == artifact)
        {
            touched.insert(inv.id.as_str().to_string());
        }
    }

    let mut cards = engine.store.list_evidence_cards(project)?;
    cards.sort_by_key(|c| std::cmp::Reverse(c.created_at));

    let relevant = |c: &EvidenceCard| -> bool {
        if touched.contains(c.invocation.as_str()) {
            return true;
        }
        if terms.is_empty() {
            return false;
        }
        c.identifiers.iter().any(|id| {
            let idl = id.to_ascii_lowercase();
            terms.iter().any(|t| idl.contains(t))
        })
    };

    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for c in cards.iter().filter(|c| relevant(c)) {
        if seen.insert(c.invocation.as_str().to_string()) {
            out.push(c.reference_line());
            if out.len() >= max {
                break;
            }
        }
    }
    Ok(out)
}

/// Deterministic classifier: is this a trivial, single-step question? Short, single-clause
/// questions skip planning; anything with a configured investigation marker is treated as an
/// investigation. No model call.
fn classify_trivial(q: &str, markers: &[String]) -> bool {
    let ql = q.to_ascii_lowercase();
    let words = ql.split_whitespace().count();
    let multi = markers.iter().any(|m| ql.contains(m.as_str()));
    words <= 14 && !multi
}

fn card_for_model(card: &EvidenceCard) -> String {
    let mut s = format!(
        "[inv:{}] {}: {}",
        card.invocation.short(),
        card.action,
        card.summary
    );
    let ids: Vec<&String> = card.identifiers.iter().filter(|i| !i.is_empty()).collect();
    if !ids.is_empty() {
        let shown: Vec<&String> = ids.into_iter().take(12).collect();
        let _ = write!(
            s,
            "\n  identifiers: {}",
            shown
                .iter()
                .map(|x| x.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !card.produced.is_empty() {
        let _ = write!(
            s,
            "\n  produced: {}",
            card.produced
                .iter()
                .map(|p| format!("art:{}", p.short()))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let _ = write!(
        s,
        "\n  (get_evidence(\"inv:{}\") reads exact rows; add match to filter)",
        card.invocation.short()
    );
    s
}

fn apply_state_update(state: &mut ResearchState, kind: &str, text: &str, args: &serde_json::Value) {
    let cites: Vec<String> = args
        .get("cites")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    match kind {
        "finding" => {
            let ct = args
                .get("claim_type")
                .and_then(|v| v.as_str())
                .map(ClaimType::from_str_lenient)
                .unwrap_or(ClaimType::Inference);
            let mut c = Claim::new(text, ct);
            c.cites = cites.clone();
            state.findings.push(c);
            let f = state.graph.add_node(ark_core::NodeKind::Finding, text);
            for cite in &cites {
                let e = state
                    .graph
                    .add_node(ark_core::NodeKind::Evidence, cite.clone());
                state.graph.link(&e, &f, ark_core::EdgeRel::Supports);
            }
        }
        "hypothesis" => {
            state
                .hypotheses
                .push(Claim::new(text, ClaimType::Hypothesis));
            state.graph.add_node(ark_core::NodeKind::Hypothesis, text);
        }
        "reject" => {
            state.rejected.push(text.to_string());
            state.hypotheses.retain(|h| h.text != text);
            if let Some(id) = state
                .graph
                .nodes
                .iter()
                .find(|n| {
                    n.kind == ark_core::NodeKind::Hypothesis && n.label.eq_ignore_ascii_case(text)
                })
                .map(|n| n.id.clone())
            {
                state
                    .graph
                    .link("rejected", &id, ark_core::EdgeRel::Contradicts);
            }
        }
        "interesting" => state.interesting.push(Interesting {
            kind: args
                .get("claim_type")
                .and_then(|v| v.as_str())
                .unwrap_or("item")
                .to_string(),
            value: text.to_string(),
            why: String::new(),
        }),
        "question" => {
            state.questions.push(text.to_string());
            state.graph.add_node(ark_core::NodeKind::Question, text);
        }
        "next_action" => {
            state.next_actions.push(text.to_string());
            state.graph.add_node(ark_core::NodeKind::Experiment, text);
        }
        "experiment" => {
            let x = state.graph.add_node(ark_core::NodeKind::Experiment, text);
            if let Some(target) = args
                .get("tests")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                state.graph.link(&x, target, ark_core::EdgeRel::TestedBy);
            }
            for cite in &cites {
                let e = state
                    .graph
                    .add_node(ark_core::NodeKind::Evidence, cite.clone());
                state.graph.link(&x, &e, ark_core::EdgeRel::DerivedFrom);
            }
        }
        "link" => {
            let f = args.get("from").and_then(|v| v.as_str()).unwrap_or(text);
            let t = args.get("to").and_then(|v| v.as_str()).unwrap_or("");
            let rel = args
                .get("rel")
                .and_then(|v| v.as_str())
                .and_then(ark_core::EdgeRel::from_str_lenient)
                .unwrap_or(ark_core::EdgeRel::Unresolved);
            state.graph.link(f, t, rel);
        }
        _ => state.questions.push(text.to_string()),
    }
    state.touch();
}

fn note_kind_from_str(s: &str) -> NoteKind {
    match s {
        "hypothesis" => NoteKind::Hypothesis,
        "assumption" => NoteKind::Assumption,
        "fact" => NoteKind::Fact,
        "finding" => NoteKind::Finding,
        "contradiction" => NoteKind::Contradiction,
        "next_step" => NoteKind::NextStep,
        "question" => NoteKind::Question,
        _ => NoteKind::Note,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_action_name_restores_mangled_form() {
        assert_eq!(normalize_action_name("fs__read"), "fs.read");
        assert_eq!(
            normalize_action_name("fs.read"),
            "fs.read",
            "already-dotted is untouched"
        );
        assert_eq!(
            normalize_action_name("  web__get "),
            "web.get",
            "trimmed and unmangled"
        );
        assert_eq!(
            normalize_action_name("strings"),
            "strings",
            "a bare adapter name is fine"
        );
    }

    #[test]
    fn files_an_answer_says_were_saved_are_found() {
        let claims = |t: &str| claimed_files(t);
        assert_eq!(
            claims("Saved the chart as chart.png and the report as `out/report.md`."),
            vec!["chart.png", "out/report.md"]
        );
        // Seen live (qwen3:4b): the failure is admitted, so only the chart is claimed.
        assert_eq!(
            claims("The line chart has been created and saved as chart.png.  \n\nThe report.md could not be saved due to a folder busy error."),
            vec!["chart.png"]
        );
        assert_eq!(
            claims("Files written:\n\n- [report](report.md)\n- data/sales.csv\n\nDone."),
            vec!["report.md", "data/sales.csv"]
        );
        assert_eq!(
            claims("I created summary.xlsx from sales.csv using clean.py."),
            vec!["summary.xlsx"],
            "inputs are not claims"
        );
        assert!(
            claims("Read report.md: sales rose 25%.").is_empty(),
            "no save"
        );
        assert!(
            claims("I wrote it up, e.g. the trend, see https://example.com/a.md and v0.10.0, on example.com.").is_empty(),
            "abbreviations, addresses, versions and domains are not files"
        );
    }

    #[test]
    fn an_answer_that_announces_work_is_sent_back() {
        // Endings seen on qwen3:4b (`files-edit`, `bench/RESULTS.md`).
        assert!(announces_work(
            "The file has no Revenue column yet.\n\nI will now call `files__edit` to add the Revenue column and the Total row."
        ));
        assert!(announces_work(
            "- Updated the file.\n\n**Next step**: Verify it. I will now read the updated file to confirm the changes.\n\nI will call `files__read` to inspect it."
        ));
        assert!(announces_work(
            "Found the data. Let me now update the sheet."
        ));
        // Answers that are done, wait on the person, or announce something far from the end.
        assert!(!announces_work(
            "Done! I've updated sales.xlsx with a Revenue column and a Total row."
        ));
        assert!(!announces_work(
            "Next step: Verify the updated spreadsheet contains the correct values."
        ));
        assert!(!announces_work(
            "I will now update the file if you confirm the prices."
        ));
        assert!(!announces_work(
            "Shall I proceed? I will now wait for your answer."
        ));
        assert!(!announces_work(
            "I will now explain. First point. Second point. Third point. That is all."
        ));
    }

    #[test]
    fn a_claim_counts_as_written_by_a_call_or_by_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let start = time::OffsetDateTime::now_utc() - time::Duration::seconds(5);
        std::fs::write(dir.path().join("extra.csv"), "a").unwrap();
        let text = "Saved report.md, out/chart.png and extra.csv.";
        let written = vec!["out/report.md".to_string()];
        let ws = || Some(dir.path().to_path_buf());
        assert_eq!(
            unwritten_claims(text, &written, ws, start),
            vec!["out/chart.png"],
            "report.md was written by a call, extra.csv changed in the folder this turn"
        );
        let later = time::OffsetDateTime::now_utc() + time::Duration::seconds(60);
        assert_eq!(
            unwritten_claims("Saved extra.csv.", &[], ws, later),
            vec!["extra.csv"],
            "a file older than the turn was not written by it"
        );
        let mut asked = false;
        assert!(unwritten_claims(
            "Hello.",
            &[],
            || {
                asked = true;
                None
            },
            start
        )
        .is_empty());
        assert!(!asked, "no claim, no look at the folder");
        assert!(unwritten_note(&["report.md".into()], &["chart.png".into()])
            .contains("no tool call in this turn wrote it. This turn wrote: chart.png."));
    }

    #[test]
    fn tool_calls_written_as_text_are_recognized() {
        let tools = vec![ToolDef {
            name: "record_state".into(),
            description: String::new(),
            parameters: json!({}),
        }];
        assert!(writes_tool_call_as_text(
            r#"record_state({ "kind": "finding" })"#,
            &tools
        ));
        assert!(writes_tool_call_as_text(
            "Done.\nrecord_state( {\"kind\": 1})",
            &tools
        ));
        assert!(!writes_tool_call_as_text("I recorded the state.", &tools));
        assert!(!writes_tool_call_as_text("see my_record_state({})", &tools));
        let dotted = vec![ToolDef {
            name: "fs.read".into(),
            description: String::new(),
            parameters: json!({}),
        }];
        assert!(writes_tool_call_as_text(
            r#"fs__read({"path": "a"})"#,
            &dotted
        ));
        assert!(!writes_tool_call_as_text(
            "fs.read is the tool for that",
            &dotted
        ));
    }

    #[test]
    fn classify_trivial_uses_markers() {
        let markers = crate::profile::AgentProfile::default().investigation_markers;
        assert!(classify_trivial("what is the arch", &markers));
        assert!(!classify_trivial(
            "why does the build fail and how is it configured",
            &markers
        ));
        // A domain marker forces investigation.
        let custom = vec!["vulnerability".to_string()];
        assert!(!classify_trivial("any vulnerability here", &custom));
    }
}
