//! Live measurement (ADR 0043 › Which agent does the work): the same tasks on ARK's own agent
//! and on an agent program, same model, same folder, every question answered "once".
//!
//! `ARK_BENCH_RUNS=3 cargo test -p ark-orchestrate --test worker_bench -- --ignored --nocapture`
//! needs Claude Code signed in (ARK's agent runs on `claude:sonnet`, the program on
//! `agent:claude-code:sonnet`). Prints one line per run and a table; cost is priced from
//! tokens at Sonnet's list price for both, and the program's own reported cost is shown too.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ark_orchestrate::*;
use ark_sdk::prelude::*;

/// Answers every question "once", counting them.
struct Once(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Approver for Once {
    async fn decide(&self, _: &ApprovalRequest) -> ApprovalDecision {
        self.0.fetch_add(1, Ordering::SeqCst);
        ApprovalDecision::AllowOnce
    }
}

struct Workload {
    name: &'static str,
    objective: &'static str,
    setup: fn(&std::path::Path),
    check: fn(&std::path::Path) -> bool,
}

const NOTES: &str = "# Team notes\n\nPlanning meeting: 2026-03-04, room B.\nBudget review: 2026-03-18.\nOwner: Dana.\n";

fn workloads() -> Vec<Workload> {
    vec![
        Workload {
            name: "analysis",
            objective: "Read sales.csv in the project folder. Create report.md in the same folder \
with one line: the month with the highest sales and its number, like `jul: 61`.",
            setup: |ws| {
                let rows = [
                    ("jan", 10),
                    ("feb", 32),
                    ("mar", 18),
                    ("apr", 25),
                    ("may", 40),
                    ("jun", 22),
                    ("jul", 61),
                    ("aug", 47),
                    ("sep", 15),
                    ("oct", 33),
                    ("nov", 29),
                    ("dec", 52),
                ];
                let mut csv = String::from("month,sales\n");
                for (m, n) in rows {
                    csv.push_str(&format!("{m},{n}\n"));
                }
                std::fs::write(ws.join("sales.csv"), csv).unwrap();
            },
            check: |ws| {
                std::fs::read_to_string(ws.join("report.md"))
                    .is_ok_and(|r| r.to_lowercase().contains("jul") && r.contains("61"))
            },
        },
        Workload {
            name: "edit",
            objective:
                "In notes.md, change the planning meeting date from 2026-03-04 to 2026-03-11. \
Change nothing else in the file.",
            setup: |ws| std::fs::write(ws.join("notes.md"), NOTES).unwrap(),
            check: |ws| {
                std::fs::read_to_string(ws.join("notes.md"))
                    .is_ok_and(|n| n == NOTES.replace("2026-03-04", "2026-03-11"))
            },
        },
    ]
}

#[derive(Debug)]
struct Run {
    workload: &'static str,
    agent: &'static str,
    ok: bool,
    secs: f64,
    tokens: u64,
    priced: f64,
    reported: f64,
    asked: usize,
}

async fn run(w: &Workload, agent: &'static str) -> Run {
    let dir = tempfile::tempdir().unwrap();
    let asked = Arc::new(AtomicUsize::new(0));
    let ark = Ark::builder()
        .store_root(dir.path())
        // Every agent names its own model; the app's default is never called.
        .provider(Arc::new(ark_model::MockProvider::new("unused", vec![])))
        .files(true)
        .workers(true)
        .build()
        .unwrap();
    let project = ark.find_or_create_project("p", "").unwrap();
    let ws = ark.engine().workspace_dir(&project.id).unwrap();
    (w.setup)(&ws);
    let o = Orchestrator::builder(&ark, &project)
        .approvals(Arc::new(ApprovalGate::new(Arc::new(Once(asked.clone())))))
        .build()
        .unwrap();
    let spec = match agent {
        "ark" => AgentSpec::new("a")
            .model("claude:sonnet")
            .tools(["files.*"]),
        _ => AgentSpec::new("a").model("agent:claude-code:sonnet"),
    };
    o.spawn(spec).await.unwrap();
    let started = Instant::now();
    let id = o.submit("a", TaskSpec::new(w.objective)).unwrap();
    let done = tokio::time::timeout(Duration::from_secs(600), o.wait(&id))
        .await
        .expect("finished in 10 minutes")
        .unwrap();
    let u = done.usage;
    let uncached = u.prompt_tokens.saturating_sub(u.cached_tokens);
    let priced =
        (uncached as f64 * 2.0 + u.cached_tokens as f64 * 0.2 + u.completion_tokens as f64 * 10.0)
            / 1e6;
    let r = Run {
        workload: w.name,
        agent,
        ok: done.state == TaskState::Done && (w.check)(&ws),
        secs: started.elapsed().as_secs_f64(),
        tokens: u.tokens(),
        priced,
        reported: u.cost_micro_usd as f64 / 1e6,
        asked: asked.load(Ordering::SeqCst),
    };
    println!("{r:?} error={:?}", done.error);
    r
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: needs Claude Code signed in"]
async fn ark_agent_and_claude_code_on_the_same_tasks() {
    let runs: usize = std::env::var("ARK_BENCH_RUNS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(3);
    let mut all = Vec::new();
    for w in workloads() {
        for _ in 0..runs {
            for agent in ["ark", "claude-code"] {
                all.push(run(&w, agent).await);
            }
        }
    }
    println!("\n| Workload | Agent | Success | Median time (s) | Median tokens | Priced cost/success ($) | Reported cost ($) | Asked per run |");
    println!("|---|---|---:|---:|---:|---:|---:|---:|");
    for w in workloads() {
        for agent in ["ark", "claude-code"] {
            let rs: Vec<&Run> = all
                .iter()
                .filter(|r| r.workload == w.name && r.agent == agent)
                .collect();
            let ok = rs.iter().filter(|r| r.ok).count();
            let med = |mut v: Vec<f64>| {
                v.sort_by(|a, b| a.partial_cmp(b).unwrap());
                v[v.len() / 2]
            };
            let priced: f64 = rs.iter().map(|r| r.priced).sum();
            let reported: f64 = rs.iter().map(|r| r.reported).sum();
            println!(
                "| {} | {} | {}/{} | {:.1} | {:.0} | {} | {} | {:.1} |",
                w.name,
                agent,
                ok,
                rs.len(),
                med(rs.iter().map(|r| r.secs).collect()),
                med(rs.iter().map(|r| r.tokens as f64).collect()),
                if ok > 0 {
                    format!("{:.4}", priced / ok as f64)
                } else {
                    "-".into()
                },
                if reported > 0.0 {
                    format!("{:.4}", reported / rs.len() as f64)
                } else {
                    "-".into()
                },
                rs.iter().map(|r| r.asked).sum::<usize>() as f64 / rs.len() as f64,
            );
        }
    }
}
