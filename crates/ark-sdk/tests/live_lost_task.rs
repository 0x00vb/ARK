//! The owner's lost task on a real model (ADR 0050). Ignored: it runs a subscription model
//! (Codex or Claude Code) for several minutes.
//!
//! ```text
//! ARK_LIVE_MODEL=codex:default cargo test -p ark-sdk --test live_lost_task -- --ignored --nocapture
//! ARK_LIVE_MODEL=claude:sonnet cargo test -p ark-sdk --test live_lost_task -- --ignored --nocapture
//! ```
//!
//! A copy of ARK's own window code is the project, as when the owner asked. A child process
//! sends the owner's message, word for word, with Files and project processes on (as ARK
//! Desktop has them), and is killed (SIGKILL) after it has made several calls. A new process
//! opens the store and sends "cointinue". The run passes when that turn keeps the request as
//! its goal, changes the editor's file for highlighting, and finishes. What happened is written
//! to `ARK_LIVE_OUT` (default: a temporary folder, printed) for a person to read.

use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ark_sdk::prelude::*;

const CHILD: &str = "ARK_LIVE_CHILD";
const DIR: &str = "ARK_LIVE_DIR";
const QUESTION: &str = "On the editor file, we shuold support sintax highlighting (according to the ark user theme).\nAnalize it, and make a smal plan and impleemnt";
/// Calls the first turn makes before the process is killed.
const KILL_AFTER: usize = 6;

fn model() -> Arc<dyn ark_model::Provider> {
    let spec = std::env::var("ARK_LIVE_MODEL").unwrap_or_else(|_| "codex:default".into());
    let (p, m) = spec.split_once(':').unwrap_or((spec.as_str(), ""));
    ark_model::build(p, (!m.is_empty()).then(|| m.to_string())).expect("the model")
}

fn ark(dir: &std::path::Path) -> Ark {
    Ark::builder()
        .store_root(dir.join("store"))
        .allow_mutate(true)
        .files(true)
        .processes(true)
        .provider(model())
        .build()
        .unwrap()
}

/// The project: ARK's window code (`crates/ark-ui/web`), as the owner's folder had it.
fn project(ark: &Ark, dir: &std::path::Path) -> ark_core::ProjectId {
    let folder = dir.join("ark-ui");
    if !folder.exists() {
        std::fs::create_dir_all(&folder).unwrap();
        let web = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../ark-ui/web");
        let copied = Command::new("cp")
            .args(["-R", web.to_str().unwrap(), folder.to_str().unwrap()])
            .status()
            .unwrap();
        assert!(copied.success());
    }
    let p = ark.find_or_create_project("ark-ui", "").unwrap();
    ark.engine().set_workspace(&p.id, &folder).unwrap();
    p.id
}

/// One line per event a person would want to read afterwards.
fn line(e: &HarnessEvent) -> Option<String> {
    Some(match e {
        HarnessEvent::ToolCall { action, input, .. } => {
            let s = input.to_string();
            format!(
                "call   {action} {}",
                s.chars().take(160).collect::<String>()
            )
        }
        HarnessEvent::ToolResult {
            action,
            ok,
            summary,
            ..
        } => format!(
            "result {action} ok={ok} {}",
            summary
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(160)
                .collect::<String>()
        ),
        HarnessEvent::Retry { error, repair, .. } => format!("retry  repair={repair} {error}"),
        HarnessEvent::Context { report } => format!(
            "ctx    step {} total {} budget {} task {} ledger {} dropped {}",
            report.step,
            report.total,
            report.budget,
            report.task,
            report.ledger,
            report.compressed_rounds
        ),
        HarnessEvent::ModelCall { usage, .. } => format!(
            "model  prompt {} cached {} out {}",
            usage.prompt_tokens, usage.cached_tokens, usage.completion_tokens
        ),
        HarnessEvent::Answer { text, .. } => format!("answer {text}"),
        _ => return None,
    })
}

