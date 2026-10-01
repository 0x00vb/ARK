//! Workloads and the runner. A [`Workload`] builds an ARK agent, says what to ask it (one or
//! more turns, possibly across a restart), supplies a simulated model for deterministic runs,
//! and judges the result. [`run`] executes one repetition and turns everything it observed
//! into a [`RunRecord`].

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use ark_sdk::prelude::ark_model::Provider;
use ark_sdk::prelude::ark_runtime::{call_digest, catalog};
use ark_sdk::prelude::*;

use crate::record::{Metered, Recorder};
use crate::report::{Pricing, RunRecord};
use crate::sim::{Policy, SimProvider};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The simulated model: deterministic, no network.
    Simulated,
    /// A real model.
    Live,
}

impl Mode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::Simulated => "simulated",
            Mode::Live => "live",
        }
    }
}

/// Which models to run. `"sim"` is the simulated model; anything else is
/// `provider:model` (for example `ollama:qwen3:4b-instruct` or `anthropic:claude-sonnet-5-5`).
#[derive(Clone, Debug)]
pub struct ModelSpec {
    pub strong: String,
    pub fast: Option<String>,
}

impl ModelSpec {
    pub fn simulated() -> Self {
        ModelSpec {
            strong: "sim".into(),
            fast: None,
        }
    }
    pub fn mode(&self) -> Mode {
        if self.strong == "sim" {
            Mode::Simulated
        } else {
            Mode::Live
        }
    }
    /// The model id cost is priced at.
    pub fn priced_model(&self) -> String {
        self.strong
            .split_once(':')
            .map(|(_, m)| m.to_string())
            .unwrap_or_else(|| self.strong.clone())
    }
}

/// Runtime settings applied to every workload (so a comparison can flip one).
#[derive(Clone, Debug)]
pub struct BenchSettings {
    pub planner: bool,
    pub critic: bool,
    pub budget: Option<u32>,
}

impl Default for BenchSettings {
    fn default() -> Self {
        BenchSettings {
            planner: true,
            critic: true,
            budget: None,
        }
    }
}

/// What a workload builds its agent from.
pub struct Env {
    pub root: PathBuf,
    pub mode: Mode,
    router: ModelRouter,
    settings: BenchSettings,
}

impl Env {
    /// An [`ArkBuilder`] with the store root, the metered models and the bench settings
    /// already set. Add tools and a profile, then build.
    pub fn builder(&self) -> ArkBuilder {
        let mut b = Ark::builder()
            .store_root(&self.root)
            .router(self.router.clone_view())
            .planner(self.settings.planner)
            .critic(self.settings.critic);
        if let Some(budget) = self.settings.budget {
            b = b.context_budget(budget);
        }
        b
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// One turn to run. `restart` drops the agent and builds a new one on the same store before
/// this turn (a process boundary), then reopens the latest session.
#[derive(Clone, Debug)]
pub struct TurnSpec {
    pub input: String,
    pub restart: bool,
}

impl TurnSpec {
    pub fn new(input: impl Into<String>) -> Self {
        TurnSpec {
            input: input.into(),
            restart: false,
        }
    }
    pub fn after_restart(input: impl Into<String>) -> Self {
        TurnSpec {
            input: input.into(),
            restart: true,
        }
    }
}

/// What one turn produced.
pub struct TurnResult {
    pub input: String,
    pub answer: String,
    pub cited: usize,
    pub steps: usize,
    pub cancelled: bool,
    pub error: Option<String>,
    pub events: Vec<HarnessEvent>,
    pub ms: u64,
}

impl TurnResult {
    /// Adapter calls that ran in this turn, by action name.
    pub fn executed(&self, action: &str) -> usize {
        self.events
            .iter()
            .filter(|e| matches!(e, HarnessEvent::ToolResult { action: a, .. } if a == action))
            .count()
    }
}

/// Everything a check can look at.
pub struct RunOutput<'a> {
    pub turns: &'a [TurnResult],
    pub ark: &'a Ark,
    pub project: &'a ProjectId,
}

impl RunOutput<'_> {
    pub fn last(&self) -> &TurnResult {
        self.turns.last().expect("at least one turn")
    }
    pub fn answer(&self) -> &str {
        &self.last().answer
    }
    pub fn executed(&self, action: &str) -> usize {
        self.turns.iter().map(|t| t.executed(action)).sum()
    }
    pub fn model_calls(&self) -> usize {
        self.turns
            .iter()
            .flat_map(|t| &t.events)
            .filter(|e| matches!(e, HarnessEvent::ModelCall { .. }))
            .count()
    }
}

