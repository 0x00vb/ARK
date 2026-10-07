//! ADR 0048: a turn the process never finished (SIGKILL: no cleanup runs) still shows what it
//! did. Its trace is saved while it runs; the next process shows each call as its record says,
//! the turn as interrupted, and offers to continue it. The window and the terminal read the same
//! `TranscriptTurn`.
//!
//! The test binary runs itself as the child, as `ark-sdk/tests/crash_recovery.rs` does.

use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use ark_interact::{ActivityKind, App, ChannelApprover, Interaction, Start, TurnControl};
use ark_sdk::prelude::ark_model::{Completion, MockProvider, ToolCall, Usage};
use ark_sdk::prelude::*;

const CHILD: &str = "ARK_INTERACT_CRASH_CHILD";
const DIR: &str = "ARK_INTERACT_CRASH_DIR";

/// `notes.read` answers; `notes.wait` says it started, then takes a minute.
struct Notes;
#[async_trait::async_trait]
impl Adapter for Notes {
    fn name(&self) -> &str {
        "notes"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new("notes.read", "read the notes", RiskTier::Read),
            ActionDescriptor::new("notes.wait", "a long read", RiskTier::Read),
        ]
    }
    async fn run(
        &self,
        action: &str,
        _i: serde_json::Value,
        _c: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        if action == "notes.wait" {
            let dir = std::path::PathBuf::from(std::env::var(DIR).unwrap());
            std::fs::write(dir.join("ready"), b"").unwrap();
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
        Ok(ActionOutput::json(
            serde_json::json!({ "text": "remember the locket" }),
        ))
    }
}

fn step(name: &str) -> Completion {
    Completion {
        content: format!("Now {name}."),
        tool_calls: vec![ToolCall {
            id: format!("c-{name}"),
            name: name.into(),
            arguments: serde_json::json!({}),
        }],
        usage: Usage::default(),
    }
}

fn open(dir: &std::path::Path, script: Vec<Completion>) -> Interaction {
    let ark = Ark::builder()
        .store_root(dir)
        .tool(Arc::new(Notes))
        .profile(
            AgentProfile::builder()
                .core_actions(["notes.read", "notes.wait"])
                .build(),
        )
        .provider(Arc::new(MockProvider::new("m", script)) as Arc<dyn ark_model::Provider>)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let (approver, _rx) = ChannelApprover::channel();
    App::new("Test")
        .ark(ark)
        .labels(
            ark_interact::ActivityLabels::new()
                .label("notes.read", |_| "Reading the notes".into())
                .label("notes.wait", |_| "Waiting on the notes".into()),
        )
        .open(&Start::default(), Arc::new(approver))
        .unwrap()
        .interaction
}

#[tokio::test]
async fn a_killed_turn_shows_what_it_did_and_where_it_stopped() {
    if std::env::var(CHILD).is_ok() {
        let dir = std::path::PathBuf::from(std::env::var(DIR).unwrap());
        let mut ix = open(&dir, vec![step("notes.read"), step("notes.wait")]);
        ix.run_turn("read the notes, then wait", &TurnControl::new(), |_, _| {})
            .await;
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mut c = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "a_killed_turn_shows_what_it_did_and_where_it_stopped",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env(DIR, dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let started = std::time::Instant::now();
    while !dir.path().join("ready").exists() {
        assert!(started.elapsed() < Duration::from_secs(30), "never ready");
        std::thread::sleep(Duration::from_millis(20));
    }
    // Killed at once: the trace may not have the last call's start yet (it is saved at most
    // once a second). Its record has it.
    c.kill().unwrap(); // SIGKILL
    c.wait().unwrap();

    // The next process: the chat that was killed, reopened.
    let mut ix = open(dir.path(), vec![]);
    let mut turns = Vec::new();
    for s in ix.sessions(true).unwrap() {
        assert!(ix.resume(&s.id).unwrap());
        turns = ix.transcript(None, 10).unwrap();
        if !turns.is_empty() {
            break;
        }
    }
    assert_eq!(turns.len(), 1);
    let t = &turns[0];
    assert!(t.has_trace, "the trace was saved while the turn ran");
    assert!(t.interrupted && t.resume && !t.cancelled);
    assert_eq!(
        t.unfinished.as_deref(),
        Some("Interrupted before it finished.")
    );
    let rows: Vec<(ActivityKind, &str)> = t
        .activity
        .iter()
        .map(|a| (a.kind, a.label.as_str()))
        .collect();
    assert!(
        rows.contains(&(ActivityKind::Narration, "Now notes.read.")),
        "{rows:?}"
    );
    assert!(
        rows.contains(&(ActivityKind::Tool, "Reading the notes")),
        "{rows:?}"
    );
    assert!(
        rows.contains(&(ActivityKind::Tool, "Waiting on the notes")),
        "{rows:?}"
    );
    let result_of = |label: &str| {
        let i = t.activity.iter().position(|a| a.label == label).unwrap();
        t.activity[i + 1..]
            .iter()
            .find(|a| a.kind == ActivityKind::Result && a.call == t.activity[i].call)
            .map(|a| a.label.clone())
    };
    assert_ne!(result_of("Reading the notes").as_deref(), Some("cut short"));
    assert_eq!(
        result_of("Waiting on the notes").as_deref(),
        Some("cut short")
    );

    // The terminal says the same.
    let recap = serde_json::to_string(&ix.recap()).unwrap();
    assert!(recap.contains("Interrupted before it finished."), "{recap}");
}