async fn child() {
    let dir = std::path::PathBuf::from(std::env::var(DIR).unwrap());
    let ark = ark(&dir);
    let p = project(&ark, &dir);
    let mut session = ark.start_session(&p).unwrap();
    let log = dir.join("turn1.log");
    let mut calls = 0usize;
    let _ = ark
        .ask_session(&mut session, QUESTION, TurnOptions::default(), |e| {
            if let Some(l) = line(&e) {
                use std::io::Write as _;
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&log)
                    .unwrap();
                let _ = writeln!(f, "{l}");
            }
            if matches!(e, HarnessEvent::ToolResult { .. }) {
                calls += 1;
                if calls >= KILL_AFTER {
                    std::fs::write(dir.join("ready"), b"").unwrap();
                }
            }
        })
        .await;
    std::fs::write(dir.join("finished"), b"").unwrap();
}

#[tokio::test]
#[ignore = "runs a subscription model for minutes"]
async fn the_owners_task_survives_a_crash_and_continues_on_a_real_model() {
    if std::env::var(CHILD).is_ok() {
        return child().await;
    }
    let out = std::env::var("ARK_LIVE_OUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| tempfile::tempdir().unwrap().keep());
    std::fs::create_dir_all(&out).unwrap();
    eprintln!("live run in {}", out.display());

    // The first turn, killed after it has made several calls.
    let mut c = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "the_owners_task_survives_a_crash_and_continues_on_a_real_model",
            "--ignored",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env(DIR, &out)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let started = Instant::now();
    while !out.join("ready").exists() {
        assert!(
            !out.join("finished").exists(),
            "the first turn ended before {KILL_AFTER} calls"
        );
        assert!(started.elapsed() < Duration::from_secs(900), "too slow");
        std::thread::sleep(Duration::from_millis(200));
    }
    c.kill().unwrap();
    c.wait().unwrap();
    eprintln!(
        "killed after {KILL_AFTER} calls, {:.0}s",
        started.elapsed().as_secs_f64()
    );

    // A new process: "cointinue".
    let ark = ark(&out);
    let p = project(&ark, &out);
    let folder = ark.engine().workspace_dir(&p).unwrap();
    let before = std::fs::read_to_string(folder.join("web/js/file.js")).unwrap_or_default();
    let mut session = ark.latest_session(&p).unwrap().unwrap();
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let l = lines.clone();
    let task_always = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let every = task_always.clone();
    let t = Instant::now();
    let result = ark
        .ask_session(
            &mut session,
            "cointinue",
            TurnOptions::default(),
            move |e| {
                if let HarnessEvent::Context { report } = &e {
                    if report.task == 0 {
                        every.store(false, std::sync::atomic::Ordering::SeqCst);
                    }
                }
                if let Some(s) = line(&e) {
                    eprintln!("{s}");
                    l.lock().unwrap().push(s);
                }
            },
        )
        .await;
    std::fs::write(out.join("turn2.log"), lines.lock().unwrap().join("\n")).unwrap();
    let outcome = result.expect("the continued turn finishes");
    eprintln!(
        "continued in {:.0}s, {} steps, prompt tokens {}",
        t.elapsed().as_secs_f64(),
        outcome.steps,
        outcome.metrics.prompt_tokens
    );
    let state = ark.engine().store.get_research_state(&p).unwrap().unwrap();
    assert_eq!(
        state.goal, QUESTION,
        "the goal is the request, not \"cointinue\""
    );
    let turns = session.transcript(ark.engine(), None).unwrap();
    let goal = turns
        .last()
        .unwrap()
        .goal
        .clone()
        .expect("the turn kept its goal");
    assert_eq!(goal.request, QUESTION);
    let after = std::fs::read_to_string(folder.join("web/js/file.js")).unwrap_or_default();
    eprintln!("answer: {}", outcome.answer);
    eprintln!("file.js changed: {}", before != after);
    std::fs::write(out.join("answer.txt"), &outcome.answer).unwrap();
    assert!(
        outcome.answer.to_lowercase().contains("highlight"),
        "the answer is about the task: {}",
        outcome.answer
    );
    assert!(
        task_always.load(std::sync::atomic::Ordering::SeqCst),
        "every request carried the task"
    );
}
