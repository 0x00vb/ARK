//! Kill a process mid-turn (SIGKILL: no cleanup runs) and reopen its store. Whatever was in
//! flight must be marked as not finished, never shown as done, and the next turn must work.
//!
//! The test binary runs itself as the child: with `ARK_CRASH_CHILD` set, a test plays the
//! child's part and blocks until it is killed. The child says it reached the point to kill by
//! creating a file (a piped stdout may be block-buffered).

use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use ark_sdk::prelude::ark_model::{Completion, MockProvider, ToolCall, Usage};
use ark_sdk::prelude::ark_runtime::session::INTERRUPTED;
use ark_sdk::prelude::*;
use serde_json::json;

const CHILD: &str = "ARK_CRASH_CHILD";
const DIR: &str = "ARK_CRASH_DIR";

/// Tell the parent the child reached the point to kill.
fn ready() {
    let dir = std::path::PathBuf::from(std::env::var(DIR).unwrap());
    std::fs::write(dir.join("ready"), b"").unwrap();
}

/// `slow.run` says it started, then takes a minute. `slow.write` changes state.
struct Slow;

#[async_trait::async_trait]
impl Adapter for Slow {
    fn name(&self) -> &str {
        "slow"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new("slow.run", "a long read", RiskTier::Read),
            ActionDescriptor::new("slow.write", "a change", RiskTier::Mutate),
        ]
    }
    async fn run(
        &self,
        action: &str,
        _i: serde_json::Value,
        _c: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        if action == "slow.run" {
            ready();
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
        Ok(ActionOutput::json(json!({ "done": true })))
    }
}

/// Never answers, after saying it was asked.
struct Waits;

#[async_trait::async_trait]
impl Approver for Waits {
    async fn decide(&self, _r: &ApprovalRequest) -> ApprovalDecision {
        ready();
        tokio::time::sleep(Duration::from_secs(60)).await;
        ApprovalDecision::Deny
    }
}

fn agent(dir: &std::path::Path, script: Vec<Completion>, chunk: Duration) -> Ark {
    let model: Arc<dyn ark_model::Provider> =
        Arc::new(MockProvider::new("m", script).with_chunk_delay(chunk));
    Ark::builder()
        .store_root(dir)
        .tool(Arc::new(Slow))
        .profile(
            AgentProfile::builder()
                .core_actions(["slow.run", "slow.write"])
                .build(),
        )
        .provider(model)
        .planner(false)
        .critic(false)
        .build()
        .unwrap()
}

fn step(name: &str) -> Completion {
    Completion {
        content: String::new(),
        tool_calls: vec![ToolCall {
            id: "c1".into(),
            name: name.into(),
            arguments: json!({}),
        }],
        usage: Usage::default(),
    }
}

fn say(text: &str) -> Completion {
    Completion {
        content: text.into(),
        tool_calls: vec![],
        usage: Usage::default(),
    }
}

const QUESTION: &str = "please do the long thing now";

/// The child's part: start the turn and block in it until killed.
async fn child(role: &str) {
    let dir = std::path::PathBuf::from(std::env::var(DIR).unwrap());
    let (script, chunk, gate) = match role {
        "tool" => (vec![step("slow.run"), say("done")], Duration::ZERO, None),
        "stream" => (
            vec![say(&"a long streamed answer, word by word. ".repeat(40))],
            Duration::from_millis(100),
            None,
        ),
        _ => (
            vec![step("slow.write"), say("done")],
            Duration::ZERO,
            Some(Arc::new(ApprovalGate::new(Arc::new(Waits)))),
        ),
    };
    let ark = agent(&dir, script, chunk);
    let p = ark.find_or_create_project("p", "").unwrap();
    let mut session = ark.start_session(&p.id).unwrap();
    let mut said = false;
    let _ = ark
        .ask_session(
            &mut session,
            QUESTION,
            TurnOptions {
                approvals: gate,
                ..Default::default()
            },
            |e| {
                if matches!(e, HarnessEvent::TextDelta { .. }) && !said {
                    ready();
                    said = true;
                }
            },
        )
        .await;
    let dir = std::path::PathBuf::from(std::env::var(DIR).unwrap());
    std::fs::write(dir.join("finished"), b"").unwrap();
}

