//! ADR 0122 on a real model. Ignored: it calls a live model (`claude:haiku` is enough).
//!
//! ```text
//! ARK_LIVE_MODEL=claude:haiku ARK_LIVE_OUT=<folder> \
//!   cargo test -p ark-orchestrate --test live_adr0122 -- --ignored --nocapture
//! ```
//!
//! A. The authority line: a named agent with `allow_mutate` off is asked, as a person turn, to
//!    make a file. It passes when the model calls the file action (the approver records the
//!    request, then allows it) and the file exists.
//! B. The step note nudge: a started task (not a chat turn) reads three files and writes one,
//!    three times. It passes when no run ends `Done` without the file.
//!
//! The agent runs on the app's model as in the apps: a managed session of its own. What it did
//! is read from the orchestrator's live events (steps, streamed text, calls). A step with no
//! call that another step follows is a nudge of some kind (the harness emits no event for
//! which). What happened is printed and written to `RESULTS.md` in `ARK_LIVE_OUT` (default:
//! a temporary folder, printed).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ark_orchestrate::*;
use ark_sdk::prelude::*;

#[derive(Clone, Default)]
struct Step {
    text: String,
    calls: Vec<String>,
}

struct Allow(Arc<Mutex<Vec<String>>>);

#[async_trait::async_trait]
impl Approver for Allow {
    async fn decide(&self, request: &ApprovalRequest) -> ApprovalDecision {
        self.0
            .lock()
            .unwrap()
            .push(format!("{} | {}", request.action, request.preview));
        ApprovalDecision::AllowOnce
    }
}

struct World {
    _dir: tempfile::TempDir,
    _ark: Ark,
    o: Orchestrator,
    folder: PathBuf,
    steps: Arc<Mutex<Vec<Step>>>,
    lagged: Arc<Mutex<u64>>,
    asked: Arc<Mutex<Vec<String>>>,
    watcher: tokio::task::JoinHandle<()>,
}

fn model() -> Arc<dyn ark_model::Provider> {
    let spec = std::env::var("ARK_LIVE_MODEL").unwrap_or_else(|_| "claude:haiku".into());
    let (p, m) = spec.split_once(':').unwrap_or((spec.as_str(), ""));
    ark_model::build(p, (!m.is_empty()).then(|| m.to_string())).expect("the model")
}

fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let ark = Ark::builder()
        .store_root(dir.path().join("store"))
        .provider(model())
        .files(true)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.find_or_create_project("p", "").unwrap();
    let folder = dir.path().join("project");
    std::fs::create_dir_all(&folder).unwrap();
    ark.engine().set_workspace(&project.id, &folder).unwrap();
    let o = Orchestrator::builder(&ark, &project)
        .approvals(Arc::new(ApprovalGate::new(Arc::new(Allow(asked.clone())))))
        .build()
        .unwrap();
    let steps = Arc::new(Mutex::new(Vec::<Step>::new()));
    let lagged = Arc::new(Mutex::new(0u64));
    let mut rx = o.subscribe();
    let (s, l) = (steps.clone(), lagged.clone());
    let watcher = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(e) => {
                    if let EventKind::Agent { event, .. } = e.kind {
                        let mut s = s.lock().unwrap();
                        match event {
                            HarnessEvent::Step { .. } => s.push(Step::default()),
                            HarnessEvent::TextDelta { text } => {
                                if let Some(last) = s.last_mut() {
                                    last.text.push_str(&text);
                                }
                            }
                            HarnessEvent::TextWithdrawn { text } => {
                                if let Some(last) = s.last_mut() {
                                    if let Some(i) = last.text.rfind(&text) {
                                        last.text.truncate(i);
                                    }
                                }
                            }
                            HarnessEvent::ToolCall { action, input, .. } => {
                                if let Some(last) = s.last_mut() {
                                    last.calls.push(format!("{action} {input}"));
                                }
                            }
                            _ => {}
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    *l.lock().unwrap() += n;
                }
                Err(_) => break,
            }
        }
    });
    World {
        _dir: dir,
        _ark: ark,
        o,
        folder,
        steps,
        lagged,
        asked,
        watcher,
    }
}

