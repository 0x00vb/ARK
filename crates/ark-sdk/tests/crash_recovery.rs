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

/// `slow.run` says it started, then takes a minute. `slow.write` changes state: it adds a line
/// to `writes.log` in the store folder (so runs are counted across processes) and, in the
/// child playing `cut`, then hangs, as a change the crash cuts short after it happened.
struct Slow(std::path::PathBuf);

fn writes(dir: &std::path::Path) -> usize {
    std::fs::read_to_string(dir.join("writes.log"))
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

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
        if action == "slow.write" {
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.0.join("writes.log"))
                .unwrap();
            writeln!(f, "written").unwrap();
            f.sync_all().unwrap();
            if std::env::var(CHILD).as_deref() == Ok("cut") {
                ready();
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        }
        Ok(ActionOutput::json(json!({ "done": true })))
    }
}

/// Says yes to everything, and keeps what it was asked.
#[derive(Default)]
struct Yes(std::sync::Mutex<Vec<ApprovalRequest>>);

#[async_trait::async_trait]
impl Approver for Yes {
    async fn decide(&self, r: &ApprovalRequest) -> ApprovalDecision {
        self.0.lock().unwrap().push(r.clone());
        ApprovalDecision::AllowOnce
    }
}

/// Says no, and keeps what it was asked.
#[derive(Default)]
struct No(std::sync::Mutex<Vec<ApprovalRequest>>);