/// Run `test` as a child in `role`, kill it once it says READY, and return the store root.
fn crash(test: &str, role: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let mut c = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture", "--test-threads", "1"])
        .env(CHILD, role)
        .env(DIR, dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let started = std::time::Instant::now();
    while !dir.path().join("ready").exists() {
        assert!(
            !dir.path().join("finished").exists(),
            "the child finished before the kill"
        );
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the child never got ready"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    c.kill().unwrap(); // SIGKILL
    c.wait().unwrap();
    dir
}

/// Reopen the store as a new process would, check the interrupted turn, then run a turn.
async fn reopen_and_check(dir: &std::path::Path) -> Ark {
    let ark = agent(
        dir,
        vec![say("Back. The last request did not finish.")],
        Duration::ZERO,
    );
    let p = ark.find_or_create_project("p", "").unwrap();
    for inv in ark.engine().store.list_invocations(&p.id).unwrap() {
        assert_ne!(
            inv.status,
            ark_core::InvocationStatus::Running,
            "nothing is left running"
        );
        assert_ne!(
            inv.status,
            ark_core::InvocationStatus::Ok,
            "nothing reads as done"
        );
    }
    let mut session = ark.latest_session(&p.id).unwrap().unwrap();
    let turns = session.transcript(ark.engine(), None).unwrap();
    assert_eq!(turns.len(), 1, "the interrupted turn is kept");
    assert_eq!(turns[0].input, QUESTION);
    assert!(turns[0].incomplete);
    assert_eq!(turns[0].error.as_deref(), Some(INTERRUPTED));
    assert!(
        turns[0].answer.is_empty(),
        "no partial answer is shown as an answer"
    );
    let digest = session.conversation_digest().unwrap();
    assert!(digest.contains("did not finish"), "{digest}");

    let out = ark
        .ask_session(
            &mut session,
            "what happened?",
            TurnOptions::default(),
            |_| {},
        )
        .await
        .unwrap();
    assert!(out.answer.starts_with("Back."));
    assert_eq!(session.turn_count(), 2);
    ark
}

#[tokio::test]
async fn killed_during_a_tool_call() {
    if let Ok(role) = std::env::var(CHILD) {
        return child(&role).await;
    }
    let dir = crash("killed_during_a_tool_call", "tool");
    let ark = reopen_and_check(dir.path()).await;
    let p = ark.find_or_create_project("p", "").unwrap();
    let invs = ark.engine().store.list_invocations(&p.id).unwrap();
    assert_eq!(invs.len(), 1, "the call was recorded before it ran");
    assert_eq!(invs[0].status, ark_core::InvocationStatus::Failed);
    assert!(invs[0].error.as_deref().unwrap().starts_with("interrupted"));
}

#[tokio::test]
async fn killed_while_the_answer_streams() {
    if let Ok(role) = std::env::var(CHILD) {
        return child(&role).await;
    }
    let dir = crash("killed_while_the_answer_streams", "stream");
    reopen_and_check(dir.path()).await;
}

#[tokio::test]
async fn killed_while_waiting_for_approval() {
    if let Ok(role) = std::env::var(CHILD) {
        return child(&role).await;
    }
    let dir = crash("killed_while_waiting_for_approval", "approval");
    let ark = reopen_and_check(dir.path()).await;
    let p = ark.find_or_create_project("p", "").unwrap();
    assert!(
        ark.engine()
            .store
            .list_invocations(&p.id)
            .unwrap()
            .is_empty(),
        "the change never ran and left nothing behind"
    );
}