fn tool_names(steps: &[Step]) -> Vec<String> {
    steps
        .iter()
        .flat_map(|s| s.calls.iter())
        .map(|c| c.split(' ').next().unwrap_or("").to_string())
        .collect()
}

/// Steps that wrote text beside their calls, and steps with no call that another step follows.
fn shape(steps: &[Step]) -> (usize, usize) {
    let narrated = steps
        .iter()
        .filter(|s| !s.calls.is_empty() && !s.text.trim().is_empty())
        .count();
    let silent: Vec<usize> = steps
        .iter()
        .enumerate()
        .filter(|(_, s)| s.calls.is_empty())
        .map(|(i, _)| i)
        .collect();
    let nudged = silent.iter().filter(|i| **i + 1 < steps.len()).count();
    (narrated, nudged)
}

fn raw(steps: &[Step]) -> String {
    let mut out = String::new();
    for (i, s) in steps.iter().enumerate() {
        out.push_str(&format!("- step {}\n", i + 1));
        if !s.text.trim().is_empty() {
            out.push_str(&format!(
                "  text: {}\n",
                s.text.trim().replace('\n', "\n        ")
            ));
        }
        for c in &s.calls {
            out.push_str(&format!(
                "  call: {}\n",
                c.chars().take(300).collect::<String>()
            ));
        }
    }
    out
}

const ASK: &str = "Make a file called plan.md with three short steps to launch a weekly newsletter. Do it, don't just tell me.";
const BRIEF: &str = "Read alpha.md, beta.md and gamma.md in the project folder, one file at a time. Then write summary.md with one line per file: the file name, a colon and its main fact. Reply with one short sentence when summary.md is written.";

async fn wait(w: &World, id: &str) -> TaskRecord {
    let t = tokio::time::timeout(Duration::from_secs(600), w.o.wait(id))
        .await
        .expect("finished in 10 minutes")
        .unwrap();
    // Let the watcher drain what the task emitted last.
    tokio::time::sleep(Duration::from_millis(300)).await;
    w.watcher.abort();
    t
}

fn cost(t: &TaskRecord) -> String {
    format!(
        "prompt {} (cached {}), completion {}, model calls {}, cost ${:.4}, measured {}",
        t.usage.prompt_tokens,
        t.usage.cached_tokens,
        t.usage.completion_tokens,
        t.usage.model_calls,
        t.usage.cost_micro_usd as f64 / 1e6,
        t.usage.measured
    )
}

async fn case_a(report: &mut String) -> bool {
    let w = world();
    w.o.spawn(AgentSpec::new("writer").tools(["files.*"]))
        .await
        .unwrap();
    let started = Instant::now();
    let id =
        w.o.submit(
            "writer",
            TaskSpec {
                person: true,
                ..TaskSpec::new(ASK)
            },
        )
        .unwrap();
    let t = wait(&w, &id).await;
    let steps = w.steps.lock().unwrap().clone();
    let asked = w.asked.lock().unwrap().clone();
    let exists = w.folder.join("plan.md").exists();
    let answer = t.result.clone().unwrap_or_default();
    let lower = answer.to_lowercase();
    let read_only = lower.contains("read-only") || lower.contains("read only");
    let pass = !asked.is_empty() && exists;
    report.push_str(&format!(
        "## A. Authority line (fix 3)\n\n| Item | Value |\n|---|---|\n| Result | {} |\n| State | {:?} |\n| Approval requests | {} |\n| plan.md exists | {} |\n| Answer calls itself read-only | {} |\n| Usage | {} |\n| Seconds | {:.0} |\n\nTool calls: {:?}\n\nApproval requests recorded:\n\n{}\n\nAnswer:\n\n> {}\n\nError: {:?}\n\nSteps:\n\n{}\n",
        if pass { "PASS" } else { "FAIL" },
        t.state,
        asked.len(),
        exists,
        read_only,
        cost(&t),
        started.elapsed().as_secs_f32(),
        tool_names(&steps),
        asked
            .iter()
            .map(|a| format!("- {a}"))
            .collect::<Vec<_>>()
            .join("\n"),
        answer.replace('\n', "\n> "),
        t.error,
        raw(&steps),
    ));
    if exists {
        report.push_str(&format!(
            "plan.md:\n\n```\n{}\n```\n\n",
            std::fs::read_to_string(w.folder.join("plan.md")).unwrap_or_default()
        ));
    }
    pass
}

