//! The machine-readable benchmark output and its comparison.
//!
//! A run of the suite writes a [`Report`] (`baseline.json`, `candidate.json`); [`compare`]
//! turns two reports into a [`Comparison`] (`comparison.json`) with a before, after and delta
//! for every metric of every workload, and a list of regressions.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

/// Prices in USD per million tokens.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Pricing {
    pub input: f64,
    pub cached_input: f64,
    /// Price of input written to the cache (Anthropic: 1.25x input; OpenAI: input).
    #[serde(default)]
    pub cache_write: f64,
    pub output: f64,
    /// Where the prices come from ("list price of <model>", "reference", ...).
    pub basis: String,
}

impl Pricing {
    /// The reference used when the model has no price (local or simulated): Claude Sonnet 5.5
    /// list price. Cost then means "what these tokens would cost there".
    pub fn reference() -> Self {
        Pricing {
            input: 2.0,
            cached_input: 0.2,
            cache_write: 2.5,
            output: 10.0,
            basis: "reference: claude-sonnet-5-5 list price per Mtok ($2 in, $2.50 cache write, $0.20 cache read, $10 out)".into(),
        }
    }

    /// List price of a known hosted model, else the reference.
    pub fn for_model(model: &str) -> Self {
        // (id, input, cache read, cache write, output)
        let known = [
            ("claude-opus-5-5", 4.0, 0.2, 5.0, 20.0),
            ("claude-sonnet-5-5", 2.0, 0.2, 2.5, 10.0),
            ("claude-haiku-4-5", 1.0, 0.1, 1.25, 5.0),
            ("claude-fable-5-1", 10.0, 0.25, 12.5, 50.0),
            ("gpt-4o-mini", 0.15, 0.075, 0.15, 0.6),
        ];
        for (id, i, c, w, o) in known {
            if model == id {
                return Pricing {
                    input: i,
                    cached_input: c,
                    cache_write: w,
                    output: o,
                    basis: format!("list price of {id}"),
                };
            }
        }
        Self::reference()
    }

    /// `uncached` includes `written` (uncached tokens that were also written to the cache).
    pub fn cost(&self, uncached: u64, cached: u64, written: u64, output: u64) -> f64 {
        let plain = uncached.saturating_sub(written);
        (plain as f64 * self.input
            + written as f64 * self.cache_write
            + cached as f64 * self.cached_input
            + output as f64 * self.output)
            / 1_000_000.0
    }
}

/// Everything measured about one run of one workload.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RunRecord {
    pub workload: String,
    pub rep: u32,
    pub success: bool,
    pub verdict: String,
    /// The last turn's answer, clipped, for reading failures.
    #[serde(default)]
    pub answer: String,
    pub turns: u32,
    pub steps: u32,
    pub model_calls: u32,
    pub calls_by_role: BTreeMap<String, u32>,
    /// Input plus output tokens, by role.
    #[serde(default)]
    pub tokens_by_role: BTreeMap<String, u64>,
    pub calls_by_model: BTreeMap<String, u32>,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub uncached_input_tokens: u64,
    /// Uncached input tokens that were written to the cache (priced above normal input).
    #[serde(default)]
    pub cache_write_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    /// False when any number is an estimate or simulated rather than provider-reported.
    pub tokens_measured: bool,
    /// False when managed subscription sessions bypassed the direct provider recorder.
    /// Usage/outcomes still come from runtime events; request/meta/prefix metrics are absent.
    #[serde(default)]
    pub request_metrics_complete: bool,
    pub cost_usd: f64,
    pub wall_ms: u64,
    pub turn_ms: Vec<u64>,
    /// Per model call: milliseconds to the first streamed text (calls with text only).
    pub first_token_ms: Vec<u64>,
    pub model_latency_ms: Vec<u64>,
    /// Adapter tool calls that ran.
    pub tool_calls: u32,
    pub tool_ok: u32,
    pub tool_failed: u32,
    /// Calls served from an earlier identical result instead of running.
    pub deduplicated: u32,
    /// Executions of a call identical to one that already ran in this run, with no
    /// state-changing call approved in between.
    pub repeated_executions: u32,
    /// Runtime-handled calls (discovery, retrieval, state), by name.
    pub meta_calls: BTreeMap<String, u32>,
    pub provider_errors: u32,
    pub provider_retries: u32,
    pub approvals_requested: u32,
    pub stall_nudges: u32,
    pub stall_stops: u32,
    pub peak_context_tokens: u32,
    pub mean_context_tokens: u32,
    /// Estimated tokens per context source, summed over the run's reasoning requests.
    pub context_sources: BTreeMap<String, u64>,
    pub tool_schema_tokens: u64,
    pub tools_exposed_max: u32,
    /// Distinct adapter actions the run called.
    pub tools_used: u32,
    /// Tokens of distilled results that entered the context, and of the raw outputs behind them.
    pub result_context_tokens: u64,
    pub result_raw_tokens: u64,
    pub compressed_rounds_max: u32,
    /// Estimated tokens each request shared with a recent one (what a prefix cache can serve).
    pub prefix_reuse_tokens: u64,
    /// Faults injected into this run's model calls (`--faults`).
    #[serde(default)]
    pub faults_injected: u32,
    /// Requests a replay had no recorded reply for (`--replay`); 0 is a faithful replay.
    #[serde(default)]
    pub replay_diverged: u32,
}

