//! A task on an agent program (ADR 0043 step 2), against a scripted program that speaks the
//! protocol: this test binary, run again as the program (`fake_program`). It asks before it
//! writes, the question is kept on the task, the answer comes from another client, its writes
//! come back as outputs from the folder, and a restart never asks the person twice.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ark_orchestrate::*;
use ark_sdk::prelude::*;
use serde_json::{json, Value};

/// The program, when this binary runs as one (`ARK_FAKE_PROGRAM` set).
#[test]
fn fake_program() {
    let Ok(dir) = std::env::var("ARK_FAKE_PROGRAM") else {
        return;
    };
    serve(Path::new(&dir));
    std::process::exit(0);
}

fn serve(log: &Path) {
    let out = std::io::stdout();
    // The test runner wrote "test fake_program ... " without a line end.
    println!();
    let send = |v: Value| {
        let mut o = out.lock();
        writeln!(o, "{v}").unwrap();
        o.flush().unwrap();
    };
    let note = |what: &str, v: &Value| {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log.join("calls.log"))
            .unwrap();
        writeln!(f, "{what} {v}").unwrap();
    };
    note(
        "env",
        &json!({
            "secret": std::env::var("ANTHROPIC_API_KEY").ok(),
            "home": std::env::var("HOME").is_ok(),
        }),
    );
    let mode = std::env::var("FAKE_MODE").unwrap_or_else(|_| "default".into());
    let mut lines = std::io::stdin().lock().lines();
    let mut loaded = false;
    let mut cwd = PathBuf::new();
    while let Some(Ok(line)) = lines.next() {
        let msg: Value = serde_json::from_str(&line).unwrap();
        let id = msg["id"].clone();
        let method = msg["method"].as_str().unwrap_or("").to_string();
        note(&method, &msg["params"]);
        match method.as_str() {
            "initialize" => send(json!({ "jsonrpc": "2.0", "id": id, "result": {
                "protocolVersion": 1, "agentCapabilities": { "loadSession": true },
                "agentInfo": { "name": "fake", "version": "1" } } })),
            "session/new" | "session/load" => {
                cwd = PathBuf::from(msg["params"]["cwd"].as_str().unwrap());
                loaded = method == "session/load";
                if loaded {
                    // Its history replays as updates: not this run's work.
                    send(
                        json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": "s1",
                        "update": { "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "OLD " } } } }),
                    );
                }
                send(
                    json!({ "jsonrpc": "2.0", "id": id, "result": { "sessionId": "s1",
                    "modes": { "currentModeId": mode, "availableModes": [] } } }),
                );
            }
            "session/set_mode" => send(json!({ "jsonrpc": "2.0", "id": id, "result": {} })),
            "session/prompt" => {
                let call = if std::env::var("FAKE_KIND").as_deref() == Ok("execute") {
                    json!({ "toolCallId": "c1", "kind": "execute", "title": "printf",
                        "rawInput": { "command": "printf done > notes.txt" } })
                } else {
                    json!({ "toolCallId": "c1", "kind": "edit", "title": "Write report.md",
                        "locations": [{ "path": cwd.join("report.md") }] })
                };
                let mut update = call.clone();
                update["sessionUpdate"] = json!("tool_call");
                send(
                    json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": "s1", "update": update } }),
                );
                send(
                    json!({ "jsonrpc": "2.0", "id": 900, "method": "session/request_permission", "params": {
                    "sessionId": "s1", "toolCall": call, "options": [
                        { "kind": "allow_always", "name": "Always", "optionId": "always" },
                        { "kind": "allow_once", "name": "Allow", "optionId": "allow" },
                        { "kind": "reject_once", "name": "Reject", "optionId": "reject" } ] } }),
                );
                let answer: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
                note("answer", &answer["result"]);
                let allowed = answer["result"]["outcome"]["optionId"] == "allow";
                if allowed {
                    if std::env::var("FAKE_KIND").as_deref() == Ok("execute") {
                        std::fs::write(cwd.join("notes.txt"), "done").unwrap();
                    } else {
                        std::fs::write(cwd.join("report.md"), "# Report\n\nfeb: 32\n").unwrap();
                    }
                    send(
                        json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": "s1",
                        "update": { "sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "completed" } } }),
                    );
                    // Stops here once: the process goes away after the change, before the end.
                    if std::env::var("FAKE_HANG").is_ok() && !loaded {
                        std::thread::sleep(Duration::from_secs(600));
                    }
                }
                let text = if allowed {
                    "Wrote the report."
                } else {
                    "I was not allowed to write."
                };
                send(
                    json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": "s1",
                    "update": { "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": text } } } }),
                );
                send(
                    json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": "s1",
                    "update": { "sessionUpdate": "usage_update", "used": 10, "size": 100, "cost": { "amount": 0.05, "currency": "USD" } } } }),
                );
                send(
                    json!({ "jsonrpc": "2.0", "id": id, "result": { "stopReason": "end_turn",
                    "usage": { "inputTokens": 100, "outputTokens": 20, "cachedReadTokens": 50, "cachedWriteTokens": 0 } } }),
                );
            }
            _ => {
                if !id.is_null() {
                    send(json!({ "jsonrpc": "2.0", "id": id, "result": {} }));
                }
            }
        }
    }
}