fn facts(folder: &Path) {
    std::fs::write(
        folder.join("alpha.md"),
        "# Alpha\nThe Alpha site opened in 2019 and has 40 staff.\n",
    )
    .unwrap();
    std::fs::write(
        folder.join("beta.md"),
        "# Beta\nThe Beta line ships on Tuesdays and costs 12 euros a unit.\n",
    )
    .unwrap();
    std::fs::write(
        folder.join("gamma.md"),
        "# Gamma\nThe Gamma team is led by Ines and closed 7 deals in March.\n",
    )
    .unwrap();
}

struct RunB {
    pass: bool,
    row: String,
    detail: String,
}

async fn case_b(n: usize) -> RunB {
    let w = world();
    facts(&w.folder);
    w.o.spawn(
        AgentSpec::new("worker")
            .tools(["files.*"])
            .allow_mutate(true),
    )
    .await
    .unwrap();
    let started = Instant::now();
    let id = w.o.submit("worker", TaskSpec::new(BRIEF)).unwrap();
    let t = wait(&w, &id).await;
    let steps = w.steps.lock().unwrap().clone();
    let tools = tool_names(&steps);
    let summary = w.folder.join("summary.md").exists();
    let (narrated, nudged) = shape(&steps);
    let pass = !(t.state == TaskState::Done && !summary);
    let row = format!(
        "| {n} | {} | {} | {} | {} | {:?} | {} | {} | {} | {:.0} |",
        tools.len(),
        steps.len(),
        narrated,
        nudged,
        t.state,
        summary,
        if pass { "PASS" } else { "FAIL" },
        cost(&t),
        started.elapsed().as_secs_f32(),
    );
    let detail = format!(
        "### Run {n}\n\nTool calls: {:?}\n\nLagged events: {}\n\nResult:\n\n> {}\n\nError: {:?}\n\nSteps:\n\n{}\n{}\n",
        tools,
        w.lagged.lock().unwrap(),
        t.result.clone().unwrap_or_default().replace('\n', "\n> "),
        t.error,
        raw(&steps),
        if summary {
            format!(
                "summary.md:\n\n```\n{}\n```\n",
                std::fs::read_to_string(w.folder.join("summary.md")).unwrap_or_default()
            )
        } else {
            "summary.md: missing\n".to_string()
        }
    );
    RunB { pass, row, detail }
}

#[tokio::test]
#[ignore = "live: calls a real model (ARK_LIVE_MODEL, default claude:haiku)"]
async fn adr_0122_on_a_real_model() {
    let out = std::env::var("ARK_LIVE_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| tempfile::tempdir().unwrap().keep());
    std::fs::create_dir_all(&out).unwrap();
    let mut report = format!(
        "# ADR 0122 on a real model\n\nModel: `{}`\n\n",
        std::env::var("ARK_LIVE_MODEL").unwrap_or_else(|_| "claude:haiku".into())
    );
    let a = case_a(&mut report).await;
    let mut rows = Vec::new();
    let mut details = Vec::new();
    let mut all = true;
    for n in 1..=3 {
        let r = case_b(n).await;
        all &= r.pass;
        rows.push(r.row);
        details.push(r.detail);
    }
    report.push_str(&format!(
        "## B. Step note nudge (fix 1)\n\nResult: {}\n\n\"Nudge\" is a step with no call that another step follows (any nudge, not only the announced-work one).\n\n| Run | Tool calls | Steps | Steps with text beside calls | Nudges | End state | summary.md | Result | Usage | Seconds |\n|---|---|---|---|---|---|---|---|---|---|\n{}\n\n{}",
        if all { "PASS" } else { "FAIL" },
        rows.join("\n"),
        details.join("\n")
    ));
    std::fs::write(out.join("RESULTS.md"), &report).unwrap();
    println!("{report}");
    eprintln!("written to {}", out.join("RESULTS.md").display());
    assert!(a, "case A failed, see RESULTS.md");
    assert!(all, "case B ended Done without summary.md, see RESULTS.md");
}
