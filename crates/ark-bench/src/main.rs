//! `ark-bench`: run the generic workload suite and compare reports.
//!
//! ```text
//! ark-bench run --out baseline.json                       # simulated, deterministic
//! ark-bench run --model ollama:qwen3:4b-instruct --reps 3 --out live.json
//! ark-bench compare baseline.json candidate.json --out comparison.json
//! ark-bench run --harness naive --out naive.json          # the naive baseline loop
//! ark-bench run --faults overloaded=0.1,bad-tool=0.05 --seed 7 --out faults.json
//! ark-bench run --model claude:haiku --record live.jsonl --out live.json
//! ark-bench run --model claude:haiku --replay live.jsonl --out replay.json
//! ```

use std::path::PathBuf;

use ark_bench::cassette::Cassette;
use ark_bench::faults::FaultPlan;
use ark_bench::{compare, report, suite, BenchSettings, HarnessKind, ModelSpec, Report};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ark-bench", about = "Benchmark an ARK agent and compare runs")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

// Parsed once per process: the size of the largest variant does not matter.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum Cmd {
    /// Run the suite and write a report.
    Run {
        /// `sim` (simulated model) or `provider:model`.
        #[arg(long, default_value = "sim")]
        model: String,
        /// A cheaper model for the fast tier (`provider:model`).
        #[arg(long)]
        fast: Option<String>,
        #[arg(long, default_value_t = 1)]
        reps: u32,
        /// Comma-separated workload names.
        #[arg(long)]
        only: Option<String>,
        /// Turns in the long-session workload (live runs cap it at 12).
        #[arg(long, default_value_t = 500)]
        long_turns: usize,
        /// Draft a plan before a non-trivial task (off by default).
        #[arg(long)]
        planner: bool,
        /// The default; kept so older commands still run.
        #[arg(long, conflicts_with = "planner")]
        no_planner: bool,
        #[arg(long)]
        no_critic: bool,
        /// Context budget in tokens (default: the runtime's).
        #[arg(long)]
        budget: Option<u32>,
        /// Seconds a turn may take before the run counts as failed.
        #[arg(long)]
        turn_timeout: Option<u64>,
        #[arg(long, default_value = "run")]
        label: String,
        #[arg(long)]
        out: Option<PathBuf>,
        /// `ark` (default) or `naive`, the baseline loop.
        #[arg(long, default_value = "ark")]
        harness: String,
        /// Faults to inject into model calls: `overloaded=0.1,malformed=0.05,bad-tool=0.05`.
        #[arg(long)]
        faults: Option<String>,
        /// Seed of the fault schedule.
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// Keep every model reply in this file (JSON lines), to replay later.
        #[arg(long, conflicts_with = "replay")]
        record: Option<PathBuf>,
        /// Serve model replies from a recording instead of calling the model.
        #[arg(long)]
        replay: Option<PathBuf>,
        /// Write each run's requests and events to `<dir>/<workload>-<rep>.jsonl`.
        #[arg(long)]
        traces: Option<PathBuf>,
    },
    /// Compare two reports.
    Compare {
        baseline: PathBuf,
        candidate: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
        /// Print a Markdown table of the totals.
        #[arg(long)]
        md: bool,
    },
    /// List the workloads.
    List,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match Cli::parse().cmd {
        Cmd::List => {
            for w in suite::all(500) {
                println!("{:<18} {:<16} {}", w.name(), w.class(), w.description());
            }
        }
        Cmd::Run {
            model,
            fast,
            reps,
            only,
            long_turns,
            planner,
            no_planner,
            no_critic,
            budget,
            turn_timeout,
            label,
            out,
            harness,
            faults,
            seed,
            record,
            replay,
            traces,
        } => {
            let harness = HarnessKind::parse(&harness).map_err(anyhow::Error::msg)?;
            let faults = faults
                .map(|f| FaultPlan::parse(&f, seed))
                .transpose()
                .map_err(anyhow::Error::msg)?;
            let cassette = match (&record, &replay) {
                (Some(_), _) => Some(Cassette::record()),
                (_, Some(path)) => Some(Cassette::load(path)?),
                _ => None,
            };
            let mut workloads = suite::all(long_turns);
            if let Some(only) = only {
                let names: Vec<&str> = only.split(',').map(str::trim).collect();
                workloads.retain(|w| names.contains(&w.name()));
            }
            let models = ModelSpec {
                strong: model,
                fast,
            };
            let settings = BenchSettings {
                planner: planner && !no_planner,
                critic: !no_critic,
                budget,
                turn_timeout: turn_timeout.map(std::time::Duration::from_secs),
                harness,
                faults,
                cassette: cassette.clone(),
                traces,
            };
            // Write the report after every run, so an interrupted run keeps its data.
            let save = |r: &Report| {
                if let Some(path) = &out {
                    if let Ok(json) = serde_json::to_string_pretty(r) {
                        let _ = std::fs::write(path, json);
                    }
                }
                if let (Some(c), Some(path)) = (&cassette, &record) {
                    let _ = c.save(path);
                }
            };
            let progress = |r: &ark_bench::RunRecord| {
                eprintln!(
                    "{:<18} rep {} {} calls={} tokens={} ({} cached) tools={} {}ms  {}",
                    r.workload,
                    r.rep,
                    if r.success { "PASS" } else { "FAIL" },
                    r.model_calls,
                    r.total_tokens,
                    r.cached_input_tokens,
                    r.tool_calls,
                    r.wall_ms,
                    r.verdict
                );
            };
            let r = ark_bench::run_suite_with(
                &workloads, &models, &settings, reps, &label, progress, save,
            )
            .await?;
            print_summary(&r);
            if let (Some(c), Some(path)) = (&cassette, &record) {
                c.save(path)?;
                eprintln!("recorded {}", path.display());
            }
            if let Some(path) = out {
                std::fs::write(&path, serde_json::to_string_pretty(&r)?)?;
                eprintln!("wrote {}", path.display());
            }
        }
        Cmd::Compare {
            baseline,
            candidate,
            out,
            md,
        } => {
            let b: Report = serde_json::from_str(&std::fs::read_to_string(&baseline)?)?;
            let c: Report = serde_json::from_str(&std::fs::read_to_string(&candidate)?)?;
            let cmp = compare(&b, &c);
            if md {
                print!("{}", report::markdown(&cmp));
            } else {
                println!("{}", serde_json::to_string_pretty(&cmp)?);
            }
            if let Some(path) = out {
                std::fs::write(&path, serde_json::to_string_pretty(&cmp)?)?;
            }
            if !cmp.regressions.is_empty() {
                std::process::exit(2);
            }
        }
    }
    Ok(())
}

fn print_summary(r: &Report) {
    println!(
        "\n{} · {} · model {} · {} rep(s)",
        r.label, r.mode, r.model, r.reps
    );
    println!(
        "{:<18} {:>7} {:>6} {:>9} {:>9} {:>6} {:>8} {:>10}",
        "workload", "success", "calls", "tokens", "uncached", "tools", "peak ctx", "cost/succ"
    );
    let row = |name: &str, s: &ark_bench::Summary| {
        println!(
            "{:<18} {:>7.2} {:>6.1} {:>9.0} {:>9.0} {:>6.1} {:>8.0} {:>10}",
            name,
            s.success_rate,
            s.model_calls,
            s.total_tokens,
            s.uncached_input_tokens,
            s.tool_calls,
            s.peak_context_tokens,
            s.cost_per_success_usd
                .map(|c| format!("${c:.5}"))
                .unwrap_or_else(|| "n/a".into())
        );
    };
    for w in &r.workloads {
        row(&w.name, &w.summary);
    }
    row("TOTAL", &r.totals);
}