/// Did the run succeed, and why (or why not).
#[derive(Clone, Debug)]
pub struct Verdict {
    pub success: bool,
    pub reason: String,
}

impl Verdict {
    pub fn pass(reason: impl Into<String>) -> Self {
        Verdict {
            success: true,
            reason: reason.into(),
        }
    }
    pub fn fail(reason: impl Into<String>) -> Self {
        Verdict {
            success: false,
            reason: reason.into(),
        }
    }
    /// Pass when every `(ok, why)` holds; otherwise fail with the first reason that does not.
    pub fn all(checks: &[(bool, &str)]) -> Self {
        match checks.iter().find(|(ok, _)| !ok) {
            Some((_, why)) => Verdict::fail(*why),
            None => Verdict::pass("all checks passed"),
        }
    }
}

pub trait Workload: Send + Sync {
    fn name(&self) -> &str;
    /// The behavior it exercises ("simple", "tool-lookup", ...).
    fn class(&self) -> &str;
    fn description(&self) -> &str;
    fn turns(&self, mode: Mode) -> Vec<TurnSpec>;
    /// The simulated model (used in simulated mode only).
    fn policy(&self) -> Policy;
    /// Build the agent. Called once, and again after every restart, on the same root.
    fn build(&self, env: &Env) -> anyhow::Result<Ark>;
    /// Seed the store before the first turn. The default creates an empty project.
    fn prepare(&self, ark: &Ark) -> anyhow::Result<ProjectId> {
        Ok(ark.find_or_create_project("bench", "")?.id)
    }
    fn check(&self, out: &RunOutput) -> Verdict;
}

fn build_provider(
    spec: &str,
    policy: &Policy,
    recorder: &Arc<Recorder>,
) -> anyhow::Result<Arc<dyn Provider>> {
    if spec == "sim" || spec == "sim-fast" {
        return Ok(SimProvider::shared(
            spec,
            policy.clone(),
            Some(recorder.clone()),
        ));
    }
    let (provider, model) = spec
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("model spec `{spec}` is not provider:model"))?;
    Ok(ark_model::build(provider, Some(model.to_string()))?)
}

/// Run one repetition of a workload and measure it.
pub async fn run(
    w: &dyn Workload,
    models: &ModelSpec,
    settings: &BenchSettings,
    rep: u32,
    pricing: &Pricing,
) -> anyhow::Result<RunRecord> {
    let dir = tempfile::tempdir()?;
    let mode = models.mode();
    let recorder = Recorder::new();
    let policy = w.policy();
    let strong = Metered::wrap(
        build_provider(&models.strong, &policy, &recorder)?,
        recorder.clone(),
    );
    let router = match &models.fast {
        Some(fast) => ModelRouter::new(
            Metered::wrap(build_provider(fast, &policy, &recorder)?, recorder.clone()),
            strong,
        ),
        None => ModelRouter::single(strong),
    };
    let env = Env {
        root: dir.path().to_path_buf(),
        mode,
        router,
        settings: settings.clone(),
    };

    let mut ark = w.build(&env)?;
    let project = w.prepare(&ark)?;
    let mut session = ark.start_session(&project)?;
    let approver: Arc<dyn Approver> = Arc::new(FixedApprover(ApprovalDecision::AllowOnce));

    let mut turns = Vec::new();
    for spec in w.turns(mode) {
        if spec.restart {
            drop(ark);
            ark = w.build(&env)?;
            session = ark
                .latest_session(&project)?
                .ok_or_else(|| anyhow::anyhow!("no session to resume"))?;
        }
        let mut events = Vec::new();
        let opts = TurnOptions {
            approvals: Some(Arc::new(ApprovalGate::new(approver.clone()))),
            ..Default::default()
        };
        let started = Instant::now();
        let out = ark
            .ask_session(&mut session, &spec.input, opts, |e| events.push(e))
            .await;
        let ms = started.elapsed().as_millis() as u64;
        let failed = out.is_err();
        turns.push(match out {
            Ok(o) => TurnResult {
                input: spec.input.clone(),
                answer: o.answer.clone(),
                cited: o.cited.len(),
                steps: o.steps,
                cancelled: o.cancelled,
                error: None,
                events,
                ms,
            },
            Err(e) => TurnResult {
                input: spec.input.clone(),
                answer: String::new(),
                cited: 0,
                steps: 0,
                cancelled: false,
                error: Some(e.to_string()),
                events,
                ms,
            },
        });
        if failed {
            break;
        }
    }

    let verdict = match turns.iter().find_map(|t| t.error.clone()) {
        Some(e) => Verdict::fail(format!("turn failed: {e}")),
        None => w.check(&RunOutput {
            turns: &turns,
            ark: &ark,
            project: &project,
        }),
    };
    Ok(measure(w, rep, &turns, &recorder, verdict, pricing))
}

