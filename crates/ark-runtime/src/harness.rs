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

use std::collections::HashSet;
use std::fmt::Write as _;
use std::sync::Arc;

use ark_core::{
    ArtifactId, Author, Claim, ClaimType, EvidenceCard, Focus, Interesting, InvocationId,
    InvocationStatus, Note, NoteKind, ProjectId, ResearchState, RiskTier, WorkspaceContext,
};
use ark_model::{estimate_tokens, Completion, Message, ToolDef, Usage};
use serde::Serialize;
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

/// Events emitted as the harness works. They describe *what happened*, never how to show it:
/// a terminal, a GUI, a JSON log and a test all consume the same stream. Every event is owned
/// and serializable so it can cross a channel or a process boundary.
#[derive(Clone, Debug, Serialize)]
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
    /// A model call finished: which role, which model, what it cost.
    ModelCall {
        role: String,
        model: String,
        usage: Usage,
        elapsed_ms: u64,
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
    /// The turn was cancelled; no answer follows.
    Cancelled,
    /// The final answer. `streamed` is true when its text already arrived as `TextDelta`s.
    Answer {
        text: String,
        streamed: bool,
    },
}

pub struct HarnessOutcome {
    pub answer: String,
    pub cited: Vec<InvocationId>,
    pub steps: usize,
    pub trivial: bool,
    pub metrics: Metrics,
    pub state: ResearchState,
    /// True when the run stopped on a [`CancelToken`]; `answer` is then empty.
    pub cancelled: bool,
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
    budget: u32,
    planner: bool,
    critic: bool,
    focus: Option<Focus>,
    conversation: Option<String>,
    approvals: Option<Arc<ApprovalGate>>,
    cancel: Option<CancelToken>,
}

/// The pieces of the first user message, kept separate so the context report can attribute
/// tokens to each.
struct FirstTurn {
    workspace: Option<String>,
    conversation: Option<String>,
    task: String,
}

impl FirstTurn {
    fn render(&self) -> String {
        let mut s = String::new();
        if let Some(w) = &self.workspace {
            let _ = write!(s, "== WORKSPACE CONTEXT ==\n{w}\n\n");
        }
        if let Some(c) = &self.conversation {
            let _ = write!(s, "== EARLIER IN THIS CONVERSATION ==\n{c}\n\n");
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
            budget: std::env::var("ARK_CTX_BUDGET")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(6000),
            planner: true,
            critic: true,
            focus: None,
            conversation: None,
            approvals: None,
            cancel: None,
        }
    }