/// The scripted program as a catalog entry.
fn catalog() -> Arc<dyn ark_runtime::worker::Workers> {
    std::env::set_var(
        "ARK_TEST_FAKE_PROGRAM_PATH",
        std::env::current_exe().unwrap(),
    );
    Arc::new(ark_acp::Catalog::of(vec![ark_acp::Entry {
        name: "fake",
        title: "Fake",
        program: "fake-program",
        path_var: "ARK_TEST_FAKE_PROGRAM_PATH",
        args: &["fake_program", "--exact", "--nocapture", "--test-threads=1"],
        model: ark_acp::ModelSetting::ConfigArg,
        default_model: None,
        session_meta: || json!({ "claudeCode": { "options": { "settingSources": [] } } }),
        mode: Some("default"),
        env: &["ARK_FAKE_PROGRAM", "FAKE_MODE", "FAKE_KIND", "FAKE_HANG"],
        setup: "",
    }]))
}

/// Never answers: the question waits for another client.
struct Nobody(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Approver for Nobody {
    async fn decide(&self, _: &ApprovalRequest) -> ApprovalDecision {
        self.0.fetch_add(1, Ordering::SeqCst);
        std::future::pending().await
    }
}

/// The tests set the program's environment: one at a time.
static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Setup {
    _one: std::sync::MutexGuard<'static, ()>,
    _dir: tempfile::TempDir,
    store: PathBuf,
    log: PathBuf,
}

fn setup() -> Setup {
    let one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    for k in ["FAKE_MODE", "FAKE_KIND", "FAKE_HANG"] {
        std::env::remove_var(k);
    }
    let dir = tempfile::tempdir().unwrap();
    let (store, log) = (dir.path().join("store"), dir.path().join("log"));
    std::fs::create_dir_all(&log).unwrap();
    std::env::set_var("ARK_FAKE_PROGRAM", &log);
    std::env::set_var("ANTHROPIC_API_KEY", "must-not-reach-the-program");
    Setup {
        _one: one,
        _dir: dir,
        store,
        log,
    }
}

fn open(s: &Setup, asked: Arc<AtomicUsize>) -> (Ark, Orchestrator) {
    let ark = Ark::builder()
        .store_root(&s.store)
        .provider(Arc::new(ark_model::MockProvider::new("unused", vec![])))
        .planner(false)
        .critic(false)
        .workers_with(catalog())
        .build()
        .unwrap();
    let project = ark.find_or_create_project("p", "").unwrap();
    let o = Orchestrator::builder(&ark, &project)
        .approvals(Arc::new(ApprovalGate::new(Arc::new(Nobody(asked)))))
        .build()
        .unwrap();
    (ark, o)
}

async fn until_asked(o: &Orchestrator, id: &str) -> TaskRecord {
    for _ in 0..200 {
        if let Some(t) = o.task(id).filter(|t| t.question.is_some()) {
            return t;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the program never asked: {:?}", o.task(id));
}

fn calls(s: &Setup) -> String {
    std::fs::read_to_string(s.log.join("calls.log")).unwrap_or_default()
}

fn folder(ark: &Ark) -> PathBuf {
    let p = ark.find_or_create_project("p", "").unwrap();
    ark.engine().workspace_dir(&p.id).unwrap()
}

/// Task, program, permission, kept question, answer from another client, the program goes
/// on, its file an output with usage and cost recorded. The program never sees ARK's keys and
/// runs without the person's own settings, in the mode that asks.
#[tokio::test(flavor = "multi_thread")]
async fn a_program_asks_ark_keeps_the_question_and_another_client_answers() {
    let s = setup();
    std::env::set_var("FAKE_MODE", "bypassPermissions");
    let asked = Arc::new(AtomicUsize::new(0));
    let (ark, o) = open(&s, asked.clone());
    o.spawn(AgentSpec::new("w").model("agent:fake"))
        .await
        .unwrap();
    let id = o.submit("w", TaskSpec::new("write the report")).unwrap();

    let waiting = until_asked(&o, &id).await;
    let q = waiting.question.unwrap();
    assert_eq!(q.request.action, "agent.change");
    assert_eq!(q.request.preview, "Change report.md");
    assert_eq!(q.request.class, "agent.change:report.md");
    assert_eq!(waiting.state, TaskState::Waiting);
    assert!(
        !folder(&ark).join("report.md").exists(),
        "nothing before the answer"
    );

    o.answer(&id, ApprovalDecision::AllowOnce).unwrap();
    let done = o.wait(&id).await.unwrap();
    assert_eq!(done.state, TaskState::Done, "{:?}", done.error);
    assert_eq!(done.result.as_deref(), Some("Wrote the report."));
    assert!(
        done.outputs.iter().any(|a| a.name == "report.md"),
        "{:?}",
        done.outputs
    );
    assert_eq!(asked.load(Ordering::SeqCst), 1);
    assert_eq!(done.usage.asked, 1);
    assert_eq!(done.usage.prompt_tokens, 150);
    assert_eq!(done.usage.cost_micro_usd, 50_000);

    let log = calls(&s);
    assert!(log.contains(r#"env {"home":true,"secret":null}"#), "{log}");
    assert!(log.contains(r#""settingSources":[]"#), "{log}");
    assert!(
        log.contains(r#"session/set_mode {"modeId":"default""#),
        "{log}"
    );
    assert!(
        log.contains(r#"answer {"outcome":{"optionId":"allow""#),
        "never 'always'"
    );
}

/// A command with no path in its request (a shell's write): its file is still an output, from
/// the folder. "Always" for one command does not cover another.
#[tokio::test(flavor = "multi_thread")]
async fn a_shell_write_is_found_in_the_folder() {
    let s = setup();
    std::env::set_var("FAKE_KIND", "execute");
    let asked = Arc::new(AtomicUsize::new(0));
    let (_ark, o) = open(&s, asked.clone());
    o.spawn(AgentSpec::new("w").model("agent:fake"))
        .await
        .unwrap();
    let id = o.submit("w", TaskSpec::new("take notes")).unwrap();
    let q = until_asked(&o, &id).await.question.unwrap();
    assert_eq!(q.request.preview, "Run `printf done > notes.txt`");
    assert_eq!(q.request.class, "agent.run:printf done > notes.txt@.");
    o.answer(&id, ApprovalDecision::AllowOnce).unwrap();
    let done = o.wait(&id).await.unwrap();
    std::env::remove_var("FAKE_KIND");
    assert_eq!(done.state, TaskState::Done, "{:?}", done.error);
    assert!(
        done.outputs.iter().any(|a| a.name == "notes.txt"),
        "{:?}",
        done.outputs
    );
}

/// The process stops after the person answered and the program wrote, before it finished.
/// The next process loads the program's own session; the program asks for the same change
/// again and gets the earlier answer: the person is asked once in all, the outputs hold the
/// file once.
#[test]
fn a_restart_after_the_answer_does_not_ask_twice() {
    let s = setup();
    std::env::set_var("FAKE_HANG", "1");
    let asked = Arc::new(AtomicUsize::new(0));
    let id = {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let id = rt.block_on(async {
            let (ark, o) = open(&s, asked.clone());
            o.spawn(AgentSpec::new("w").model("agent:fake"))
                .await
                .unwrap();
            let id = o.submit("w", TaskSpec::new("write the report")).unwrap();
            until_asked(&o, &id).await;
            o.answer(&id, ApprovalDecision::AllowOnce).unwrap();
            for _ in 0..200 {
                if folder(&ark).join("report.md").exists() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            assert!(folder(&ark).join("report.md").exists());
            id
        });
        // The process goes away: its runtime, the program with it.
        rt.shutdown_timeout(Duration::from_secs(2));
        id
    };
    std::env::remove_var("FAKE_HANG");
    assert_eq!(asked.load(Ordering::SeqCst), 1);

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let asked2 = Arc::new(AtomicUsize::new(0));
        let (_ark, o) = open(&s, asked2.clone());
        let done = tokio::time::timeout(Duration::from_secs(30), o.wait(&id))
            .await
            .expect("the task finished after the restart")
            .unwrap();
        assert_eq!(done.state, TaskState::Done, "{:?}", done.error);
        assert_eq!(asked2.load(Ordering::SeqCst), 0, "not asked again");
        assert_eq!(
            done.result.as_deref(),
            Some("Wrote the report."),
            "replayed history is not the answer"
        );
        let reports = done
            .outputs
            .iter()
            .filter(|a| a.name == "report.md")
            .count();
        assert_eq!(reports, 1, "{:?}", done.outputs);
    });
    let log = calls(&s);
    assert!(log.contains("session/load"), "{log}");
}