/// Aggregates over a workload's runs (or over everything).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Summary {
    pub runs: u32,
    pub successes: u32,
    pub success_rate: f64,
    pub model_calls: f64,
    pub input_tokens: f64,
    pub cached_input_tokens: f64,
    pub uncached_input_tokens: f64,
    pub output_tokens: f64,
    pub total_tokens: f64,
    pub cost_usd: f64,
    /// Total cost over all runs divided by successful runs. `None` with no success.
    pub cost_per_success_usd: Option<f64>,
    pub tool_calls: f64,
    pub tool_failed: f64,
    pub deduplicated: f64,
    pub repeated_executions: f64,
    pub provider_retries: f64,
    pub provider_errors: f64,
    pub steps: f64,
    pub peak_context_tokens: f64,
    pub mean_context_tokens: f64,
    pub tool_schema_tokens: f64,
    pub prefix_reuse_tokens: f64,
    pub median_turn_ms: f64,
    pub p95_turn_ms: f64,
    pub median_first_token_ms: Option<f64>,
    pub stall_stops: f64,
}

impl Summary {
    pub fn of(runs: &[&RunRecord]) -> Summary {
        let n = runs.len().max(1) as f64;
        let mean = |f: &dyn Fn(&RunRecord) -> f64| runs.iter().map(|r| f(r)).sum::<f64>() / n;
        let successes = runs.iter().filter(|r| r.success).count() as u32;
        let total_cost: f64 = runs.iter().map(|r| r.cost_usd).sum();
        let mut turns: Vec<u64> = runs
            .iter()
            .flat_map(|r| r.turn_ms.iter().copied())
            .collect();
        let mut ttft: Vec<u64> = runs
            .iter()
            .flat_map(|r| r.first_token_ms.iter().copied())
            .collect();
        Summary {
            runs: runs.len() as u32,
            successes,
            success_rate: successes as f64 / n,
            model_calls: mean(&|r| r.model_calls as f64),
            input_tokens: mean(&|r| r.input_tokens as f64),
            cached_input_tokens: mean(&|r| r.cached_input_tokens as f64),
            uncached_input_tokens: mean(&|r| r.uncached_input_tokens as f64),
            output_tokens: mean(&|r| r.output_tokens as f64),
            total_tokens: mean(&|r| r.total_tokens as f64),
            cost_usd: mean(&|r| r.cost_usd),
            cost_per_success_usd: (successes > 0).then(|| total_cost / successes as f64),
            tool_calls: mean(&|r| r.tool_calls as f64),
            tool_failed: mean(&|r| r.tool_failed as f64),
            deduplicated: mean(&|r| r.deduplicated as f64),
            repeated_executions: mean(&|r| r.repeated_executions as f64),
            provider_retries: mean(&|r| r.provider_retries as f64),
            provider_errors: mean(&|r| r.provider_errors as f64),
            steps: mean(&|r| r.steps as f64),
            peak_context_tokens: runs
                .iter()
                .map(|r| r.peak_context_tokens as f64)
                .fold(0.0, f64::max),
            mean_context_tokens: mean(&|r| r.mean_context_tokens as f64),
            tool_schema_tokens: mean(&|r| r.tool_schema_tokens as f64),
            prefix_reuse_tokens: mean(&|r| r.prefix_reuse_tokens as f64),
            median_turn_ms: percentile(&mut turns, 0.5).unwrap_or(0.0),
            p95_turn_ms: percentile(&mut turns, 0.95).unwrap_or(0.0),
            median_first_token_ms: percentile(&mut ttft, 0.5),
            stall_stops: mean(&|r| r.stall_stops as f64),
        }
    }
}

