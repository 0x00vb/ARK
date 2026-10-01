//! # ark-bench
//!
//! Benchmark and eval harness for ARK applications. It answers one question: does a runtime
//! change make the agent better or cheaper?
//!
//! - [`Workload`]: what to ask an agent, how to build it, and how to judge the answer.
//! - [`record::Metered`]: wraps a provider and records every request (size by part, measured
//!   usage, latency, time to first token, prefix shared with earlier requests).
//! - [`sim`]: a simulated model whose policy reacts to what each request contains, so runs
//!   are deterministic and still sensitive to runtime changes.
//! - [`report`]: `baseline.json` / `candidate.json` / `comparison.json`.
//! - [`suite`]: the generic workloads. They name no application domain; an application keeps
//!   its own workloads and runs them through the same [`run_suite`].

pub mod record;
pub mod report;
pub mod sim;
pub mod suite;
pub mod workload;

use std::collections::BTreeMap;

pub use report::{compare, Comparison, Pricing, Report, RunRecord, Summary};
pub use workload::{
    BenchSettings, Env, Mode, ModelSpec, RunOutput, TurnResult, TurnSpec, Verdict, Workload,
};

/// Run every workload `reps` times and build the report.
pub async fn run_suite(
    workloads: &[Box<dyn Workload>],
    models: &ModelSpec,
    settings: &BenchSettings,
    reps: u32,
    label: &str,
    progress: impl FnMut(&RunRecord),
) -> anyhow::Result<Report> {
    run_suite_with(workloads, models, settings, reps, label, progress, |_| {}).await
}

/// [`run_suite`], also handing the report so far to `checkpoint` after every run, so an
/// interrupted live run keeps what it measured.
pub async fn run_suite_with(
    workloads: &[Box<dyn Workload>],
    models: &ModelSpec,
    settings: &BenchSettings,
    reps: u32,
    label: &str,
    mut progress: impl FnMut(&RunRecord),
    mut checkpoint: impl FnMut(&Report),
) -> anyhow::Result<Report> {
    let pricing = match models.mode() {
        Mode::Simulated => Pricing::reference(),
        Mode::Live => Pricing::for_model(&models.priced_model()),
    };
    let mut out: Vec<report::WorkloadReport> = Vec::new();
    for w in workloads {
        out.push(report::WorkloadReport {
            name: w.name().to_string(),
            class: w.class().to_string(),
            description: w.description().to_string(),
            summary: Summary::default(),
            runs: Vec::new(),
        });
        for rep in 0..reps {
            let r = workload::run(w.as_ref(), models, settings, rep, &pricing).await?;
            progress(&r);
            let last = out.last_mut().expect("pushed above");
            last.runs.push(r);
            let refs: Vec<&RunRecord> = last.runs.iter().collect();
            last.summary = Summary::of(&refs);
            checkpoint(&assemble(&out, models, settings, reps, label, &pricing));
        }
    }
    Ok(assemble(&out, models, settings, reps, label, &pricing))
}

fn assemble(
    out: &[report::WorkloadReport],
    models: &ModelSpec,
    settings: &BenchSettings,
    reps: u32,
    label: &str,
    pricing: &Pricing,
) -> Report {
    let all: Vec<&RunRecord> = out.iter().flat_map(|w| w.runs.iter()).collect();
    let mut s = BTreeMap::new();
    s.insert("planner".into(), settings.planner.to_string());
    s.insert("critic".into(), settings.critic.to_string());
    s.insert(
        "budget".into(),
        settings
            .budget
            .map(|b| b.to_string())
            .unwrap_or_else(|| "default".into()),
    );
    if let Some(t) = settings.turn_timeout {
        s.insert("turn_timeout_s".into(), t.as_secs().to_string());
    }
    Report {
        schema_version: report::SCHEMA_VERSION,
        label: label.to_string(),
        ark_version: env!("CARGO_PKG_VERSION").to_string(),
        created_at: time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default(),
        mode: models.mode().as_str().to_string(),
        model: models.strong.clone(),
        fast_model: models.fast.clone(),
        reps,
        pricing: pricing.clone(),
        settings: s,
        totals: Summary::of(&all),
        workloads: out.to_vec(),
    }
}