fn measure(
    w: &dyn Workload,
    rep: u32,
    turns: &[TurnResult],
    recorder: &Recorder,
    verdict: Verdict,
    pricing: &Pricing,
) -> RunRecord {
    let mut r = RunRecord {
        workload: w.name().to_string(),
        rep,
        success: verdict.success,
        verdict: verdict.reason,
        turns: turns.len() as u32,
        tokens_measured: true,
        ..Default::default()
    };
    let mut pending: HashMap<String, String> = HashMap::new();
    let mut executed: HashSet<String> = HashSet::new();
    let mut used: HashSet<String> = HashSet::new();
    for t in turns {
        r.steps += t.steps as u32;
        r.wall_ms += t.ms;
        r.turn_ms.push(t.ms);
        for e in &t.events {
            match e {
                HarnessEvent::ModelCall {
                    role,
                    model,
                    usage,
                    elapsed_ms,
                    first_token_ms,
                    retries,
                } => {
                    r.model_calls += 1;
                    *r.calls_by_role.entry(role.clone()).or_default() += 1;
                    *r.calls_by_model.entry(model.clone()).or_default() += 1;
                    r.input_tokens += usage.prompt_tokens as u64;
                    r.cached_input_tokens += usage.cached_tokens as u64;
                    r.output_tokens += usage.completion_tokens as u64;
                    r.tokens_measured &= usage.measured;
                    r.model_latency_ms.push(*elapsed_ms);
                    if let Some(f) = first_token_ms {
                        r.first_token_ms.push(*f);
                    }
                    r.provider_retries += retries;
                }
                HarnessEvent::ToolCall { id, action, input } => {
                    pending.insert(id.clone(), call_digest(action, input));
                }
                HarnessEvent::ToolResult {
                    id,
                    action,
                    ok,
                    context_tokens,
                    raw_tokens,
                    ..
                } => {
                    r.tool_calls += 1;
                    if *ok {
                        r.tool_ok += 1;
                    } else {
                        r.tool_failed += 1;
                    }
                    used.insert(action.clone());
                    r.result_context_tokens += *context_tokens as u64;
                    r.result_raw_tokens += *raw_tokens as u64;
                    if let Some(d) = pending.remove(id) {
                        if !executed.insert(d) {
                            r.repeated_executions += 1;
                        }
                    }
                }
                HarnessEvent::Duplicate { .. } => r.deduplicated += 1,
                HarnessEvent::ApprovalRequested { .. } => r.approvals_requested += 1,
                HarnessEvent::Stall { stopped, .. } => {
                    if *stopped {
                        r.stall_stops += 1;
                    } else {
                        r.stall_nudges += 1;
                    }
                }
                HarnessEvent::Context { report } => {
                    for (k, v) in report.rows() {
                        *r.context_sources.entry(k.to_string()).or_default() += v as u64;
                    }
                    *r.context_sources.entry("Tool schemas".into()).or_default() +=
                        report.tool_schemas as u64;
                    r.compressed_rounds_max =
                        r.compressed_rounds_max.max(report.compressed_rounds as u32);
                }
                _ => {}
            }
        }
    }
    r.tools_used = used.len() as u32;
    r.cache_write_tokens = recorder.cache_writes();
    r.uncached_input_tokens = r.input_tokens.saturating_sub(r.cached_input_tokens);
    r.total_tokens = r.input_tokens + r.output_tokens;
    r.cost_usd = pricing.cost(
        r.uncached_input_tokens,
        r.cached_input_tokens,
        r.cache_write_tokens,
        r.output_tokens,
    );

    let records = recorder.records();
    let mut meta: BTreeMap<String, u32> = BTreeMap::new();
    let mut sum_ctx = 0u64;
    for q in &records {
        r.peak_context_tokens = r.peak_context_tokens.max(q.est_prompt_tokens);
        sum_ctx += q.est_prompt_tokens as u64;
        r.tool_schema_tokens += q.tool_schema_tokens as u64;
        r.tools_exposed_max = r.tools_exposed_max.max(q.tools);
        r.prefix_reuse_tokens += q.prefix_reuse_tokens as u64;
        if q.error.is_some() {
            r.provider_errors += 1;
        }
        for c in &q.calls {
            if catalog::is_meta_tool(c) {
                *meta.entry(c.clone()).or_default() += 1;
            }
        }
    }
    r.meta_calls = meta;
    r.mean_context_tokens = if records.is_empty() {
        0
    } else {
        (sum_ctx / records.len() as u64) as u32
    };
    r
}