/// Nearest-rank percentile.
pub fn percentile(v: &mut [u64], p: f64) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_unstable();
    let rank = ((p * v.len() as f64).ceil() as usize).clamp(1, v.len());
    Some(v[rank - 1] as f64)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkloadReport {
    pub name: String,
    pub class: String,
    pub description: String,
    pub summary: Summary,
    pub runs: Vec<RunRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    pub schema_version: u32,
    /// Free-form label for the run ("v0.3.0 baseline", a commit, ...).
    pub label: String,
    pub ark_version: String,
    pub created_at: String,
    /// "simulated" or "live".
    pub mode: String,
    pub model: String,
    pub fast_model: Option<String>,
    pub reps: u32,
    pub pricing: Pricing,
    /// Settings that change what is measured (budget, planner, critic, ...).
    pub settings: BTreeMap<String, String>,
    pub workloads: Vec<WorkloadReport>,
    pub totals: Summary,
}

/// One metric before and after.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Delta {
    pub metric: String,
    pub before: Option<f64>,
    pub after: Option<f64>,
    pub delta: Option<f64>,
    pub pct: Option<f64>,
    /// "better", "worse" or "same", given which direction is good for this metric.
    pub verdict: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkloadDelta {
    pub name: String,
    pub metrics: Vec<Delta>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Comparison {
    pub schema_version: u32,
    pub baseline: String,
    pub candidate: String,
    pub mode: String,
    pub totals: Vec<Delta>,
    pub workloads: Vec<WorkloadDelta>,
    /// Human-readable lines for every regression found.
    pub regressions: Vec<String>,
}

/// (name, value, higher-is-better).
type Metric = (&'static str, Option<f64>, bool);

fn metrics(s: &Summary) -> Vec<Metric> {
    vec![
        ("success_rate", Some(s.success_rate), true),
        ("cost_per_success_usd", s.cost_per_success_usd, false),
        ("cost_usd", Some(s.cost_usd), false),
        ("model_calls", Some(s.model_calls), false),
        ("input_tokens", Some(s.input_tokens), false),
        (
            "uncached_input_tokens",
            Some(s.uncached_input_tokens),
            false,
        ),
        ("cached_input_tokens", Some(s.cached_input_tokens), true),
        ("output_tokens", Some(s.output_tokens), false),
        ("total_tokens", Some(s.total_tokens), false),
        ("tool_calls", Some(s.tool_calls), false),
        ("tool_failed", Some(s.tool_failed), false),
        ("deduplicated", Some(s.deduplicated), true),
        ("repeated_executions", Some(s.repeated_executions), false),
        ("provider_retries", Some(s.provider_retries), false),
        ("steps", Some(s.steps), false),
        ("peak_context_tokens", Some(s.peak_context_tokens), false),
        ("mean_context_tokens", Some(s.mean_context_tokens), false),
        ("tool_schema_tokens", Some(s.tool_schema_tokens), false),
        ("prefix_reuse_tokens", Some(s.prefix_reuse_tokens), true),
        ("median_turn_ms", Some(s.median_turn_ms), false),
        ("p95_turn_ms", Some(s.p95_turn_ms), false),
        ("median_first_token_ms", s.median_first_token_ms, false),
        ("stall_stops", Some(s.stall_stops), false),
    ]
}

/// Relative change below which a metric counts as unchanged.
const NOISE: f64 = 0.005;

fn deltas(before: &Summary, after: &Summary) -> Vec<Delta> {
    metrics(before)
        .into_iter()
        .zip(metrics(after))
        .map(|((name, b, higher), (_, a, _))| {
            let delta = match (b, a) {
                (Some(b), Some(a)) => Some(a - b),
                _ => None,
            };
            let pct = match (b, delta) {
                (Some(b), Some(d)) if b.abs() > f64::EPSILON => Some(d / b * 100.0),
                _ => None,
            };
            let verdict = match (b, a, delta) {
                (Some(b), Some(_), Some(d)) => {
                    let rel = if b.abs() > f64::EPSILON {
                        d / b.abs()
                    } else {
                        d
                    };
                    if rel.abs() <= NOISE {
                        "same"
                    } else if (d > 0.0) == higher {
                        "better"
                    } else {
                        "worse"
                    }
                }
                _ => "same",
            };
            Delta {
                metric: name.to_string(),
                before: b,
                after: a,
                delta,
                pct,
                verdict: verdict.to_string(),
            }
        })
        .collect()
}

/// Metrics whose worsening is a regression. Latency is noisy and only reported.
fn gated(metric: &str, mode: &str) -> bool {
    match metric {
        "success_rate" => true,
        "repeated_executions" => true,
        // Token and call counts are deterministic in simulated runs; live runs are noisy.
        "model_calls"
        | "input_tokens"
        | "uncached_input_tokens"
        | "total_tokens"
        | "cost_per_success_usd"
        | "peak_context_tokens"
        | "tool_calls" => mode == "simulated",
        _ => false,
    }
}

pub fn compare(baseline: &Report, candidate: &Report) -> Comparison {
    let mode = candidate.mode.clone();
    let totals = deltas(&baseline.totals, &candidate.totals);
    let mut regressions = Vec::new();
    let mut workloads = Vec::new();
    for w in &candidate.workloads {
        let Some(b) = baseline.workloads.iter().find(|x| x.name == w.name) else {
            continue;
        };
        let ds = deltas(&b.summary, &w.summary);
        for d in &ds {
            if d.verdict == "worse" && gated(&d.metric, &mode) {
                regressions.push(format!(
                    "{}: {} {} -> {}",
                    w.name,
                    d.metric,
                    fmt_opt(d.before),
                    fmt_opt(d.after)
                ));
            }
        }
        workloads.push(WorkloadDelta {
            name: w.name.clone(),
            metrics: ds,
        });
    }
    if baseline.mode != candidate.mode {
        regressions.push(format!(
            "modes differ ({} vs {}): numbers are not comparable",
            baseline.mode, candidate.mode
        ));
    }
    Comparison {
        schema_version: SCHEMA_VERSION,
        baseline: baseline.label.clone(),
        candidate: candidate.label.clone(),
        mode,
        totals,
        workloads,
        regressions,
    }
}

fn fmt_opt(v: Option<f64>) -> String {
    match v {
        None => "n/a".into(),
        Some(x) if x.fract() == 0.0 => format!("{x:.0}"),
        Some(x) if x.abs() < 1.0 => format!("{x:.6}"),
        Some(x) if x.abs() < 100.0 => format!("{x:.3}"),
        Some(x) => format!("{x:.1}"),
    }
}

/// A Markdown table of the totals, for humans.
pub fn markdown(c: &Comparison) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(s, "| Metric | {} | {} | Delta |", c.baseline, c.candidate);
    let _ = writeln!(s, "|---|---:|---:|---:|");
    for d in &c.totals {
        let pct = d.pct.map(|p| format!(" ({p:+.1}%)")).unwrap_or_default();
        let _ = writeln!(
            s,
            "| {} | {} | {} | {}{} |",
            d.metric,
            fmt_opt(d.before),
            fmt_opt(d.after),
            d.delta
                .map(|x| fmt_opt(Some(x)))
                .unwrap_or_else(|| "n/a".into()),
            pct
        );
    }
    let _ = writeln!(
        s,
        "\n| Workload | Success | Model calls | Total tokens | Uncached input | Cost/success (USD) |"
    );
    let _ = writeln!(s, "|---|---:|---:|---:|---:|---:|");
    for w in &c.workloads {
        let get = |m: &str| w.metrics.iter().find(|d| d.metric == m);
        let cell = |m: &str| match get(m) {
            Some(d) if d.before == d.after => fmt_opt(d.after),
            Some(d) => format!("{} -> {}", fmt_opt(d.before), fmt_opt(d.after)),
            None => "n/a".into(),
        };
        let _ = writeln!(
            s,
            "| {} | {} | {} | {} | {} | {} |",
            w.name,
            cell("success_rate"),
            cell("model_calls"),
            cell("total_tokens"),
            cell("uncached_input_tokens"),
            cell("cost_per_success_usd")
        );
    }
    if !c.regressions.is_empty() {
        let _ = writeln!(s, "\nRegressions:");
        for r in &c.regressions {
            let _ = writeln!(s, "- {r}");
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(success: f64, tokens: f64) -> Summary {
        Summary {
            success_rate: success,
            total_tokens: tokens,
            ..Default::default()
        }
    }

    #[test]
    fn fewer_tokens_is_better_and_lower_success_is_a_regression() {
        let d = deltas(&summary(1.0, 1000.0), &summary(0.5, 800.0));
        let tokens = d.iter().find(|x| x.metric == "total_tokens").unwrap();
        assert_eq!(tokens.verdict, "better");
        assert_eq!(tokens.pct, Some(-20.0));
        let success = d.iter().find(|x| x.metric == "success_rate").unwrap();
        assert_eq!(success.verdict, "worse");
        assert!(gated("success_rate", "live"));
    }

    #[test]
    fn percentile_is_nearest_rank() {
        let mut v = vec![10, 20, 30, 40, 100];
        assert_eq!(percentile(&mut v, 0.5), Some(30.0));
        assert_eq!(percentile(&mut v, 0.95), Some(100.0));
        assert_eq!(percentile(&mut [], 0.5), None);
    }
}
