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

use ark_core::{
    ArtifactId, Author, Claim, ClaimType, EvidenceCard, Focus, Interesting, InvocationId,
    InvocationStatus, Note, NoteKind, ProjectId, ResearchState, RiskTier, WorkspaceContext,
};
use ark_model::Message;
use serde_json::json;

use crate::budget::WorkingSet;
use crate::catalog::{self, MetaToolConfig};
use crate::distill::{call_digest, distill};
use crate::engine::Engine;
use crate::error::AgentError;
use crate::metrics::Metrics;
use crate::profile::AgentProfile;
use crate::router::{ModelRouter, Role};

/// Events emitted as the harness works, for a live front-end.
pub enum HarnessEvent<'a> {
    Classified { trivial: bool },
    Plan { steps: &'a [String] },
    ToolCall { action: &'a str, input: &'a serde_json::Value },
    ToolResult { action: &'a str, ok: bool, summary: String },
    Duplicate { action: &'a str, reused: String },
    Retrieval { what: String },
    NeedsApproval { action: &'a str },
    /// A capability-acquisition meta-tool ran (find/propose/acquire). `action` is which one;
    /// `detail` is the agent-facing result.
    Capability { action: &'a str, detail: String },
    StateUpdate { kind: String, text: String },
    Note { kind: String, body: String },
    Present { directive: ark_core::PresentDirective },
    SelfCheck { passed: bool, detail: String },
    Answer { text: &'a str },
}

pub struct HarnessOutcome {
    pub answer: String,
    pub cited: Vec<InvocationId>,
    pub steps: usize,
    pub trivial: bool,
    pub metrics: Metrics,
    pub state: ResearchState,
}

pub struct Harness {
    router: ModelRouter,
    allow_mutate: bool,
    max_steps: usize,
    budget: u32,
    planner: bool,
    critic: bool,
    focus: Option<Focus>,
}

impl Harness {
    pub fn new(router: ModelRouter) -> Self {
        Harness {
            router,
            allow_mutate: false,
            max_steps: 16,
            budget: std::env::var("ARK_CTX_BUDGET").ok().and_then(|s| s.parse().ok()).unwrap_or(6000),
            planner: true,
            critic: true,
            focus: None,
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
    pub fn describe(&self) -> String {
        self.router.describe()
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

        // Resume: load accumulated state; set the current objective without discarding prior
        // findings/interesting/hypotheses.
        let mut state = engine.store.get_research_state(project)?.unwrap_or_default();
        state.goal = question.to_string();
        state.touch();
        engine.store.put_research_state(project, &state)?;

        let trivial = classify_trivial(question, &profile.investigation_markers);
        on_event(HarnessEvent::Classified { trivial });

        // Plan (non-trivial only, when enabled).
        if !trivial && self.planner {
            if let Some(steps) = self.draft_plan(profile, question, &mut metrics).await {
                if !steps.is_empty() {
                    on_event(HarnessEvent::Plan { steps: &steps });
                    state.next_actions = steps;
                    engine.store.put_research_state(project, &state)?;
                }
            }
        }

        let static_system = self.static_system(engine, profile, project)?;
        let index = crate::capmap::capability_index(&engine.registry, &profile.capabilities, &profile.inferrer).await;
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

        let mut ws = WorkingSet::new(self.budget);
        let first_turn = match wsctx.digest(6) {
            Some(d) => format!("== WORKSPACE CONTEXT ==\n{d}\n\n== TASK ==\n{question}"),
            None => question.to_string(),
        };
        ws.push_round(vec![Message::user(first_turn)]);

        let mut cited: Vec<InvocationId> = Vec::new();
        let mut answer = String::new();
        let mut steps_taken = 0usize;

        // Stall guard: a weak model can churn on meta-tools or errored calls for many steps
        // without gathering evidence. Count consecutive no-progress steps; steer once, then
        // force a final answer instead of burning the whole budget.
        const STALL_STEER: u32 = 3;
        const STALL_MAX: u32 = 5;
        let mut stall: u32 = 0;

        for _ in 0..self.max_steps {
            steps_taken += 1;
            metrics.record_step();
            let progress_before =
                metrics.cards_created + metrics.state_updates + metrics.retrievals + loaded.len() as u32;

            let system = format!(
                "{static_system}\n\n{index}\n\n== WORKING STATE ==\n{}",
                state.digest(6)
            );
            let (messages, est) = ws.assemble(&system);
            metrics.peak_prompt_tokens = metrics.peak_prompt_tokens.max(est);

            let tools = self.tool_set(engine, &loaded, meta_cfg);
            let provider = self.router.provider(Role::Reason);
            let completion = provider
                .complete(&system, &messages, &tools)
                .await
                .map_err(|e| AgentError::Provider(e.to_string()))?;
            metrics.record_call(Role::Reason, &provider.model_id(), completion.usage);

            if completion.tool_calls.is_empty() {
                answer = completion.content.trim().to_string();
                break;
            }

            let mut round = vec![Message::assistant_tool_calls(completion.tool_calls.clone())];

            for call in &completion.tool_calls {
                if catalog::is_meta_tool(&call.name) {
                    let result = self
                        .handle_meta(engine, profile, project, call, &mut loaded, &mut state, &mut metrics, &mut on_event)
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

                on_event(HarnessEvent::ToolCall { action: &call.name, input: &call.arguments });

                if desc.risk == RiskTier::Mutate && !self.allow_mutate {
                    on_event(HarnessEvent::NeedsApproval { action: &call.name });
                    round.push(Message::tool_result(
                        call,
                        "denied: this action changes state and needs human approval. Do not retry; reason from read-only evidence instead.",
                    ));
                    continue;
                }

                // Dedup: reuse identical prior evidence instead of re-running the tool.
                let digest = call_digest(&call.name, &call.arguments);
                if let Some(existing) = engine.store.find_card_by_digest(project, &digest)? {
                    metrics.record_duplicate();
                    let reused = existing.reference_line();
                    on_event(HarnessEvent::Duplicate { action: &call.name, reused: reused.clone() });
                    cited.push(existing.invocation.clone());
                    round.push(Message::tool_result(call, format!("(reused prior evidence) {reused}")));
                    continue;
                }

                let inv = engine
                    .run_action(project, &call.name, call.arguments.clone(), self.allow_mutate)
                    .await?;
                metrics.record_tool_call();
                let ok = matches!(inv.status, InvocationStatus::Ok);
                let card = distill(&inv, &profile.distillers);
                engine.store.put_evidence_card(project, &card)?;
                metrics.record_card_created();
                cited.push(inv.id.clone());

                on_event(HarnessEvent::ToolResult { action: &call.name, ok, summary: card.summary.clone() });
                round.push(Message::tool_result(call, card_for_model(&card)));
            }

            let progress_after =
                metrics.cards_created + metrics.state_updates + metrics.retrievals + loaded.len() as u32;
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

        if answer.is_empty() {
            answer = self.force_answer(profile, &mut ws, &static_system, &index, &state, &mut metrics).await?;
        }

        let (passed, detail) = self.self_check(&answer, &cited, trivial, steps_taken);
        on_event(HarnessEvent::SelfCheck { passed, detail: detail.clone() });
        if !passed && self.critic && !trivial {
            if let Some(revised) = self.run_critic(profile, &static_system, &state, &answer, &detail, &mut metrics).await {
                answer = revised;
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

        on_event(HarnessEvent::Answer { text: &answer });
        Ok(HarnessOutcome { answer, cited, steps: steps_taken, trivial, metrics, state })
    }

    fn tool_set(&self, engine: &Engine, loaded: &HashSet<String>, cfg: MetaToolConfig) -> Vec<ark_model::ToolDef> {
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
                    return Ok(format!("no adapter named `{adapter}`. See the capability index."));
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
                    None => Ok(format!("no action named `{name}`. Use list_actions to see an adapter's actions.")),
                }
            }
            catalog::GET_EVIDENCE => {
                metrics.record_retrieval();
                let inv_ref = sarg("invocation");
                let path = a.get("path").and_then(|v| v.as_str());
                let limit = a.get("limit").and_then(|v| v.as_u64()).unwrap_or(40) as usize;
                on_event(HarnessEvent::Retrieval { what: format!("get_evidence {inv_ref}{}", path.map(|p| format!(".{p}")).unwrap_or_default()) });
                match engine.store.resolve_invocation(project, &inv_ref)? {
                    None => Ok(format!("no invocation matching `{inv_ref}`.")),
                    Some(inv) => Ok(extract_evidence(&inv.output, path, limit)),
                }
            }
            catalog::SEARCH_EVIDENCE => {
                metrics.record_retrieval();
                let query = sarg("query");
                let limit = a.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
                on_event(HarnessEvent::Retrieval { what: format!("search_evidence \"{query}\"") });
                let hits = engine.store.search(project, &query, limit)?;
                if hits.is_empty() {
                    return Ok(format!("no matches for \"{query}\"."));
                }
                let mut s = format!("{} match(es) for \"{query}\":\n", hits.len());
                for h in hits {
                    let _ = writeln!(s, "  art:{} {} ({}) score {:.2}", &h.artifact_id[..h.artifact_id.len().min(12)], h.name, h.kind, h.score);
                }
                Ok(s)
            }
            catalog::SEARCH_KNOWLEDGE => {
                metrics.record_retrieval();
                let query = sarg("query");
                let limit = a.get("limit").and_then(|v| v.as_u64()).unwrap_or(4) as usize;
                on_event(HarnessEvent::Retrieval { what: format!("search_knowledge \"{query}\"") });
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
                on_event(HarnessEvent::StateUpdate { kind: kind.clone(), text: text.clone() });
                Ok(format!("recorded {kind}."))
            }
            catalog::RECORD_NOTE => {
                let kind = sarg("kind");
                let body = sarg("body");
                let nk = note_kind_from_str(&kind);
                engine.store.put_note(project, &Note::new(nk, &body, Author::Agent))?;
                on_event(HarnessEvent::Note { kind: kind.clone(), body: body.clone() });
                Ok(format!("recorded {kind} note."))
            }
            catalog::PRESENT => {
                let view = sarg("view");
                if view.is_empty() {
                    return Ok("error: `present` needs a `view` (artifact, file, diff, …).".into());
                }
                let opt = |k: &str| a.get(k).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_string);
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
            catalog::FIND_CAPABILITY | catalog::PROPOSE_CAPABILITY | catalog::ACQUIRE_CAPABILITY => {
                match &profile.acquisition {
                    None => Ok("capability acquisition is not enabled for this agent.".into()),
                    Some(acq) => {
                        let need = sarg("need");
                        if need.is_empty() {
                            return Ok("error: this meta-tool needs a `need` (a capability slug or free-text description).".into());
                        }
                        let (which, out) = match call.name.as_str() {
                            catalog::FIND_CAPABILITY => ("find_capability", acq.find(&need).await),
                            catalog::PROPOSE_CAPABILITY => ("propose_capability", acq.propose(&need).await),
                            // Acquisition is gated by the same approval as a Mutate action.
                            _ => ("acquire_capability", acq.acquire(&need, self.allow_mutate).await.summary),
                        };
                        on_event(HarnessEvent::Capability { action: which, detail: out.clone() });
                        Ok(out)
                    }
                }
            }
            other => Ok(format!("error: unknown meta-tool `{other}`.")),
        }
    }

    /// One cheap-model call to draft a short plan. Returns its steps, or None on any failure.
    async fn draft_plan(&self, profile: &AgentProfile, question: &str, metrics: &mut Metrics) -> Option<Vec<String>> {
        let provider = self.router.provider(Role::Plan);
        let msgs = [Message::user(format!("Goal: {question}"))];
        let c = provider.complete(&profile.prompts.plan_system, &msgs, &[]).await.ok()?;
        metrics.record_call(Role::Plan, &provider.model_id(), c.usage);
        let steps: Vec<String> = c
            .content
            .lines()
            .map(|l| l.trim().trim_start_matches(['-', '*', '•']).trim())
            .map(|l| l.trim_start_matches(|ch: char| ch.is_ascii_digit() || ch == '.' || ch == ')').trim())
            .filter(|l| l.len() > 3)
            .take(6)
            .map(str::to_string)
            .collect();
        Some(steps)
    }

    async fn force_answer(
        &self,
        profile: &AgentProfile,
        ws: &mut WorkingSet,
        static_system: &str,
        index: &str,
        state: &ResearchState,
        metrics: &mut Metrics,
    ) -> Result<String, AgentError> {
        let system = format!("{static_system}\n\n{index}\n\n== WORKING STATE ==\n{}", state.digest(8));
        let (mut messages, est) = ws.assemble(&system);
        metrics.peak_prompt_tokens = metrics.peak_prompt_tokens.max(est);
        messages.push(Message::user(&profile.prompts.force_answer));
        let provider = self.router.provider(Role::Synthesize);
        let c = provider
            .complete(&system, &messages, &[])
            .await
            .map_err(|e| AgentError::Provider(e.to_string()))?;
        metrics.record_call(Role::Synthesize, &provider.model_id(), c.usage);
        Ok(c.content.trim().to_string())
    }

    /// Deterministic checks, no model call: did we answer, and is a factual answer grounded in
    /// cited evidence? Returns (passed, human detail).
    fn self_check(&self, answer: &str, cited: &[InvocationId], trivial: bool, steps: usize) -> (bool, String) {
        if answer.is_empty() {
            return (false, "no answer produced".into());
        }
        let admits_unknown = ["unknown", "cannot determine", "not enough", "no evidence", "unable to"]
            .iter()
            .any(|p| answer.to_ascii_lowercase().contains(p));
        let cites_something = answer.contains("inv:") || !cited.is_empty();
        if !trivial && !cites_something && !admits_unknown && steps > 1 {
            return (false, "answer is ungrounded: no evidence cited and no uncertainty acknowledged".into());
        }
        (true, format!("answered; {} evidence item(s) cited", cited.len()))
    }

    async fn run_critic(
        &self,
        profile: &AgentProfile,
        static_system: &str,
        state: &ResearchState,
        answer: &str,
        issue: &str,
        metrics: &mut Metrics,
    ) -> Option<String> {
        let sys = format!(
            "{static_system}\n\n{} (the check that failed: {issue})",
            profile.prompts.critic_intro
        );
        let provider = self.router.provider(Role::Critic);
        let msgs = [Message::user(format!(
            "== WORKING STATE ==\n{}\n\n== DRAFT ANSWER ==\n{answer}",
            state.digest(8)
        ))];
        let c = provider.complete(&sys, &msgs, &[]).await.ok()?;
        metrics.record_call(Role::Critic, &provider.model_id(), c.usage);
        let out = c.content.trim().to_string();
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }

    fn build_workspace_context(&self, engine: &Engine, project: &ProjectId) -> Result<WorkspaceContext, AgentError> {
        let label = engine
            .store
            .get_project(project)?
            .map(|p| if p.target.is_empty() { p.name.clone() } else { format!("{} — {}", p.name, p.target) })
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

    fn static_system(&self, engine: &Engine, profile: &AgentProfile, project: &ProjectId) -> Result<String, AgentError> {
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
            let _ = writeln!(s, "  - id={} name={} kind={}", a.id.as_str(), a.name, a.kind.as_tag());
        }
        let _ = write!(s, "\nActions that change state need explicit human approval; observation and analysis are free — reason from what tools return.");
        Ok(s)
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
        if inv.consumed.iter().chain(inv.produced.iter()).any(|a| a == artifact) {
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
    let mut s = format!("[inv:{}] {}: {}", card.invocation.short(), card.action, card.summary);
    if !card.identifiers.is_empty() {
        let shown: Vec<&String> = card.identifiers.iter().take(12).collect();
        let _ = write!(s, "\n  identifiers: {}", shown.iter().map(|x| x.as_str()).collect::<Vec<_>>().join(", "));
    }
    if !card.produced.is_empty() {
        let _ = write!(s, "\n  produced: {}", card.produced.iter().map(|p| format!("art:{}", p.short())).collect::<Vec<_>>().join(", "));
    }
    let _ = write!(s, "\n  (call get_evidence(\"inv:{}\") for exact rows)", card.invocation.short());
    s
}

fn extract_evidence(output: &serde_json::Value, path: Option<&str>, limit: usize) -> String {
    let mut cur = output;
    if let Some(p) = path {
        for seg in p.split('.').filter(|s| !s.is_empty()) {
            match cur.get(seg) {
                Some(v) => cur = v,
                None => return format!("no such path `{p}` in this evidence (top-level keys: {}).", top_keys(output)),
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
            let mut out: Vec<serde_json::Value> = a.iter().take(limit).map(|x| cap_arrays(x, limit)).collect();
            if a.len() > limit {
                out.push(json!(format!("… (+{} more of {})", a.len() - limit, a.len())));
            }
            serde_json::Value::Array(out)
        }
        serde_json::Value::Object(m) => {
            serde_json::Value::Object(m.iter().map(|(k, x)| (k.clone(), cap_arrays(x, limit))).collect())
        }
        other => other.clone(),
    }
}

fn top_keys(v: &serde_json::Value) -> String {
    v.as_object().map(|m| m.keys().cloned().collect::<Vec<_>>().join(", ")).unwrap_or_default()
}

fn apply_state_update(state: &mut ResearchState, kind: &str, text: &str, args: &serde_json::Value) {
    let cites: Vec<String> = args
        .get("cites")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    match kind {
        "finding" => {
            let ct = args.get("claim_type").and_then(|v| v.as_str()).map(ClaimType::from_str_lenient).unwrap_or(ClaimType::Inference);
            let mut c = Claim::new(text, ct);
            c.cites = cites.clone();
            state.findings.push(c);
            let f = state.graph.add_node(ark_core::NodeKind::Finding, text);
            for cite in &cites {
                let e = state.graph.add_node(ark_core::NodeKind::Evidence, cite.clone());
                state.graph.link(&e, &f, ark_core::EdgeRel::Supports);
            }
        }
        "hypothesis" => {
            state.hypotheses.push(Claim::new(text, ClaimType::Hypothesis));
            state.graph.add_node(ark_core::NodeKind::Hypothesis, text);
        }
        "reject" => {
            state.rejected.push(text.to_string());
            state.hypotheses.retain(|h| h.text != text);
            if let Some(id) = state.graph.nodes.iter().find(|n| n.kind == ark_core::NodeKind::Hypothesis && n.label.eq_ignore_ascii_case(text)).map(|n| n.id.clone()) {
                state.graph.link("rejected", &id, ark_core::EdgeRel::Contradicts);
            }
        }
        "interesting" => state.interesting.push(Interesting {
            kind: args.get("claim_type").and_then(|v| v.as_str()).unwrap_or("item").to_string(),
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
            if let Some(target) = args.get("tests").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                state.graph.link(&x, target, ark_core::EdgeRel::TestedBy);
            }
            for cite in &cites {
                let e = state.graph.add_node(ark_core::NodeKind::Evidence, cite.clone());
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
        assert_eq!(normalize_action_name("fs.read"), "fs.read", "already-dotted is untouched");
        assert_eq!(normalize_action_name("  web__get "), "web.get", "trimmed and unmangled");
        assert_eq!(normalize_action_name("strings"), "strings", "a bare adapter name is fine");
    }

    #[test]
    fn classify_trivial_uses_markers() {
        let markers = crate::profile::AgentProfile::default().investigation_markers;
        assert!(classify_trivial("what is the arch", &markers));
        assert!(!classify_trivial("why does the build fail and how is it configured", &markers));
        // A domain marker forces investigation.
        let custom = vec!["vulnerability".to_string()];
        assert!(!classify_trivial("any vulnerability here", &custom));
    }
}