    /// Tell the harness what the user is currently looking at, so on-screen references
    /// resolve against it.
    pub fn focus(mut self, focus: Option<Focus>) -> Self {
        self.focus = focus.filter(|f| !f.is_empty());
        self
    }
    pub fn allow_mutate(mut self, yes: bool) -> Self {
        self.allow_mutate = yes;
        self
    }
    pub fn max_steps(mut self, n: usize) -> Self {
        self.max_steps = n;
        self
    }
    pub fn budget(mut self, tokens: u32) -> Self {
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
    /// Stop cooperatively when this token is cancelled.
    pub fn cancel(mut self, token: Option<CancelToken>) -> Self {
        self.cancel = token;
        self
    }
    pub fn describe(&self) -> String {
        self.router.describe()
    }
    pub fn budget_tokens(&self) -> u32 {
        self.budget
    }

    /// Run a task grounded in a project, shaped by `profile`.
    pub async fn ask(
        &self,
        engine: &Engine,
        profile: &AgentProfile,
        project: &ProjectId,
        question: &str,
        mut on_event: impl FnMut(HarnessEvent),
    ) -> Result<HarnessOutcome, AgentError> {
        let mut metrics = Metrics::start();
        let turn_started = time::OffsetDateTime::now_utc();

        // Resume: load accumulated state; set the current objective without discarding prior
        // findings/interesting/hypotheses.
        let mut state = engine
            .store
            .get_research_state(project)?
            .unwrap_or_default();
        state.goal = question.to_string();
        state.touch();
        engine.store.put_research_state(project, &state)?;

        let trivial = classify_trivial(question, &profile.investigation_markers);
        on_event(HarnessEvent::Classified { trivial });

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

        let static_system = self.static_system(engine, profile, project)?;
        let index = crate::capmap::capability_index(
            &engine.registry,
            &profile.capabilities,
            &profile.inferrer,
        )
        .await;
        let meta_cfg = MetaToolConfig {
            knowledge: profile.knowledge.is_some(),
            acquisition: profile.acquisition.is_some(),
            present: true,
        };

        let wsctx = self.build_workspace_context(engine, project)?;

        // Loaded action schemas: the profile's core set (present in the registry) + on-demand.
        let mut loaded: HashSet<String> = profile
            .core_actions
            .iter()
            .filter(|a| engine.registry.descriptor(a).is_some())
            .cloned()
            .collect();

        let first = FirstTurn {
            workspace: wsctx.digest(6),
            conversation: self.conversation.clone(),
            task: question.to_string(),
        };
        let first_text = first.render();
        let first_tokens = estimate_tokens(&first_text) + 4;
        let mut ws = WorkingSet::new(self.budget);
        ws.push_round(vec![Message::user(first_text)]);

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

        for _ in 0..self.max_steps {
            if cancelled || self.is_cancelled() {
                cancelled = true;
                break;
            }
            steps_taken += 1;
            metrics.record_step();
            on_event(HarnessEvent::Step { index: steps_taken });
            let progress_before = metrics.cards_created
                + metrics.state_updates
                + metrics.retrievals
                + loaded.len() as u32;

            let state_digest = state.digest(6);
            let system =
                format!("{static_system}\n\n{index}\n\n== WORKING STATE ==\n{state_digest}");
            let (messages, est) = ws.assemble(&system);
            metrics.peak_prompt_tokens = metrics.peak_prompt_tokens.max(est);

            let tools = self.tool_set(engine, &loaded, meta_cfg);
            let report = self.report(
                profile,
                &static_system,
                &index,
                &state_digest,
                &first,
                first_tokens,
                &ws,
                est,
                &tools,
                loaded.len(),
                steps_taken,
            );
            on_event(HarnessEvent::Context { report });

            let Some(completion) = self
                .call_model(
                    Role::Reason,
                    &system,
                    &messages,
                    &tools,
                    true,
                    &mut metrics,
                    &mut on_event,
                )
                .await?
            else {
                cancelled = true;
                break;
            };

            if completion.tool_calls.is_empty() {
                answer = completion.content.trim().to_string();
                streamed = !answer.is_empty();
                break;
            }

            let mut round = vec![Message::assistant_tool_calls(completion.tool_calls.clone())];

            for call in &completion.tool_calls {
                if self.is_cancelled() {
                    cancelled = true;
                    break;
                }
                if catalog::is_meta_tool(&call.name) {
                    let result = self
                        .handle_meta(
                            engine,
                            profile,
                            project,
                            call,
                            &mut loaded,
                            &mut state,
                            &mut metrics,
                            &mut on_event,
                        )
                        .await?;
                    round.push(Message::tool_result(call, result));
                    continue;
                }

                let Some(desc) = engine.registry.descriptor(&call.name) else {
                    round.push(Message::tool_result(
                        call,
                        format!("error: unknown tool `{}`. Call load_action(name) first, or check the capability index.", call.name),
                    ));
                    continue;
                };

                on_event(HarnessEvent::ToolCall {
                    id: call.id.clone(),
                    action: call.name.clone(),
                    input: call.arguments.clone(),
                });

                let mut approved = self.allow_mutate;
                if desc.risk == RiskTier::Mutate && !self.allow_mutate {
                    match self
                        .request_approval(profile, &desc, call, &mut on_event)
                        .await
                    {
                        None => {
                            cancelled = true;
                            break;
                        }
                        Some(Some(ApprovalDecision::Deny)) => {
                            round.push(Message::tool_result(
                                call,
                                "denied: the user declined this action. Do not retry it; continue from read-only evidence or ask the user.",
                            ));
                            continue;
                        }
                        Some(Some(_)) => approved = true,
                        Some(None) => {
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

                // Dedup: reuse identical prior evidence instead of re-running the tool. A
                // state-changing call is never deduplicated: running it again is the point.
                let digest = call_digest(&call.name, &call.arguments);
                if desc.risk != RiskTier::Mutate {
                    let reuse = engine
                        .store
                        .find_card_by_digest(project, &digest)?
                        .filter(|card| card.created_at >= turn_started || reusable_across_turns(engine, project, &desc, &call.arguments));
                    if let Some(existing) = reuse {
                        metrics.record_duplicate();
                        let reused = existing.reference_line();
                        on_event(HarnessEvent::Duplicate {
                            action: call.name.clone(),
                            reused: reused.clone(),
                        });
                        cited.push(existing.invocation.clone());
                        round.push(Message::tool_result(
                            call,
                            format!("(reused prior evidence) {reused}"),
                        ));
                        continue;
                    }
                }

                let run = engine.run_action(project, &call.name, call.arguments.clone(), approved);
                let Some(inv) = self.cancellable(run).await else {
                    cancelled = true;
                    break;
                };
                let inv = inv?;
                metrics.record_tool_call();
                let ok = matches!(inv.status, InvocationStatus::Ok);
                let card = distill(&inv, &profile.distillers);
                engine.store.put_evidence_card(project, &card)?;
                metrics.record_card_created();
                cited.push(inv.id.clone());

                on_event(HarnessEvent::ToolResult {
                    id: call.id.clone(),
                    action: call.name.clone(),
                    ok,
                    summary: card.summary.clone(),
                    invocation: inv.id.as_str().to_string(),
                    duration_ms: inv.duration_ms(),
                    error: inv.error.clone(),
                });
                round.push(Message::tool_result(call, card_for_model(&card)));
            }

            if cancelled {
                break;
            }

            let progress_after = metrics.cards_created
                + metrics.state_updates
                + metrics.retrievals
                + loaded.len() as u32;
            if progress_after > progress_before {
                stall = 0;
            } else {
                stall += 1;
                if stall == STALL_STEER {
                    round.push(Message::user(
                        "You have gathered no new evidence for several steps. Either call load_action(name) \
                         and then a concrete action to produce evidence, or stop now and give your best \
                         grounded answer. Do not repeat discovery calls you have already made.",
                    ));
                }
            }
            ws.push_round(round);
            if stall >= STALL_MAX {
                break;
            }
        }

        if cancelled {
            state.touch();
            engine.store.put_research_state(project, &state)?;
            metrics.finish();
            on_event(HarnessEvent::Cancelled);
            return Ok(HarnessOutcome {
                answer: String::new(),
                cited,
                steps: steps_taken,
                trivial,
                metrics,
                state,
                cancelled: true,
            });
        }

        if answer.is_empty() {
            match self
                .force_answer(
                    profile,
                    &mut ws,
                    &static_system,
                    &index,
                    &state,
                    &mut metrics,
                    &mut on_event,
                )
                .await?
            {
                Some(a) => {
                    answer = a;
                    streamed = !answer.is_empty();
                }
                None => {
                    metrics.finish();
                    on_event(HarnessEvent::Cancelled);
                    return Ok(HarnessOutcome {
                        answer: String::new(),
                        cited,
                        steps: steps_taken,
                        trivial,
                        metrics,
                        state,
                        cancelled: true,
                    });
                }
            }
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
                    &static_system,
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
        let state = engine
            .store
            .get_research_state(project)?
            .unwrap_or_default();
        let static_system = self.static_system(engine, profile, project)?;
        let index = crate::capmap::capability_index(
            &engine.registry,
            &profile.capabilities,
            &profile.inferrer,
        )
        .await;
        let wsctx = self.build_workspace_context(engine, project)?;
        let first = FirstTurn {
            workspace: wsctx.digest(6),
            conversation: self.conversation.clone(),
            task: task.to_string(),
        };
        let first_text = first.render();
        let first_tokens = estimate_tokens(&first_text) + 4;
        let mut ws = WorkingSet::new(self.budget);
        ws.push_round(vec![Message::user(first_text)]);
        let state_digest = state.digest(6);
        let system = format!("{static_system}\n\n{index}\n\n== WORKING STATE ==\n{state_digest}");
        let (_, est) = ws.assemble(&system);
        let loaded: HashSet<String> = profile
            .core_actions
            .iter()
            .filter(|a| engine.registry.descriptor(a).is_some())
            .cloned()
            .collect();
        let meta_cfg = MetaToolConfig {
            knowledge: profile.knowledge.is_some(),
            acquisition: profile.acquisition.is_some(),
            present: true,
        };
        let tools = self.tool_set(engine, &loaded, meta_cfg);
        Ok(self.report(
            profile,
            &static_system,
            &index,
            &state_digest,
            &first,
            first_tokens,
            &ws,
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
        static_system: &str,
        index: &str,
        state_digest: &str,
        first: &FirstTurn,
        first_tokens: u32,
        ws: &WorkingSet,
        total: u32,
        tools: &[ToolDef],
        loaded: usize,
        step: usize,
    ) -> ContextReport {
        let instructions = estimate_tokens(&profile.prompts.instructions);
        let project = estimate_tokens(static_system).saturating_sub(instructions);
        let capability_index = estimate_tokens(index);
        let working_state = estimate_tokens(state_digest);
        let system_total = estimate_tokens(&format!(
            "{static_system}\n\n{index}\n\n== WORKING STATE ==\n{state_digest}"
        ));
        // The first round holds workspace + conversation + task; attribute its framing to the task.
        let (workspace, conversation, task) = if ws.last_compressed == 0 {
            let w = first.workspace.as_deref().map(estimate_tokens).unwrap_or(0);
            let c = first
                .conversation
                .as_deref()
                .map(estimate_tokens)
                .unwrap_or(0);
            (w, c, first_tokens.saturating_sub(w + c))
        } else {
            (0, 0, 0)
        };
        let first_kept = if ws.last_compressed == 0 {
            first_tokens
        } else {
            0
        };
        let tool_rounds = total
            .saturating_sub(system_total)
            .saturating_sub(first_kept);
        let tool_schemas = tools
            .iter()
            .map(|t| {
                estimate_tokens(&t.name)
                    + estimate_tokens(&t.description)
                    + estimate_tokens(&t.parameters.to_string())
            })
            .sum();
        ContextReport {
            step,
            instructions,
            project,
            capability_index,
            working_state,
            conversation,
            workspace,
            task,
            tool_rounds,
            compressed_rounds: ws.last_compressed,
            tool_schemas,
            loaded_actions: loaded,
            total,
            budget: self.budget,
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .map(|c| c.is_cancelled())
            .unwrap_or(false)
    }

    /// Run a future unless the turn is cancelled first. `None` means cancelled.
    async fn cancellable<F: std::future::Future>(&self, fut: F) -> Option<F::Output> {
        tokio::select! {
            biased;
            _ = cancel::wait(&self.cancel) => None,
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
    ) -> Result<Option<Completion>, AgentError> {
        let provider = self.router.provider(role).clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let sink = move |t: &str| {
            let _ = tx.send(t.to_string());
        };
        let started = std::time::Instant::now();
        let fut = provider.complete_streaming(system, messages, tools, &sink);
        tokio::pin!(fut);
        let result = loop {
            tokio::select! {
                biased;
                _ = cancel::wait(&self.cancel) => return Ok(None),
                Some(text) = rx.recv() => {
                    if stream {
                        on_event(HarnessEvent::TextDelta { text });
                    }
                }
                r = &mut fut => break r,
            }
        };
        while let Ok(text) = rx.try_recv() {
            if stream {
                on_event(HarnessEvent::TextDelta { text });
            }
        }
        let completion = result.map_err(|e| AgentError::Provider(e.to_string()))?;
        let model = provider.model_id();
        metrics.record_call(role, &model, completion.usage);
        on_event(HarnessEvent::ModelCall {
            role: role_name(role).to_string(),
            model,
            usage: completion.usage,
            elapsed_ms: started.elapsed().as_millis() as u64,
        });
        Ok(Some(completion))
    }

    /// Ask the approval gate about a `Mutate` call. `None`: cancelled while waiting.
    /// `Some(None)`: no gate is configured (refuse as before). `Some(Some(d))`: the decision.
    async fn request_approval(
        &self,
        profile: &AgentProfile,
        desc: &ark_core::ActionDescriptor,
        call: &ark_model::ToolCall,
        on_event: &mut impl FnMut(HarnessEvent),
    ) -> Option<Option<ApprovalDecision>> {
        let gate = match &self.approvals {
            None => return Some(None),
            Some(g) => g.clone(),
        };
        let request = ApprovalRequest {
            action: call.name.clone(),
            summary: desc.summary.clone(),
            risk: desc.risk,
            input: call.arguments.clone(),
            class: profile.approval_class(&call.name, &call.arguments),
            preview: preview_call(&call.name, &call.arguments),
        };
        if !gate.is_granted(&request.class) {
            on_event(HarnessEvent::ApprovalRequested {
                request: request.clone(),
            });
        }
        let decision = self.cancellable(gate.check(&request)).await?;
        on_event(HarnessEvent::ApprovalResolved {
            action: request.action.clone(),
            class: request.class.clone(),
            decision,
        });
        Some(Some(decision))
    }

    fn tool_set(
        &self,
        engine: &Engine,
        loaded: &HashSet<String>,
        cfg: MetaToolConfig,
    ) -> Vec<ark_model::ToolDef> {
        let mut tools = catalog::meta_tool_defs(cfg);
        let mut names: Vec<&String> = loaded.iter().collect();
        names.sort();
        for name in names {
            if let Some(d) = engine.registry.descriptor(name) {
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
        loaded: &mut HashSet<String>,
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
                let acts = catalog::list_actions(&engine.registry, &adapter);
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
                match engine.registry.descriptor(&name) {
                    Some(d) => {
                        loaded.insert(name.clone());
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
                let path = a.get("path").and_then(|v| v.as_str());
                let limit = a.get("limit").and_then(|v| v.as_u64()).unwrap_or(40) as usize;
                on_event(HarnessEvent::Retrieval {
                    what: format!(
                        "get_evidence {inv_ref}{}",
                        path.map(|p| format!(".{p}")).unwrap_or_default()
                    ),
                });
                match engine.store.resolve_invocation(project, &inv_ref)? {
                    None => Ok(format!("no invocation matching `{inv_ref}`.")),
                    Some(inv) => Ok(extract_evidence(&inv.output, path, limit)),
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
                    return Ok(format!("no matches for \"{query}\"."));
                }
                let mut s = format!("{} match(es) for \"{query}\":\n", hits.len());
                for h in hits {
                    let _ = writeln!(
                        s,
                        "  art:{} {} ({}) score {:.2}",
                        &h.artifact_id[..h.artifact_id.len().min(12)],
                        h.name,
                        h.kind,
                        h.score
                    );
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
                    return Ok("error: `present` needs a `view` (artifact, file, diff, …).".into());
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
                            // Acquisition is gated by the same approval as a Mutate action.
                            _ => {
                                let approved = self.allow_mutate
                                    || self.approve_acquisition(profile, &need, on_event).await;
                                (
                                    "acquire_capability",
                                    acq.acquire(&need, approved).await.summary,
                                )
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
        let c = self
            .call_model(
                Role::Plan,
                &profile.prompts.plan_system,
                &msgs,
                &[],
                false,
                metrics,
                on_event,
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
        );
        let call = ark_model::ToolCall {
            id: String::new(),
            name: catalog::ACQUIRE_CAPABILITY.into(),
            arguments: json!({ "need": need }),
        };
        matches!(self.request_approval(profile, &desc, &call, on_event).await, Some(Some(d)) if d.allows())
    }

    #[allow(clippy::too_many_arguments)]
    async fn force_answer(
        &self,
        profile: &AgentProfile,
        ws: &mut WorkingSet,
        static_system: &str,
        index: &str,
        state: &ResearchState,
        metrics: &mut Metrics,
        on_event: &mut impl FnMut(HarnessEvent),
    ) -> Result<Option<String>, AgentError> {
        let system = format!(
            "{static_system}\n\n{index}\n\n== WORKING STATE ==\n{}",
            state.digest(8)
        );
        let (mut messages, est) = ws.assemble(&system);
        metrics.peak_prompt_tokens = metrics.peak_prompt_tokens.max(est);
        messages.push(Message::user(&profile.prompts.force_answer));
        Ok(self
            .call_model(
                Role::Synthesize,
                &system,
                &messages,
                &[],
                true,
                metrics,
                on_event,
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
            format!("answered; {} evidence item(s) cited", cited.len()),
        )
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_critic(
        &self,
        profile: &AgentProfile,
        static_system: &str,
        state: &ResearchState,
        answer: &str,
        issue: &str,
        metrics: &mut Metrics,
        on_event: &mut impl FnMut(HarnessEvent),
    ) -> Option<String> {
        let sys = format!(
            "{static_system}\n\n{} (the check that failed: {issue})",
            profile.prompts.critic_intro
        );
        let msgs = [Message::user(format!(
            "== WORKING STATE ==\n{}\n\n== DRAFT ANSWER ==\n{answer}",
            state.digest(8)
        ))];
        let c = self
            .call_model(Role::Critic, &sys, &msgs, &[], false, metrics, on_event)
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

    fn static_system(
        &self,
        engine: &Engine,
        profile: &AgentProfile,
        project: &ProjectId,
    ) -> Result<String, AgentError> {
        let p = engine
            .store
            .get_project(project)?
            .ok_or_else(|| AgentError::ProjectNotFound(project.to_string()))?;
        let artifacts = engine.store.list_artifacts(project)?;
        let mut s = String::new();
        // The consumer's persona + method.
        s.push_str(&profile.prompts.instructions);
        s.push_str("\n\n");
        // The runtime's mechanical footer.
        let _ = writeln!(s, "Project: {} — {}", p.name, p.target);
        let _ = writeln!(s, "Artifacts (pass an id as the `artifact` argument):");
        if artifacts.is_empty() {
            let _ = writeln!(s, "  (none yet — ingest a file or capture one first)");
        }
        for a in artifacts.iter().take(20) {
            let _ = writeln!(
                s,
                "  - id={} name={} kind={}",
                a.id.as_str(),
                a.name,
                a.kind.as_tag()
            );
        }
        let _ = write!(s, "\nActions that change state need explicit human approval; observation and analysis are free — reason from what tools return.");
        Ok(s)
    }
}

/// Whether evidence from an earlier turn still answers this call. Workspace files and external
/// systems change between turns, so only calls on fixed data qualify: an action declared
/// reusable, or one that names a content-addressed artifact (its bytes can never change).
fn reusable_across_turns(engine: &Engine, project: &ProjectId, desc: &ark_core::ActionDescriptor, input: &serde_json::Value) -> bool {
    if desc.reusable {
        return true;
    }
    match input.get("artifact").and_then(|a| a.as_str()) {
        Some(id) => engine.store.resolve_artifact(project, id).ok().flatten().is_some(),
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
        for t in [&sel.function, &sel.symbol, &sel.address] {
            if let Some(v) = t {
                let v = v.trim();
                if v.len() >= 2 {
                    terms.push(v.to_ascii_lowercase());
                }
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
    cards.sort_by(|a, b| b.created_at.cmp(&a.created_at));

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
    if !card.identifiers.is_empty() {
        let shown: Vec<&String> = card.identifiers.iter().take(12).collect();
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
        "\n  (call get_evidence(\"inv:{}\") for exact rows)",
        card.invocation.short()
    );
    s
}

fn extract_evidence(output: &serde_json::Value, path: Option<&str>, limit: usize) -> String {
    let mut cur = output;
    if let Some(p) = path {
        for seg in p.split('.').filter(|s| !s.is_empty()) {
            match cur.get(seg) {
                Some(v) => cur = v,
                None => {
                    return format!(
                        "no such path `{p}` in this evidence (top-level keys: {}).",
                        top_keys(output)
                    )
                }
            }
        }
    }
    let capped = cap_arrays(cur, limit);
    let body = serde_json::to_string_pretty(&capped).unwrap_or_default();
    if body.chars().count() > 6000 {
        let t: String = body.chars().take(6000).collect();
        format!("{t}\n… (truncated; narrow with a path argument)")
    } else {
        body
    }
}

fn cap_arrays(v: &serde_json::Value, limit: usize) -> serde_json::Value {
    match v {
        serde_json::Value::Array(a) => {
            let mut out: Vec<serde_json::Value> =
                a.iter().take(limit).map(|x| cap_arrays(x, limit)).collect();
            if a.len() > limit {
                out.push(json!(format!(
                    "… (+{} more of {})",
                    a.len() - limit,
                    a.len()
                )));
            }
            serde_json::Value::Array(out)
        }
        serde_json::Value::Object(m) => serde_json::Value::Object(
            m.iter()
                .map(|(k, x)| (k.clone(), cap_arrays(x, limit)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn top_keys(v: &serde_json::Value) -> String {
    v.as_object()
        .map(|m| m.keys().cloned().collect::<Vec<_>>().join(", "))
        .unwrap_or_default()
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
