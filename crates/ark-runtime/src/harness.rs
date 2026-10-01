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
    InvocationStatus, Note, NoteKind, ProjectId, ResearchState, Reuse, RiskTier, WorkspaceContext,
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
use crate::steer::{self, Steering};

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
    },
    /// A model call failed with a temporary error and is tried again after `delay_ms`. No
    /// text of the failed attempt had reached the user, and no tool runs twice: tools run
    /// only after a model call succeeds.
    Retry {
        role: String,
        attempt: u32,
        delay_ms: u64,
        error: String,
    },
    /// Several steps in a row produced nothing new. `stopped` is false when the model was
    /// nudged to change course, true when the loop ended and a final answer was forced.
    Stall {
        idle_steps: u32,
        stopped: bool,
    },
    /// The turn was cancelled; no answer follows.
    Cancelled,
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
    /// Messages the user sent while the turn ran that reached the model, in order.
    pub steered: Vec<String>,
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
    steer: Option<Steering>,
    retry: ark_model::RetryPolicy,
    presentation: bool,
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
            steer: None,
            retry: ark_model::RetryPolicy::default(),
            presentation: false,
        }
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
    /// Take messages the user sends while the turn runs (see [`Steering`]).
    pub fn steering(mut self, steer: Option<Steering>) -> Self {
        self.steer = steer;
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

        let sys = self.system_prompt(engine, profile, project).await?;
        let system = sys.render();
        let meta_cfg = MetaToolConfig {
            knowledge: profile.knowledge.is_some(),
            acquisition: profile.acquisition.is_some(),
            present: self.presentation,
        };

        let wsctx = self.build_workspace_context(engine, project)?;

        // Loaded action schemas: the profile's core set (present in the registry), then each
        // on-demand load appended. The order never changes within a turn, so loading one more
        // action keeps every earlier tool definition in the cacheable prefix.
        let mut loaded: Vec<String> = core_loaded(engine, profile);

        let first = FirstTurn {
            workspace: wsctx.digest(6),
            conversation: self.conversation.clone(),
            task: question.to_string(),
        };
        let first_text = first.render();
        let first_tokens = estimate_tokens(&first_text) + 4;
        let mut ws = WorkingSet::new(self.budget);
        ws.push_round(vec![Message::user(first_text)]);
        // Tokens of retrieval results (evidence lookups, knowledge search) per round, aligned
        // with the working set's rounds, so the context report can show them apart.
        let mut retrieved: Vec<u32> = vec![0];

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
        // When an action that changes state last ran this turn. Reads before it are not
        // reused after it.
        let mut changed_at = turn_started;
        let mut fresh_meta: u32 = 0;
        let mut steered: Vec<String> = Vec::new();

        for _ in 0..self.max_steps {
            if cancelled || self.is_cancelled() {
                cancelled = true;
                break;
            }
            steps_taken += 1;
            metrics.record_step();
            on_event(HarnessEvent::Step { index: steps_taken });
            let progress_before =
                metrics.cards_created - repeated_failures + fresh_meta + loaded.len() as u32;

            let state_digest = state.digest(STATE_DIGEST_ITEMS);
            let tail = state_message(&state_digest);
            let (mut messages, est) = ws.assemble_with(&system, WorkingSet::message_tokens(&tail));
            messages.push(tail);
            metrics.peak_prompt_tokens = metrics.peak_prompt_tokens.max(est);

            let tools = self.tool_set(engine, &loaded, meta_cfg);
            let report = self.report(
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
                answer = completion.content.trim().to_string();
                streamed = !answer.is_empty();
                break;
            }

            let mut round = vec![Message::assistant_tool_calls(completion.tool_calls.clone())];
            let mut round_retrieved = 0u32;

            // Reads and observations of this step that need no approval and were not reused
            // run together (see `flush`). Anything else first finishes the pending batch, so
            // results stay in call order.
            let mut batch: Vec<Pending> = Vec::new();
            let mut tally = Tally {
                metrics: &mut metrics,
                cited: &mut cited,
                repeated_failures: &mut repeated_failures,
                changed_at: &mut changed_at,
            };
            macro_rules! flush {
                () => {
                    if !batch.is_empty()
                        && !self
                            .flush(
                                engine,
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

                if desc.risk == RiskTier::Mutate {
                    // A change waits for the reads before it, and runs alone.
                    flush!();
                }
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

                // Dedup: reuse identical prior evidence instead of re-running the tool (see
                // `Reuse`). A state-changing call is never deduplicated: running it again is
                // the point. Only a successful result is evidence. A failure (an error, a
                // timeout, a missing tool) may be transient, so the same call runs again; a
                // repeat that fails again counts as no progress, which keeps the stall guard
                // working. The digest uses the canonical artifact id, so a short id and a full
                // id name the same call.
                let canonical = engine.canonical_input(project, call.arguments.clone());
                let digest = call_digest(&call.name, &canonical);
                if batch.iter().any(|p| p.digest == digest) {
                    // The same call is already pending: let it finish so this one can reuse it.
                    flush!();
                }
                let mut failed_before = false;
                if desc.risk != RiskTier::Mutate && desc.reuse != Reuse::Volatile {
                    let prior = engine.store.find_card_by_digest(project, &digest)?;
                    failed_before = prior
                        .as_ref()
                        .is_some_and(|c| !c.ok && c.created_at >= turn_started);
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

                batch.push(Pending {
                    call: call.clone(),
                    risk: desc.risk,
                    approved,
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
                        engine,
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
            on_event(HarnessEvent::Cancelled);
            return Ok(HarnessOutcome {
                answer: String::new(),
                cited,
                steps: steps_taken,
                trivial,
                metrics,
                state,
                cancelled: true,
                steered,
            });
        }

        if answer.is_empty() {
            match self
                .force_answer(
                    profile,
                    &mut ws,
                    &sys,
                    (&first, first_tokens, &retrieved, loaded.len(), steps_taken),
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
                        steered,
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
            steered,
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
        let sys = self.system_prompt(engine, profile, project).await?;
        let system = sys.render();
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
        let state_digest = state.digest(STATE_DIGEST_ITEMS);
        let tail = state_message(&state_digest);
        let (_, est) = ws.assemble_with(&system, WorkingSet::message_tokens(&tail));
        let loaded = core_loaded(engine, profile);
        let meta_cfg = MetaToolConfig {
            knowledge: profile.knowledge.is_some(),
            acquisition: profile.acquisition.is_some(),
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
        // The runtime's own lines: the rules after the instructions, and the project block.
        let project = (estimate_tokens(&sys.rules) + estimate_tokens(&sys.project))
            .saturating_sub(instructions);
        let capability_index = estimate_tokens(&sys.index);
        let working_state = WorkingSet::message_tokens(&state_message(state_digest));
        let system_total = estimate_tokens(&sys.render());
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
        // Rounds before `last_compressed` were dropped from this request.
        let retrieved: u32 = retrieved.iter().skip(ws.last_compressed).sum();
        let model_output = ws.kept_model_output();
        let tool_rounds = total
            .saturating_sub(system_total)
            .saturating_sub(working_state)
            .saturating_sub(first_kept)
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
            task,
            tool_rounds,
            retrieved,
            compressed_rounds: ws.last_compressed,
            tool_schemas,
            loaded_actions: loaded,
            total,
            budget: self.budget,
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
        let jobs: Vec<(String, serde_json::Value, bool)> = pending
            .iter()
            .map(|p| (p.call.name.clone(), p.call.arguments.clone(), p.approved))
            .collect();
        let runs =
            futures::stream::iter(jobs.into_iter().map(|(name, input, approved)| async move {
                engine.run_action(project, &name, input, approved).await
            }))
            .buffered(MAX_PARALLEL_TOOLS)
            .collect::<Vec<_>>();
        let Some(results) = self.cancellable(runs).await else {
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
            let card = distill(&inv, &profile.distillers);
            engine.store.put_evidence_card(project, &card)?;
            tally.metrics.record_card_created();
            if !ok && p.failed_before {
                *tally.repeated_failures += 1;
            }
            tally.cited.push(inv.id.clone());

            let model_text = card_for_model(&card);
            let raw_tokens = estimate_tokens(&inv.output.to_string());
            on_event(HarnessEvent::ToolResult {
                id: p.call.id.clone(),
                action: p.call.name.clone(),
                ok,
                summary: card.summary.clone(),
                invocation: inv.id.as_str().to_string(),
                duration_ms: inv.duration_ms(),
                error: inv.error.clone(),
                // The same per-message estimate the working set budgets with.
                context_tokens: estimate_tokens(&model_text) + 4,
                raw_tokens,
            });
            round.push(Message::tool_result(&p.call, model_text));
        }
        Ok(true)
    }

    /// Hand the user's mid-turn messages to the model at the end of `round`.
    fn deliver_steering(
        &self,
        texts: Vec<String>,
        round: &mut Vec<Message>,
        steered: &mut Vec<String>,
        on_event: &mut impl FnMut(HarnessEvent),
    ) {
        for text in texts {
            round.push(Message::user(format!(
                "The user added this while you were working. Take it into account and continue:\n{text}"
            )));
            on_event(HarnessEvent::Steered { text: text.clone() });
            steered.push(text);
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
        let started = std::time::Instant::now();
        let mut retries = 0u32;
        loop {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let sink = move |t: &str| {
                let _ = tx.send(t.to_string());
            };
            let attempt_started = std::time::Instant::now();
            let mut first_token_ms: Option<u64> = None;
            let fut = provider.complete_streaming(system, messages, tools, &sink);
            tokio::pin!(fut);
            let result = loop {
                tokio::select! {
                    biased;
                    _ = cancel::wait(&self.cancel) => return Ok(None),
                    Some(text) = rx.recv() => {
                        first_token_ms.get_or_insert(attempt_started.elapsed().as_millis() as u64);
                        if stream {
                            on_event(HarnessEvent::TextDelta { text });
                        }
                    }
                    r = &mut fut => break r,
                }
            };
            while let Ok(text) = rx.try_recv() {
                first_token_ms.get_or_insert(attempt_started.elapsed().as_millis() as u64);
                if stream {
                    on_event(HarnessEvent::TextDelta { text });
                }
            }
            let completion = match result {
                Ok(c) => c,
                // Retry only when nothing of this attempt was shown: a second answer after
                // half of a first one would duplicate output.
                Err(e) if first_token_ms.is_none() && self.retry.should_retry(&e, retries) => {
                    let delay = self.retry.delay(retries, &e);
                    retries += 1;
                    metrics.record_retry();
                    on_event(HarnessEvent::Retry {
                        role: role_name(role).to_string(),
                        attempt: retries,
                        delay_ms: delay.as_millis() as u64,
                        error: e.to_string(),
                    });
                    if self.cancellable(tokio::time::sleep(delay)).await.is_none() {
                        return Ok(None);
                    }
                    continue;
                }
                Err(e) => return Err(AgentError::Provider(e.to_string())),
            };
            let model = provider.model_id();
            metrics.record_call(role, &model, completion.usage);
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
        loaded: &[String],
        cfg: MetaToolConfig,
    ) -> Vec<ark_model::ToolDef> {
        let mut tools = catalog::meta_tool_defs(cfg);
        for name in loaded {
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
        loaded: &mut Vec<String>,
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
        let instructions = estimate_tokens(&profile.prompts.plan_system);
        let task = WorkingSet::message_tokens(&msgs[0]);
        on_event(HarnessEvent::Context {
            report: ContextReport {
                role: "plan".into(),
                instructions,
                task,
                total: instructions + task,
                budget: self.budget,
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
        sys: &SystemPrompt,
        (first, first_tokens, retrieved, loaded, step): (&FirstTurn, u32, &[u32], usize, usize),
        state: &ResearchState,
        metrics: &mut Metrics,
        on_event: &mut impl FnMut(HarnessEvent),
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
                budget: self.budget,
                ..Default::default()
            },
        });
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

    /// The system prompt for a turn. Fixed for the whole turn, and ordered from most to least
    /// stable (instructions and rules, the capability index, then the project and its
    /// artifacts), so a provider's prefix cache can serve it across steps and across turns.
    async fn system_prompt(
        &self,
        engine: &Engine,
        profile: &AgentProfile,
        project: &ProjectId,
    ) -> Result<SystemPrompt, AgentError> {
        let p = engine
            .store
            .get_project(project)?
            .ok_or_else(|| AgentError::ProjectNotFound(project.to_string()))?;
        let artifacts = engine.store.list_artifacts(project)?;
        let rules = format!(
            "{}\n\nActions that change state need explicit human approval; observation and analysis are free — reason from what tools return.",
            profile.prompts.instructions
        );
        let index = crate::capmap::capability_index(
            &engine.registry,
            &profile.capabilities,
            &profile.inferrer,
        )
        .await;
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
        Ok(SystemPrompt {
            rules,
            index,
            project: project_block.trim_end().to_string(),
        })
    }
}

/// Independent read-only calls from one model reply that run at the same time.
pub const MAX_PARALLEL_TOOLS: usize = 4;

/// An adapter call waiting to run in the current batch.
struct Pending {
    call: ark_model::ToolCall,
    risk: RiskTier,
    approved: bool,
    failed_before: bool,
    digest: String,
}

/// The per-turn tallies a finished call updates.
struct Tally<'a> {
    metrics: &'a mut Metrics,
    cited: &'a mut Vec<InvocationId>,
    repeated_failures: &'a mut u32,
    changed_at: &'a mut time::OffsetDateTime,
}

/// Artifacts named in the system prompt. Older ones stay reachable through search.
const ARTIFACTS_LISTED: usize = 12;

/// The system prompt, by part.
struct SystemPrompt {
    /// The profile's instructions plus the runtime's fixed rules.
    rules: String,
    index: String,
    project: String,
}

impl SystemPrompt {
    fn render(&self) -> String {
        format!("{}\n\n{}\n\n{}", self.rules, self.index, self.project)
    }
}

/// The working state, sent as the last message of every request. It changes as the agent
/// records findings; keeping it at the end leaves everything before it byte-identical between
/// steps, which is what a prefix cache needs.
fn state_message(digest: &str) -> Message {
    Message::user(format!("== WORKING STATE ==\n{digest}"))
}

/// The profile's core actions that exist in the registry, in the profile's order.
fn core_loaded(engine: &Engine, profile: &AgentProfile) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for a in &profile.core_actions {
        if engine.registry.descriptor(a).is_some() && !v.contains(a) {
            v.push(a.clone());
        }
    }
    v
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