#[async_trait::async_trait]
impl Approver for No {
    async fn decide(&self, r: &ApprovalRequest) -> ApprovalDecision {
        self.0.lock().unwrap().push(r.clone());
        ApprovalDecision::Deny
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
    agent_on(
        dir,
        Arc::new(MockProvider::new("m", script).with_chunk_delay(chunk)),
        false,
    )
}

fn agent_on(dir: &std::path::Path, model: Arc<MockProvider>, allow_mutate: bool) -> Ark {
    Ark::builder()
        .store_root(dir)
        .allow_mutate(allow_mutate)
        .tool(Arc::new(Slow(dir.to_path_buf())))
        .profile(
            AgentProfile::builder()
                .core_actions(["slow.run", "slow.write"])
                .build(),
        )
        .provider(model as Arc<dyn ark_model::Provider>)
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
    let yes = || Some(Arc::new(ApprovalGate::new(Arc::new(Yes::default()))));
    let (script, chunk, gate) = match role {
        "tool" => (vec![step("slow.run"), say("done")], Duration::ZERO, None),
        // A change succeeds, then a long read is under way.
        "change" => (
            vec![step("slow.write"), step("slow.run"), say("done")],
            Duration::ZERO,
            yes(),
        ),
        // A change happens and its call is still under way.
        "cut" => (vec![step("slow.write"), say("done")], Duration::ZERO, yes()),
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
    // Listed as it is, before anyone opens it: the store's opening counted the turn (a chat
    // whose only turn was cut short is not hidden as empty).
    let listed = Session::list(ark.engine(), Some(&p.id)).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].turn_count, 1);
    assert_eq!(listed[0].title, QUESTION);
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

/// The first request of the last turn `model` answered, as text.
fn first_request(model: &MockProvider, of_last: usize) -> String {
    let calls = model.recorded();
    serde_json::to_string(&calls[calls.len() - of_last].messages).unwrap()
}

/// ADR 0048: killed after a change succeeded and while a read was under way. The next process
/// shows the turn interrupted; "continue" is told what was done, and the model making the same
/// change again gets the earlier result: it is not made twice.
#[tokio::test]
async fn killed_after_a_change_continues_without_making_it_again() {
    if let Ok(role) = std::env::var(CHILD) {
        return child(&role).await;
    }
    let dir = crash(
        "killed_after_a_change_continues_without_making_it_again",
        "change",
    );
    assert_eq!(writes(dir.path()), 1);

    let model = Arc::new(MockProvider::new(
        "m",
        vec![
            step("slow.write"),
            say("Finished: the change was already made."),
        ],
    ));
    let ark = agent_on(dir.path(), model.clone(), false);
    let p = ark.find_or_create_project("p", "").unwrap();
    let mut session = ark.latest_session(&p.id).unwrap().unwrap();
    let turns = session.transcript(ark.engine(), None).unwrap();
    assert!(turns[0].incomplete);
    assert_eq!(turns[0].error.as_deref(), Some(INTERRUPTED));
    let left = session.unfinished(ark.engine()).unwrap().unwrap();
    let states: Vec<_> = left
        .calls
        .iter()
        .map(|c| (c.action.as_str(), c.status))
        .collect();
    assert_eq!(
        states,
        [
            ("slow.write", ark_core::InvocationStatus::Ok),
            ("slow.run", ark_core::InvocationStatus::Failed)
        ]
    );

    let gate = Arc::new(ApprovalGate::new(Arc::new(Yes::default())));
    let out = ark
        .ask_session(
            &mut session,
            "continue",
            TurnOptions {
                approvals: Some(gate),
                ..Default::default()
            },
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(out.answer, "Finished: the change was already made.");
    assert_eq!(writes(dir.path()), 1, "the change was not made twice");
    let asked = first_request(&model, 2);
    assert!(
        asked.contains("Your last turn stopped before it finished"),
        "{asked}"
    );
    assert!(asked.contains("done: slow.write"), "{asked}");
    assert!(asked.contains("cut short: slow.run"), "{asked}");
    // The continuation finished: the turn after it starts fresh.
    assert!(session.unfinished(ark.engine()).unwrap().is_none());
}

/// ADR 0048: killed while a change was under way (it happened, its call never returned).
/// Whether it happened is unknown, so the same change is asked of the person again, even with a
/// lasting grant for it; "no" leaves it not run.
#[tokio::test]
async fn a_change_cut_short_by_a_crash_is_asked_again_whatever_was_granted() {
    if let Ok(role) = std::env::var(CHILD) {
        return child(&role).await;
    }
    let dir = crash(
        "a_change_cut_short_by_a_crash_is_asked_again_whatever_was_granted",
        "cut",
    );
    assert_eq!(writes(dir.path()), 1);

    // The model tries twice: a "no" holds for the second try too.
    let model = Arc::new(MockProvider::new(
        "m",
        vec![
            step("slow.write"),
            step("slow.write"),
            say("It may need checking."),
        ],
    ));
    let ark = agent_on(dir.path(), model, false);
    let p = ark.find_or_create_project("p", "").unwrap();
    let mut session = ark.latest_session(&p.id).unwrap().unwrap();
    let left = session.unfinished(ark.engine()).unwrap().unwrap();
    assert_eq!(left.cut_short().len(), 1);

    let no = Arc::new(No::default());
    let gate = Arc::new(ApprovalGate::new(no.clone()));
    gate.keep_in(ark.engine().store.clone(), p.id.as_str());
    let grant = ApprovalRequest {
        action: "slow.write".into(),
        summary: String::new(),
        risk: RiskTier::Mutate,
        input: json!({}),
        class: "slow.write".into(),
        preview: "slow.write()".into(),
        agent: None,
        task: None,
        ask_each_time: false,
        cut_short: false,
    };
    gate.remember(&grant, ApprovalDecision::AllowAlways);
    ark.ask_session(
        &mut session,
        "continue",
        TurnOptions {
            approvals: Some(gate),
            ..Default::default()
        },
        |_| {},
    )
    .await
    .unwrap();
    let asked = no.0.lock().unwrap().clone();
    assert_eq!(asked.len(), 2, "asked each time, though always allowed");
    assert!(asked.iter().all(|a| a.cut_short && a.ask_each_time));
    assert_eq!(writes(dir.path()), 1, "not run again");
}

/// ADR 0048: with nobody to ask (changes allowed without asking), a change cut short is never
/// run again on its own, however often the model tries.
#[tokio::test]
async fn a_change_cut_short_is_not_run_again_without_anyone_to_ask() {
    if let Ok(role) = std::env::var(CHILD) {
        return child(&role).await;
    }
    let dir = crash(
        "a_change_cut_short_is_not_run_again_without_anyone_to_ask",
        "cut",
    );
    let model = Arc::new(MockProvider::new(
        "m",
        vec![
            step("slow.write"),
            step("slow.write"),
            say("It was cut short."),
        ],
    ));
    let ark = agent_on(dir.path(), model.clone(), true);
    let p = ark.find_or_create_project("p", "").unwrap();
    let mut session = ark.latest_session(&p.id).unwrap().unwrap();
    ark.ask_session(&mut session, "continue", TurnOptions::default(), |_| {})
        .await
        .unwrap();
    assert_eq!(writes(dir.path()), 1);
    let last = first_request(&model, 1);
    assert!(last.contains("only the person can tell"), "{last}");
}
